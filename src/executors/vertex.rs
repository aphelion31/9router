use anyhow::{Context, bail};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

static TOKEN_CACHE: OnceLock<Mutex<HashMap<String, (String, Option<String>, u64)>>> =
    OnceLock::new();

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub(super) async fn discover_project(
    client: &reqwest::Client,
    access_token: &str,
    base_url: Option<&str>,
) -> Option<String> {
    let base = base_url
        .unwrap_or("https://cloudresourcemanager.googleapis.com")
        .trim_end_matches('/');
    let res = client
        .get(format!("{base}/v1/projects?filter=lifecycleState%3AACTIVE"))
        .bearer_auth(access_token)
        .send()
        .await
        .ok()?;
    if !res.status().is_success() {
        return None;
    }
    let body: Value = serde_json::from_slice(&res.bytes().await.ok()?).ok()?;
    body.get("projects")?
        .as_array()?
        .first()?
        .get("projectId")?
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

pub(super) async fn refresh(
    client: &reqwest::Client,
    key: Option<&str>,
) -> anyhow::Result<Option<(String, Option<String>)>> {
    let Some(key) = key.filter(|s| s.trim_start().starts_with('{')) else {
        return Ok(None);
    };
    let cache = TOKEN_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let now = now_secs();
    if let Ok(guard) = cache.lock()
        && let Some((tok, proj, exp)) = guard.get(key)
        && *exp > now + 60
    {
        return Ok(Some((tok.clone(), proj.clone())));
    }
    let credentials: Value =
        serde_json::from_str(key).context("invalid Vertex credentials JSON")?;
    let field = |name: &str| {
        credentials
            .get(name)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .with_context(|| format!("missing Vertex credential field {name}"))
    };
    let kind = field("type")?;
    let token_uri = "https://oauth2.googleapis.com/token";
    let mut form = reqwest::Url::parse("https://localhost/")?;
    match kind {
        "authorized_user" => {
            form.query_pairs_mut()
                .append_pair("grant_type", "refresh_token")
                .append_pair("refresh_token", field("refresh_token")?)
                .append_pair("client_id", field("client_id")?)
                .append_pair("client_secret", field("client_secret")?);
        }
        "service_account" => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs();
            let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
            let claims = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&serde_json::json!({"iss": field("client_email")?, "scope": "https://www.googleapis.com/auth/cloud-platform", "aud": token_uri, "iat": now, "exp": now + 3600}))?);
            let payload = format!("{header}.{claims}");
            let pem = field("private_key")?
                .lines()
                .filter(|line| !line.starts_with("-----"))
                .collect::<String>();
            let der = STANDARD.decode(pem).context("invalid Vertex private key")?;
            let key = ring::signature::RsaKeyPair::from_pkcs8(&der)
                .map_err(|_| anyhow::anyhow!("invalid Vertex RSA private key"))?;
            let mut signature = vec![0; key.public().modulus_len()];
            key.sign(
                &ring::signature::RSA_PKCS1_SHA256,
                &ring::rand::SystemRandom::new(),
                payload.as_bytes(),
                &mut signature,
            )
            .map_err(|_| anyhow::anyhow!("Vertex JWT signing failed"))?;
            let assertion = format!("{payload}.{}", URL_SAFE_NO_PAD.encode(signature));
            form.query_pairs_mut()
                .append_pair("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer")
                .append_pair("assertion", &assertion);
        }
        _ => bail!("unsupported Vertex credential type"),
    }
    let bytes = client
        .post(token_uri)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(form.query().unwrap_or_default().to_owned())
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    let response: Value =
        serde_json::from_slice(&bytes).context("invalid Vertex token response")?;
    let token = response
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .context("Vertex token response missing access_token")?
        .to_owned();
    let mut project = credentials
        .get(if kind == "service_account" {
            "project_id"
        } else {
            "quota_project_id"
        })
        .and_then(Value::as_str)
        .map(str::to_owned);
    if project.is_none() {
        project = discover_project(client, &token, None).await;
    }
    if let Ok(mut guard) = cache.lock() {
        guard.insert(key.to_string(), (token.clone(), project.clone(), now + 3000));
    }
    Ok(Some((token, project)))
}

pub(super) fn url(
    endpoint: &str,
    partner: bool,
    credential_project: Option<&str>,
    model: &str,
    stream: bool,
    api_key: Option<&str>,
    access_token: Option<&str>,
    settings: Option<&Value>,
) -> anyhow::Result<reqwest::Url> {
    let mut url = reqwest::Url::parse(endpoint).context("invalid Vertex endpoint")?;
    let project = credential_project
        .map(str::to_owned)
        .or_else(|| settings.and_then(|s| s.get("projectId")).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_owned))
        .or_else(|| std::env::var("GOOGLE_CLOUD_PROJECT").ok())
        .filter(|s| !s.is_empty());
    if (access_token.is_some() || partner) && project.is_none() {
        bail!("Vertex OAuth requires GOOGLE_CLOUD_PROJECT");
    }
    let location = settings.and_then(|s| s.get("location")).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_owned).or_else(|| std::env::var("GOOGLE_CLOUD_LOCATION").ok()).unwrap_or_else(|| {
        if access_token.is_some() {
            "us-central1"
        } else {
            "global"
        }
        .into()
    });
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| anyhow::anyhow!("invalid Vertex endpoint"))?;
        segments.pop_if_empty();
        segments.push("v1");
        if let Some(project) = project.as_deref() {
            segments.extend([
                "projects",
                project,
                "locations",
                if partner { "global" } else { &location },
            ]);
        }
        if partner {
            segments.extend(["endpoints", "openapi", "chat", "completions"]);
        } else {
            segments.extend([
                "publishers",
                "google",
                "models",
                &format!(
                    "{}:{}",
                    model,
                    if stream {
                        "streamGenerateContent"
                    } else {
                        "generateContent"
                    }
                ),
            ]);
        }
    }
    if stream && !partner {
        url.query_pairs_mut().append_pair("alt", "sse");
    }
    if access_token.is_none() {
        if let Some(key) = api_key {
            if key.trim_start().starts_with('{') {
                bail!("Vertex service account and ADC credentials require token refresh support");
            }
            url.query_pairs_mut().append_pair("key", key);
        }
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn invalid_credentials_fail_before_network() {
        let result = super::refresh(
            &reqwest::Client::new(),
            Some(r#"{"type":"authorized_user"}"#),
        )
        .await;
        assert!(result.unwrap_err().to_string().contains("refresh_token"));
    }

    #[test]
    fn partner_requires_project() {
        if std::env::var("GOOGLE_CLOUD_PROJECT").is_err() {
            assert!(
                super::url(
                    "http://localhost",
                    true,
                    None,
                    "model",
                    false,
                    Some("key"),
                    None,
                    None,
                )
                .is_err()
            );
        }
    }

    #[test]
    fn connection_project_and_location() {
        let settings = serde_json::json!({"projectId": "my-project", "location": "europe-west1"});
        let url = super::url("https://example.com", false, None, "model", true, Some("key"), None, Some(&settings)).unwrap();
        assert!(url.as_str().contains("/projects/my-project/locations/europe-west1/"));
        assert!(url.as_str().contains("alt=sse&key=key"));
    }
}
