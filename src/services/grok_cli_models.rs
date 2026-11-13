use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const MODELS_URL: &str = "https://cli-chat-proxy.grok.com/v1/models";
const GROK_CLI_VERSION: &str = "1.0.44";
const GROK_CLI_USER_AGENT: &str = "grok-shell/1.0.44 (linux; x86_64)";
const FETCH_TIMEOUT: Duration = Duration::from_secs(20);
const CACHE_TTL: Duration = Duration::from_secs(5 * 60);

fn cache() -> &'static Mutex<HashMap<u64, (Instant, Vec<Value>)>> {
    static CACHE: OnceLock<Mutex<HashMap<u64, (Instant, Vec<Value>)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn clear_grok_cli_model_cache() {
    cache().lock().unwrap_or_else(|e| e.into_inner()).clear();
}

/// Live Grok CLI discovery. On 401/403 retries once with `refresh` (a freshly issued access token).
/// Returns `None` on failure so callers keep the static catalog. Cached per token for 5 minutes.
pub async fn resolve_grok_cli_models(
    client: &reqwest::Client,
    token: &str,
    refresh: Option<&str>,
    email: Option<&str>,
    user_id: Option<&str>,
) -> Option<Vec<Value>> {
    resolve_grok_cli_models_at(client, MODELS_URL, token, refresh, email, user_id).await
}

pub async fn resolve_grok_cli_models_at(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    refresh: Option<&str>,
    email: Option<&str>,
    user_id: Option<&str>,
) -> Option<Vec<Value>> {
    let token = token.trim();
    if token.is_empty() {
        return None;
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(token, &mut hasher);
    let key = std::hash::Hasher::finish(&hasher);
    if let Some((at, models)) = cache().lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        if at.elapsed() < CACHE_TTL {
            return Some(models.clone());
        }
    }

    let (mut status, mut body) = fetch(client, url, token, email, user_id).await?;
    if status == 401 || status == 403 {
        let fresh = refresh?.trim();
        if fresh.is_empty() {
            return None;
        }
        (status, body) = fetch(client, url, fresh, email, user_id).await?;
    }
    if !(200..300).contains(&status) {
        return None;
    }
    let raw: Value = serde_json::from_str(&body).ok()?;
    let models = parse_grok_cli_models(&raw);
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
    email: Option<&str>,
    user_id: Option<&str>,
) -> Option<(u16, String)> {
    let mut request = client
        .get(url)
        .timeout(FETCH_TIMEOUT)
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json")
        .header("User-Agent", GROK_CLI_USER_AGENT)
        .header("x-xai-token-auth", "xai-grok-cli")
        .header("x-grok-client-version", GROK_CLI_VERSION)
        .header("x-grok-client-identifier", "grok-shell")
        .header("x-grok-client-mode", "headless");
    if let Some(email) = email.filter(|s| !s.is_empty()) {
        request = request.header("x-email", email);
    }
    if let Some(id) = user_id.filter(|s| !s.is_empty()) {
        request = request.header("x-userid", id);
    }
    let response = request.send().await.ok()?;
    let status = response.status().as_u16();
    Some((status, response.text().await.ok()?))
}

pub fn parse_grok_cli_models(data: &Value) -> Vec<Value> {
    let entries = if data.is_array() {
        data
    } else {
        data.get("data")
            .or_else(|| data.get("models"))
            .or_else(|| data.get("results"))
            .unwrap_or(&Value::Null)
    };
    let pairs: Vec<(Option<&str>, &Value)> = match entries {
        Value::Array(items) => items.iter().map(|item| (None, item)).collect(),
        Value::Object(items) => items
            .iter()
            .map(|(key, item)| (Some(key.as_str()), item))
            .collect(),
        _ => Vec::new(),
    };
    let mut seen = HashSet::new();
    let mut models = Vec::new();
    for (key, raw) in pairs {
        let mut item = match raw {
            Value::String(id) => Map::from_iter([("id".into(), Value::String(id.clone()))]),
            Value::Object(item) => item.clone(),
            _ => continue,
        };
        let id = ["id", "model_id", "modelId", "model", "slug"]
            .iter()
            .find_map(|k| item.get(*k).filter(|v| !v.is_null()))
            .and_then(Value::as_str)
            .or(key)
            .or_else(|| item.get("name").and_then(Value::as_str))
            .unwrap_or("")
            .trim()
            .to_owned();
        if id.is_empty() || !seen.insert(id.clone()) {
            continue;
        }
        let name = ["display_name", "displayName", "name"]
            .iter()
            .find_map(|k| item.get(*k).filter(|v| !v.is_null()))
            .cloned()
            .unwrap_or_else(|| id.clone().into());
        for (target, keys) in [
            (
                "contextLength",
                [
                    "context_length",
                    "contextLength",
                    "context_window",
                    "contextWindow",
                ],
            ),
            (
                "maxOutputTokens",
                ["max_output_tokens", "maxOutputTokens", "", ""],
            ),
        ] {
            let number = keys
                .iter()
                .find_map(|key| item.get(*key).filter(|v| !v.is_null()))
                .and_then(|v| {
                    v.as_f64()
                        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                })
                .filter(|v: &f64| v.is_finite() && *v > 0.0);
            if let Some(number) = number {
                if let Some(n) = serde_json::Number::from_f64(number) {
                    item.insert(target.into(), n.into());
                }
            }
        }
        if id == "grok-build" {
            if !item
                .get("contextLength")
                .and_then(Value::as_f64)
                .is_some_and(|n| n > 0.0)
            {
                item.insert("contextLength".into(), 500000.into());
            }
            if !item
                .get("maxOutputTokens")
                .and_then(Value::as_f64)
                .is_some_and(|n| n > 0.0)
            {
                item.insert("maxOutputTokens".into(), 64000.into());
            }
        }
        item.insert("id".into(), id.into());
        item.insert("name".into(), name);
        models.push(item.into());
    }
    models
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn mock(responses: Vec<(&'static str, &'static str)>) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/models", listener.local_addr().unwrap());
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
    async fn live_discovery_refreshes_on_401_and_caches() {
        let body = r#"{"data":[{"id":"grok-build","context_window":1}]}"#;
        let (url, handle) = mock(vec![("401 Unauthorized", "{}"), ("200 OK", body)]).await;
        let client = reqwest::Client::new();
        let models = resolve_grok_cli_models_at(&client, &url, "stale", Some("fresh"), Some("a@b.c"), Some("u1"))
            .await
            .unwrap();
        assert_eq!(models[0]["contextLength"].as_f64(), Some(1.0));
        let seen = handle.await.unwrap();
        assert!(seen[0].contains("Bearer stale"));
        assert!(seen[1].contains("Bearer fresh"));
        assert!(seen[1].to_ascii_lowercase().contains("x-email: a@b.c"));
        assert!(seen[1].contains("grok-shell/1.0.44"));
        let again = resolve_grok_cli_models_at(&client, &url, "stale", None, None, None).await;
        assert_eq!(again.unwrap(), models);
    }
    #[test]
    fn normalizes_official_metadata_and_deduplicates() {
        let models = parse_grok_cli_models(
            &json!({"models":[{"model_id":"grok-build","display_name":"Grok Build","context_window":500000,"max_output_tokens":64000,"supported_in_api":false},{"id":"grok-build"}]}),
        );
        assert_eq!(models.len(), 1);
        assert_eq!(models[0]["id"], "grok-build");
        assert_eq!(models[0]["name"], "Grok Build");
        assert_eq!(models[0]["contextLength"].as_f64(), Some(500000.0));
        assert_eq!(models[0]["maxOutputTokens"].as_f64(), Some(64000.0));
        assert_eq!(models[0]["supported_in_api"], false);
        assert_eq!(
            parse_grok_cli_models(&json!(["grok-build"]))[0]["contextLength"],
            500000
        );
    }
}
