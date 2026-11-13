//! OAuth authorization, device grants and persistent token refresh.
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(Clone)]
pub struct Config {
    pub client_id: String,
    pub client_secret: Option<String>,
    pub authorize_url: String,
    pub token_url: String,
    pub device_url: Option<String>,
    pub scope: String,
    pub pkce: bool,
    pub json: bool,
    pub refresh_skew_seconds: i64,
}

/// Non-empty env var as `&'static str` (configs are built rarely; leak is bounded).
// ponytail: leaks one small string per lookup; cache in a OnceLock if provider() gets hot.
fn env_leak(key: &str) -> Option<&'static str> {
    std::env::var(key).ok().filter(|v| !v.is_empty()).map(|v| &*Box::leak(v.into_boxed_str()))
}

pub fn provider(id: &str) -> Option<Config> {
    let (client, secret, auth, token, device, scope, pkce, json, skew) = match id {
        "claude" => (
            "9d1c250a-e61b-44d9-88ed-5944d1962f5e",
            None,
            "https://claude.ai/oauth/authorize",
            "https://api.anthropic.com/v1/oauth/token",
            None,
            "org:create_api_key user:profile user:inference",
            true,
            true,
            14400,
        ),
        "codex" | "openai" => (
            "app_EMoamEEZ73f0CkXaXp7hrann",
            None,
            "https://auth.openai.com/oauth/authorize",
            "https://auth.openai.com/oauth/token",
            None,
            "openid profile email offline_access",
            true,
            false,
            600,
        ),
        "github" => (
            "Iv1.b507a08c87ecfe98",
            None,
            "https://github.com/login/oauth/authorize",
            "https://github.com/login/oauth/access_token",
            Some("https://github.com/login/device/code"),
            "read:user",
            false,
            false,
            300,
        ),
        "kimi" | "kimi-coding" => (
            "17e5f671-d194-4dfb-9706-5516cb48c098",
            None,
            "",
            "https://auth.kimi.com/api/oauth/token",
            Some("https://auth.kimi.com/api/oauth/device_authorization"),
            "",
            false,
            false,
            300,
        ),
        "grok-cli" | "xai" => (
            "b1a00492-073a-47ea-816f-4c329264a828",
            None,
            "https://auth.x.ai/oauth2/authorize",
            "https://auth.x.ai/oauth2/token",
            Some("https://auth.x.ai/oauth2/device/code"),
            "openid profile email offline_access grok-cli:access api:access conversations:read conversations:write",
            true,
            false,
            300,
        ),
        "muse" => (
            "1031625952748946",
            None,
            "",
            "https://auth.meta.com/oidc/device/token/",
            Some("https://auth.meta.com/oidc/device/authorization/"),
            "",
            false,
            false,
            300,
        ),
        "nous" => (
            "hermes-cli",
            None,
            "",
            "https://portal.nousresearch.com/api/oauth/token",
            Some("https://portal.nousresearch.com/api/oauth/device/code"),
            "inference:invoke inference:mint_agent_key",
            false,
            false,
            300,
        ),
        "iflow" => (
            "10009311001",
            Some(env_leak("IFLOW_OAUTH_CLIENT_SECRET")?),
            "https://iflow.cn/oauth",
            "https://iflow.cn/oauth/token",
            None,
            "",
            false,
            false,
            86400,
        ),
        "gemini-cli" | "antigravity" => {
            // Credentials come from env so they never land in the public repo.
            let p = if id == "gemini-cli" { "GEMINI_CLI" } else { "ANTIGRAVITY" };
            let (c, s) = (
                env_leak(&format!("{p}_OAUTH_CLIENT_ID"))?,
                env_leak(&format!("{p}_OAUTH_CLIENT_SECRET"))?,
            );
            (
                c,
                Some(s),
                "https://accounts.google.com/o/oauth2/v2/auth",
                "https://oauth2.googleapis.com/token",
                None,
                "https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile",
                false,
                false,
                300,
            )
        }
        _ => return None,
    };
    Some(Config {
        client_id: client.into(),
        client_secret: secret.map(str::to_owned),
        authorize_url: auth.into(),
        token_url: token.into(),
        device_url: device.map(str::to_owned),
        scope: scope.into(),
        pkce,
        json,
        refresh_skew_seconds: skew,
    })
}

/// Caller generates a cryptographically random state and S256 challenge, retains verifier securely,
/// and verifies the returned state before exchanging the authorization code.
pub fn authorization_url(
    config: &Config,
    redirect_uri: &str,
    state: &str,
    challenge: Option<&str>,
) -> Result<String> {
    ensure!(!state.is_empty(), "OAuth state is required");
    let mut url =
        reqwest::Url::parse(&config.authorize_url).context("invalid authorization URL")?;
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("response_type", "code")
            .append_pair("client_id", &config.client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("state", state);
        if !config.scope.is_empty() {
            q.append_pair("scope", &config.scope);
        }
        if config.pkce {
            q.append_pair(
                "code_challenge",
                challenge
                    .filter(|s| !s.is_empty())
                    .context("PKCE challenge required")?,
            )
            .append_pair("code_challenge_method", "S256");
        }
        if config.client_id == "9d1c250a-e61b-44d9-88ed-5944d1962f5e" {
            q.append_pair("code", "true");
        }
        if config.client_id == "app_EMoamEEZ73f0CkXaXp7hrann" {
            q.append_pair("id_token_add_organizations", "true")
                .append_pair("codex_cli_simplified_flow", "true")
                .append_pair("originator", "codex_cli_rs");
        }
    }
    Ok(url.into())
}

#[derive(Deserialize, Serialize)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: Option<i64>,
    pub token_type: Option<String>,
    pub scope: Option<String>,
    pub id_token: Option<String>,
}

async fn post(config: &Config, url: &str, mut fields: Vec<(&str, &str)>) -> Result<Value> {
    let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build()?;
    post_with_client(config, url, fields, &client).await
}

async fn post_with_client(config: &Config, url: &str, mut fields: Vec<(&str, &str)>, client: &reqwest::Client) -> Result<Value> {
    fields.push(("client_id", &config.client_id));
    if let Some(secret) = &config.client_secret {
        fields.push(("client_secret", secret));
    }
    // Never include upstream response bodies or request URLs in errors: they can contain credentials.
    let request = client.post(url).header("Accept", "application/json");
    let request = if config.json {
        request
            .header("Content-Type", "application/json")
            .body(serde_json::to_vec(
                &fields
                    .iter()
                    .copied()
                    .collect::<std::collections::HashMap<_, _>>(),
            )?)
    } else {
        let mut encoded = reqwest::Url::parse("https://localhost/")?;
        encoded.query_pairs_mut().extend_pairs(fields);
        request
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(encoded.query().unwrap_or_default().to_owned())
    };
    let response = request
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("OAuth request failed"))?;
    let status = response.status();
    ensure!(status.is_success(), "OAuth endpoint returned HTTP {status}");
    let bytes = response
        .bytes()
        .await
        .map_err(|_| anyhow::anyhow!("invalid OAuth response"))?;
    serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("invalid OAuth response"))
}

fn tokens(value: Value) -> Result<Tokens> {
    if let Some(error) = value.get("error").and_then(Value::as_str) {
        // Provider-controlled error text may contain secrets.
        match error {
            "authorization_pending" => bail!("authorization_pending"),
            "slow_down" => bail!("slow_down"),
            "expired_token" => bail!("expired_token"),
            _ => bail!("OAuth grant rejected"),
        }
    }
    let t: Tokens = serde_json::from_value(value).context("invalid OAuth token response")?;
    ensure!(!t.access_token.is_empty(), "empty OAuth access token");
    Ok(t)
}

pub async fn exchange_code(
    config: &Config,
    code: &str,
    redirect_uri: &str,
    verifier: Option<&str>,
    state: &str,
) -> Result<Tokens> {
    let mut fields = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
    ];
    if config.pkce {
        fields.push((
            "code_verifier",
            verifier
                .filter(|v| !v.is_empty())
                .context("PKCE verifier required")?,
        ));
    }
    if config.json {
        fields.push(("state", state));
    }
    tokens(post(config, &config.token_url, fields).await?)
}

pub async fn device_code(config: &Config) -> Result<Value> {
    let url = config
        .device_url
        .as_deref()
        .context("device flow unsupported")?;
    let fields = if config.scope.is_empty() {
        vec![]
    } else {
        vec![("scope", config.scope.as_str())]
    };
    post(config, url, fields).await
}
pub async fn poll_device(config: &Config, device_code: &str) -> Result<Tokens> {
    tokens(
        post(
            config,
            &config.token_url,
            vec![
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", device_code),
            ],
        )
        .await?,
    )
}
pub async fn refresh_token(config: &Config, refresh: &str) -> Result<Tokens> {
    ensure!(!refresh.is_empty(), "refresh token required");
    tokens(
        post(
            config,
            &config.token_url,
            vec![("grant_type", "refresh_token"), ("refresh_token", refresh)],
        )
        .await?,
    )
}

pub async fn refresh_token_with_client(config: &Config, refresh: &str, client: &reqwest::Client) -> Result<Tokens> {
    ensure!(!refresh.is_empty(), "refresh token required");
    tokens(post_with_client(config, &config.token_url, vec![("grant_type", "refresh_token"), ("refresh_token", refresh)], client).await?)
}

/// Connection data remains compatible with the upstream providerConnections JSON column.
#[derive(Clone)]
pub struct Connection {
    pub id: String,
    pub provider: String,
    pub data: Value,
    pub db_path: PathBuf,
}

pub fn load(db_path: PathBuf, id: &str) -> Result<Option<Connection>> {
    let db = crate::db::open(&db_path)?;
    let row: Option<(String, String)> = db
        .query_row(
            "SELECT provider, data FROM providerConnections WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    row.map(|(provider, data)| {
        Ok(Connection {
            id: id.into(),
            provider,
            data: crate::crypto::parse_data(&data).context("invalid connection data")?,
            db_path,
        })
    })
    .transpose()
}

pub async fn refresh_if_needed(conn: Connection) -> Result<Connection> {
    let config = provider(&conn.provider).context("unsupported OAuth provider")?;
    refresh_with_config(conn, &config).await
}

async fn refresh_with_config(mut conn: Connection, config: &Config) -> Result<Connection> {
    static REFRESH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _guard = REFRESH_LOCK.lock().await;
    // Re-read after locking so concurrent requests cannot reuse a rotated refresh token.
    conn = load(conn.db_path.clone(), &conn.id)?.context("connection not found")?;
    let db = crate::db::open(&conn.db_path)?;
    let expires = conn.data.get("expiresAt").and_then(Value::as_str);
    let Some(expires) = expires else {
        return Ok(conn);
    };
    let remaining: Option<i64> = db.query_row(
        "SELECT CAST((julianday(?1)-julianday('now'))*86400 AS INTEGER)",
        [expires],
        |r| r.get(0),
    )?;
    if remaining.is_some_and(|n| n > config.refresh_skew_seconds) {
        return Ok(conn);
    }
    let refresh = conn
        .data
        .get("refreshToken")
        .and_then(Value::as_str)
        .context("refresh token missing")?;
    let old_refresh = refresh.to_owned();
    drop(db);
    let new = refresh_token(config, &old_refresh).await?;
    let db = crate::db::open(&conn.db_path)?;
    // Compare-and-swap prevents overwriting changes made while the request was in flight.
    // Compare in Rust: the stored refresh token may be an encrypted envelope.
    let current = load(conn.db_path.clone(), &conn.id)?.context("connection not found")?;
    ensure!(
        current.data["refreshToken"].as_str() == Some(old_refresh.as_str()),
        "connection changed during OAuth refresh"
    );
    let mut data = current.data;
    data["accessToken"] = json!(new.access_token);
    data["refreshToken"] = json!(new.refresh_token.as_deref().unwrap_or(&old_refresh));
    data["expiresAt"] = match new.expires_in {
        Some(s) => json!(db.query_row(
            "SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now', printf('+%d seconds', ?1))",
            [s],
            |r| r.get::<_, String>(0),
        )?),
        None => Value::Null,
    };
    conn.data = data.clone();
    crate::crypto::encrypt_fields(&mut data);
    db.execute(
        "UPDATE providerConnections SET data=?1, updatedAt=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?2",
        params![data.to_string(), conn.id],
    )?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, routing::post as route_post};
    use serde_json::json;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[tokio::test]
    async fn refresh_persists_and_deduplicates() {
        let count = Arc::new(AtomicUsize::new(0));
        let observed = count.clone();
        let app = Router::new().route(
            "/token",
            route_post(move |body: String| async move {
                observed.fetch_add(1, Ordering::SeqCst);
                assert!(body.contains("grant_type=refresh_token"));
                assert!(body.contains("refresh_token=old"));
                (
                    [("content-type", "application/json")],
                    r#"{"access_token":"new","refresh_token":"rotated","expires_in":3600}"#,
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/token", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let path = std::env::temp_dir().join(format!(
            "ninerouter-oauth-{}-{}.sqlite",
            std::process::id(),
            listener_id()
        ));
        let db = crate::db::open(&path).unwrap();
        let data = json!({"accessToken":"old-access","refreshToken":"old","expiresAt":"2000-01-01T00:00:00Z","other":"preserved"}).to_string();
        db.execute("INSERT INTO providerConnections(id,provider,authType,data,createdAt,updatedAt) VALUES('a','codex','oauth',?1,'now','now')", [&data]).unwrap();
        drop(db);
        let conn = load(path.clone(), "a").unwrap().unwrap();
        let mut config = provider("codex").unwrap();
        config.token_url = url;
        let (a, b) = tokio::join!(
            refresh_with_config(conn.clone(), &config),
            refresh_with_config(conn, &config)
        );
        assert_eq!(a.unwrap().data["accessToken"], "new");
        assert_eq!(b.unwrap().data["refreshToken"], "rotated");
        let saved = load(path.clone(), "a").unwrap().unwrap();
        assert_eq!(saved.data["other"], "preserved");
        assert_eq!(count.load(Ordering::SeqCst), 1);
        server.abort();
        let _ = std::fs::remove_file(path);
    }

    fn listener_id() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }

    #[test]
    fn authorization_requires_pkce_and_state() {
        let c = provider("codex").unwrap();
        assert!(authorization_url(&c, "http://localhost", "", Some("challenge")).is_err());
        assert!(authorization_url(&c, "http://localhost", "state", None).is_err());
        assert!(
            authorization_url(&c, "http://localhost", "state", Some("challenge"))
                .unwrap()
                .contains("code_challenge_method=S256")
        );
        assert!(tokens(json!({"error":"authorization_pending"})).is_err());
    }
}
