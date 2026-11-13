use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const MODELS_URL: &str = "https://api.githubcopilot.com/models";
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
const CACHE_TTL: Duration = Duration::from_secs(5 * 60);

fn cache() -> &'static Mutex<HashMap<u64, (Instant, Vec<Value>)>> {
    static CACHE: OnceLock<Mutex<HashMap<u64, (Instant, Vec<Value>)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn expand_copilot_catalog(raw: &[Value]) -> Vec<Value> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for model in raw {
        if model["capabilities"]["type"] != "chat"
            || (model.get("policy").is_some_and(|p| !p.is_null())
                && model["policy"]["state"] != "enabled")
        {
            continue;
        }
        let Some(id) = model["id"].as_str().filter(|s| !s.is_empty()) else {
            continue;
        };
        if seen.insert(id) {
            out.push(json!({"id":id,"name":model["name"].as_str().filter(|s| !s.is_empty()).unwrap_or(id)}));
        }
    }
    out
}

fn token_hash(token: &str) -> u64 {
    let mut h = DefaultHasher::new();
    token.hash(&mut h);
    h.finish()
}

/// Fetches the live catalog. On 401/403 the caller supplies a refreshed Copilot token
/// via `refresh`; the request is retried once with it. Results are cached per token for 5 minutes.
pub async fn resolve_copilot_models(
    client: &reqwest::Client,
    token: &str,
    refresh: Option<&str>,
) -> Option<Vec<Value>> {
    resolve_copilot_models_at(client, MODELS_URL, token, refresh).await
}

pub async fn resolve_copilot_models_at(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    refresh: Option<&str>,
) -> Option<Vec<Value>> {
    let token = token.trim();
    if token.is_empty() {
        return None;
    }
    let key = token_hash(token);
    if let Some((at, models)) = cache().lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        if at.elapsed() < CACHE_TTL {
            return Some(models.clone());
        }
    }

    let (mut status, mut body) = fetch(client, url, token).await?;
    if status == 401 || status == 403 {
        let fresh = refresh?.trim();
        if fresh.is_empty() {
            return None;
        }
        (status, body) = fetch(client, url, fresh).await?;
    }
    if !(200..300).contains(&status) {
        return None;
    }
    let raw: Value = serde_json::from_str(&body).ok()?;
    let models = expand_copilot_catalog(raw["data"].as_array()?);
    if models.is_empty() {
        return None;
    }
    cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(token_hash(token), (Instant::now(), models.clone()));
    Some(models)
}

fn headers(builder: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
    builder
        .timeout(FETCH_TIMEOUT)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .header("Copilot-Integration-Id", "vscode-chat")
        .header("editor-version", "vscode/1.110.0")
        .header("editor-plugin-version", "copilot-chat/0.38.0")
        .header("user-agent", "GitHubCopilotChat/0.38.0")
        .header("x-github-api-version", "2025-04-01")
}

async fn fetch(client: &reqwest::Client, url: &str, token: &str) -> Option<(u16, String)> {
    let response = headers(client.get(url), token).send().await.ok()?;
    let status = response.status().as_u16();
    Some((status, response.text().await.ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn selects_enabled_chat_models_and_deduplicates() {
        let models = expand_copilot_catalog(&[
            json!({"id":"ok","capabilities":{"type":"chat"}}),
            json!({"id":"ok","capabilities":{"type":"chat"}}),
            json!({"id":"disabled","capabilities":{"type":"chat"},"policy":{"state":"disabled"}}),
            json!({"id":"embed","capabilities":{"type":"embeddings"}}),
        ]);
        assert_eq!(models, vec![json!({"id":"ok","name":"ok"})]);
    }

    /// Serves one canned response per connection: (status line, body).
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
    async fn refreshes_on_401_then_caches() {
        let body = r#"{"data":[{"id":"gpt-x","name":"GPT X","capabilities":{"type":"chat"}},{"id":"emb","capabilities":{"type":"embeddings"}}]}"#;
        let (url, handle) = mock(vec![("401 Unauthorized", "{}"), ("200 OK", body)]).await;
        let client = reqwest::Client::new();
        let models = resolve_copilot_models_at(&client, &url, "stale", Some("fresh"))
            .await
            .unwrap();
        assert_eq!(models, vec![json!({"id":"gpt-x","name":"GPT X"})]);
        let seen = handle.await.unwrap();
        assert!(seen[0].contains("Bearer stale"));
        assert!(seen[1].contains("Bearer fresh"));
        assert!(seen[1].to_ascii_lowercase().contains("copilot-integration-id: vscode-chat"));

        // Server is done; the original token key must be served from cache.
        let again = resolve_copilot_models_at(&client, &url, "stale", None).await;
        assert_eq!(again.unwrap(), models);
    }
}
