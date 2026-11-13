use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
const CACHE_TTL: Duration = Duration::from_secs(5 * 60);
const KIRO_VERSION: &str = "0.10.32";
const KIRO_SDK_VERSION: &str = "1.0.0";

fn cache() -> &'static Mutex<HashMap<String, (Instant, Vec<Value>)>> {
    static CACHE: OnceLock<Mutex<HashMap<String, (Instant, Vec<Value>)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Builds the `ListAvailableModels` URL for the account's profile region.
pub fn list_models_url(profile_arn: &str) -> String {
    let region = region_from_profile_arn(profile_arn);
    let mut url = reqwest::Url::parse(&format!("https://q.{region}.amazonaws.com/ListAvailableModels"))
        .expect("static host template");
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("origin", "AI_EDITOR");
        if !profile_arn.is_empty() {
            q.append_pair("profileArn", profile_arn);
        }
    }
    url.into()
}

/// Stable per-account machine fingerprint, same derivation as the JS (`sha256(seed)` hex).
fn machine_id(seed: &str) -> String {
    ring::digest::digest(&ring::digest::SHA256, seed.as_bytes())
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Fetches the live Kiro catalog, expanded into 9router variants.
/// On HTTP 401 and a `refresh` callback returning a new access token, retries once.
/// Returns `None` on any failure so callers fall back to the static catalog.
pub async fn resolve_kiro_models(
    client: &reqwest::Client,
    profile_arn: &str,
    access_token: &str,
    cache_seed: &str,
    refresh: Option<&str>,
) -> Option<Vec<Value>> {
    resolve_kiro_models_at(client, &list_models_url(profile_arn), access_token, cache_seed, refresh).await
}

pub async fn resolve_kiro_models_at(
    client: &reqwest::Client,
    url: &str,
    access_token: &str,
    cache_seed: &str,
    refresh: Option<&str>,
) -> Option<Vec<Value>> {
    let access_token = access_token.trim();
    if access_token.is_empty() {
        return None;
    }
    let key = machine_id(&format!("kiro:{cache_seed}"));
    if let Some((at, models)) = cache().lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        if at.elapsed() < CACHE_TTL {
            return Some(models.clone());
        }
    }

    let (mut status, mut body) = fetch(client, url, access_token, cache_seed).await?;
    if status == 401 {
        let fresh = refresh?.trim();
        if fresh.is_empty() {
            return None;
        }
        (status, body) = fetch(client, url, fresh, cache_seed).await?;
    }
    if !(200..300).contains(&status) {
        return None;
    }
    let raw: Value = serde_json::from_str(&body).ok()?;
    let models = expand_kiro_catalog(raw["models"].as_array()?);
    if models.is_empty() {
        return None;
    }
    cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, (Instant::now(), models.clone()));
    Some(models)
}

async fn fetch(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    seed: &str,
) -> Option<(u16, String)> {
    let machine = machine_id(seed);
    let user_agent = format!(
        "aws-sdk-js/{KIRO_SDK_VERSION} ua/2.1 os/windows#10.0.26200 lang/js md/nodejs#22.21.1 api/codewhispererruntime#{KIRO_SDK_VERSION} m/N,E KiroIDE-{KIRO_VERSION}-{machine}"
    );
    let response = client
        .get(url)
        .timeout(FETCH_TIMEOUT)
        .header("Accept", "application/json")
        .header("Authorization", format!("Bearer {token}"))
        .header("User-Agent", user_agent)
        .header("x-amz-user-agent", format!("aws-sdk-js/{KIRO_SDK_VERSION} KiroIDE-{KIRO_VERSION}-{machine}"))
        .header("x-amzn-kiro-agent-mode", "vibe")
        .header("x-amzn-codewhisperer-optout", "true")
        .header("amz-sdk-request", "attempt=1; max=1")
        .send()
        .await
        .ok()?;
    let status = response.status().as_u16();
    Some((status, response.text().await.ok()?))
}

pub fn region_from_profile_arn(arn: &str) -> &str {
    arn.split(':')
        .nth(3)
        .filter(|s| !s.is_empty())
        .unwrap_or("us-east-1")
}

pub fn expand_kiro_catalog(raw: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    for model in raw {
        let Some(id) = model["modelId"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| model["id"].as_str().filter(|s| !s.is_empty()))
        else {
            continue;
        };
        let safe = id.strip_suffix("-agentic").unwrap_or(id);
        let safe = safe.strip_suffix("-thinking").unwrap_or(safe);
        let base = model["modelName"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or(id)
            .trim();
        let rate = model["rateMultiplier"]
            .as_f64()
            .or_else(|| {
                model["rateMultiplier"]
                    .as_str()
                    .and_then(|s| s.parse::<f64>().ok())
            })
            .filter(|n| n.is_finite())
            .unwrap_or(1.0);
        let display = if rate > 0.0 && (rate - 1.0).abs() >= 1e-9 {
            format!("Kiro {base} ({rate:.1}x credit)")
        } else {
            format!("Kiro {base}")
        };
        let context = model["tokenLimits"]["maxInputTokens"]
            .as_f64()
            .or_else(|| {
                model["tokenLimits"]["maxInputTokens"]
                    .as_str()
                    .and_then(|s| s.parse().ok())
            })
            .filter(|n| *n != 0.0 && n.is_finite())
            .unwrap_or(200000.0);
        let mut variants = vec![
            ("", "", false, false),
            ("-thinking", " (Thinking)", true, false),
        ];
        if safe != "auto" {
            variants.extend([
                ("-agentic", " (Agentic)", false, true),
                ("-thinking-agentic", " (Thinking + Agentic)", true, true),
            ]);
        }
        for (suffix, label, thinking, agentic) in variants {
            out.push(json!({"id":format!("{safe}{suffix}"),"name":format!("{display}{label}"),"capabilities":{"thinking":thinking,"agentic":agentic},"contextLength":context,"rateMultiplier":rate,"upstreamModelId":id,"description":model["description"].as_str().unwrap_or("")}));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn list_url_carries_region_and_profile() {
        let url = list_models_url("arn:aws:codewhisperer:eu-west-1:123:profile/X");
        assert!(url.starts_with("https://q.eu-west-1.amazonaws.com/ListAvailableModels?origin=AI_EDITOR&profileArn="));
        assert!(url.contains("profileArn=arn%3Aaws%3Acodewhisperer"));
    }

    async fn mock(responses: Vec<(&'static str, &'static str)>) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/ListAvailableModels", listener.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            let mut seen = Vec::new();
            for (status, body) in responses {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = vec![0u8; 8192];
                let n = sock.read(&mut buf).await.unwrap();
                seen.push(String::from_utf8_lossy(&buf[..n]).into_owned());
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                sock.write_all(resp.as_bytes()).await.unwrap();
            }
            seen
        });
        (base, handle)
    }

    #[tokio::test]
    async fn refreshes_on_401_and_caches() {
        let body = r#"{"models":[{"modelId":"claude","modelName":"Claude"}]}"#;
        let (url, handle) = mock(vec![("401 Unauthorized", "{}"), ("200 OK", body)]).await;
        let client = reqwest::Client::new();
        let models = resolve_kiro_models_at(&client, &url, "stale", "seed-kiro-test", Some("fresh"))
            .await
            .unwrap();
        assert_eq!(models.len(), 4);
        let seen = handle.await.unwrap();
        assert!(seen[0].contains("Bearer stale"));
        assert!(seen[1].contains("Bearer fresh"));
        assert!(seen[1].contains("KiroIDE-0.10.32-"));
        let again = resolve_kiro_models_at(&client, &url, "stale", "seed-kiro-test", None).await;
        assert_eq!(again.unwrap(), models);
    }

    #[test]
    fn expands_variants_and_skips_auto_agentic() {
        let models = expand_kiro_catalog(&[
            json!({"modelId":"auto"}),
            json!({"modelId":"claude","modelName":"Claude","rateMultiplier":2}),
        ]);
        assert_eq!(models.len(), 6);
        assert_eq!(models[0]["id"], "auto");
        assert_eq!(models[2]["name"], "Kiro Claude (2.0x credit)");
        assert_eq!(models[5]["id"], "claude-thinking-agentic");
        assert_eq!(
            region_from_profile_arn("arn:aws:codewhisperer:eu-west-1:foo"),
            "eu-west-1"
        );
    }
}
