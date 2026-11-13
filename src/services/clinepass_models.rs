use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const MODELS_URL: &str = "https://api.cline.bot/api/v1/models";
const FREE_URL: &str = "https://api.cline.bot/api/v1/ai/cline/recommended-models";
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
const CACHE_TTL: Duration = Duration::from_secs(5 * 60);

type CacheMap = HashMap<(u64, bool), (Instant, Vec<Value>)>;

fn cache() -> &'static Mutex<CacheMap> {
    static CACHE: OnceLock<Mutex<CacheMap>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn normalize(list: &[Value], pass_only: bool) -> Vec<Value> {
    list.iter().filter_map(|item| {
        let id = item["id"].as_str()?;
        if if pass_only { !id.starts_with("cline-pass/") } else { id.trim().is_empty() } { return None; }
        Some(json!({"id":id,"name":item["name"].as_str().filter(|s| !s.is_empty()).unwrap_or(id)}))
    }).collect()
}

pub fn cline_models_from_payload(
    raw: &Value,
    free: Option<&Value>,
    pass_only: bool,
) -> Option<Vec<Value>> {
    let list = raw.as_array().or_else(|| raw["data"].as_array())?;
    let mut result = normalize(list, pass_only);
    if !pass_only {
        let mut seen: HashSet<String> = result
            .iter()
            .filter_map(|v| v["id"].as_str().map(str::to_owned))
            .collect();
        if let Some(list) = free.and_then(|v| v["free"].as_array()) {
            for model in normalize(list, false) {
                if seen.insert(model["id"].as_str().unwrap().to_owned()) {
                    result.push(model);
                }
            }
        }
    }
    (!result.is_empty()).then_some(result)
}

/// Mirrors `getClineAccessToken`: OAuth JWTs get the `workos:` prefix, API keys pass verbatim.
fn is_workos_jwt(token: &str) -> bool {
    let mut parts = token.split('.');
    matches!(
        (parts.next(), parts.next()),
        (Some(h), Some(p)) if h.len() > 3 && h.starts_with("eyJ") && !p.is_empty()
    )
}

fn auth_token(token: &str, api_key: bool) -> String {
    if !api_key && is_workos_jwt(token) && !token.to_ascii_lowercase().starts_with("workos:") {
        format!("workos:{token}")
    } else {
        token.to_owned()
    }
}

fn token_hash(token: &str) -> u64 {
    let mut h = DefaultHasher::new();
    token.hash(&mut h);
    h.finish()
}

pub async fn resolve_cline_models(
    client: &reqwest::Client,
    token: &str,
    api_key: bool,
    pass_only: bool,
) -> Option<Vec<Value>> {
    resolve_cline_models_at(client, MODELS_URL, FREE_URL, token, api_key, pass_only).await
}

pub async fn resolve_cline_models_at(
    client: &reqwest::Client,
    models_url: &str,
    free_url: &str,
    token: &str,
    api_key: bool,
    pass_only: bool,
) -> Option<Vec<Value>> {
    let token = token.trim();
    if token.is_empty() {
        return None;
    }
    let key = (token_hash(token), pass_only);
    if let Some((at, models)) = cache().lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        if at.elapsed() < CACHE_TTL {
            return Some(models.clone());
        }
    }

    let mut request = client
        .get(models_url)
        .timeout(FETCH_TIMEOUT)
        .header("Accept", "application/json")
        .bearer_auth(auth_token(token, api_key));
    if !api_key {
        request = request
            .header("HTTP-Referer", "https://cline.bot")
            .header("X-Title", "Cline")
            .header("User-Agent", format!("9Router/{}", env!("CARGO_PKG_VERSION")))
            .header("X-PLATFORM", std::env::consts::OS)
            .header("X-CLIENT-TYPE", "9router")
            .header("X-CLIENT-VERSION", env!("CARGO_PKG_VERSION"))
            .header("X-CORE-VERSION", env!("CARGO_PKG_VERSION"))
            .header("X-IS-MULTIROOT", "false");
    }
    let raw = request
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .text()
        .await
        .ok()?;
    let raw: Value = serde_json::from_str(&raw).ok()?;
    // Free tier is additive: a dead feed must not take the catalog down.
    let free = if pass_only {
        None
    } else {
        match client
            .get(free_url)
            .timeout(FETCH_TIMEOUT)
            .header("Accept", "application/json")
            .send()
            .await
        {
            Ok(response) => match response.error_for_status() {
                Ok(response) => match response.text().await {
                    Ok(text) => serde_json::from_str::<Value>(&text).ok(),
                    Err(_) => None,
                },
                Err(_) => None,
            },
            Err(_) => None,
        }
    };
    let models = cline_models_from_payload(&raw, free.as_ref(), pass_only)?;
    cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, (Instant::now(), models.clone()));
    Some(models)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn merges_free_models_first_writer_wins() {
        let raw = json!({"data":[{"id":"a","name":"Original"},{"id":"cline-pass/b"}]});
        let free = json!({"free":[{"id":"a","name":"Duplicate"},{"id":"free"}]});
        let all = cline_models_from_payload(&raw, Some(&free), false).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0]["name"], "Original");
        assert_eq!(
            cline_models_from_payload(&raw, None, true).unwrap().len(),
            1
        );
    }

    #[test]
    fn workos_prefix_only_for_oauth_jwt() {
        assert_eq!(auth_token("eyJhbGc.payload", false), "workos:eyJhbGc.payload");
        assert_eq!(auth_token("eyJhbGc.payload", true), "eyJhbGc.payload");
        assert_eq!(auth_token("clp_key", false), "clp_key");
        assert_eq!(auth_token("workos:eyJx.y", false), "workos:eyJx.y");
    }

    async fn mock(bodies: Vec<&'static str>) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            let mut requests = Vec::new();
            for body in bodies {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = vec![0u8; 8192];
                let n = sock.read(&mut buf).await.unwrap();
                requests.push(String::from_utf8_lossy(&buf[..n]).into_owned());
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                sock.write_all(resp.as_bytes()).await.unwrap();
            }
            requests
        });
        (base, handle)
    }

    #[tokio::test]
    async fn fetches_merges_sends_headers_and_caches() {
        let (base, handle) = mock(vec![
            r#"{"data":[{"id":"cline-pass/x","name":"X"}]}"#,
            r#"{"free":[{"id":"free/one"}]}"#,
        ])
        .await;
        let client = reqwest::Client::new();
        let token = "eyJcachetest.part2";
        let models_url = format!("{base}/api/v1/models");
        let free_url = format!("{base}/free");
        let first = resolve_cline_models_at(&client, &models_url, &free_url, token, false, false)
            .await
            .unwrap();
        assert_eq!(first.len(), 2);
        assert_eq!(first[1]["id"], "free/one");

        let requests = handle.await.unwrap();
        let lower = requests[0].to_ascii_lowercase();
        assert!(requests[0].contains("authorization: Bearer workos:eyJcachetest.part2"));
        assert!(lower.contains("x-client-type: 9router"));
        assert!(lower.contains("user-agent: 9router/"));
        assert!(!requests[1].to_ascii_lowercase().contains("authorization:"));

        // Listener is gone; a second call can only succeed from cache.
        let cached = resolve_cline_models_at(&client, &models_url, &free_url, token, false, false)
            .await
            .unwrap();
        assert_eq!(cached, first);
    }
}
