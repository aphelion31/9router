use anyhow::{Context, bail};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

const MARKER: &str =
    "You are MiMoCode, an interactive CLI tool that helps users with software engineering tasks.";
const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

static JWT_CACHE: OnceLock<Mutex<HashMap<String, (String, u64)>>> = OnceLock::new();

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn get_cached_jwt(key: &str) -> Option<String> {
    let cache = JWT_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = cache.lock().ok()?;
    let now = now_secs();
    if let Some((tok, exp)) = guard.get(key) {
        if *exp > now + 60 {
            return Some(tok.clone());
        }
        guard.remove(key);
    }
    None
}

fn set_cached_jwt(key: String, tok: String) {
    let cache = JWT_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut guard) = cache.lock() {
        guard.insert(key, (tok, now_secs() + 3000));
    }
}

fn clear_cached_jwt(key: &str) {
    let cache = JWT_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut guard) = cache.lock() {
        guard.remove(key);
    }
}

pub(super) async fn execute(
    client: &reqwest::Client,
    endpoint: &str,
    body: &mut Value,
    stream: bool,
    proxy: Option<&Value>,
) -> anyhow::Result<reqwest::Response> {
    let client_id =
        std::env::var("MIMO_FREE_CLIENT_ID").context("MIMO_FREE_CLIENT_ID must be configured")?;
    let bootstrap = std::env::var("MIMO_FREE_BOOTSTRAP_URL")
        .unwrap_or_else(|_| "https://api.xiaomimimo.com/api/free-ai/bootstrap".into());
    execute_with(client, endpoint, body, stream, &client_id, &bootstrap, proxy).await
}

async fn execute_with(
    client: &reqwest::Client,
    endpoint: &str,
    body: &mut Value,
    stream: bool,
    client_id: &str,
    bootstrap: &str,
    proxy: Option<&Value>,
) -> anyhow::Result<reqwest::Response> {
    if let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) {
        if !messages.iter().any(|m| {
            m.get("role").and_then(Value::as_str) == Some("system")
                && m.get("content")
                    .and_then(Value::as_str)
                    .is_some_and(|s| s.contains(MARKER))
        }) {
            messages.insert(0, json!({"role":"system","content":MARKER}));
        }
    }
    let cache_key = format!("{bootstrap}:{client_id}");
    for attempt in 0..2 {
        let token = if let Some(cached) = get_cached_jwt(&cache_key) {
            cached
        } else {
            let builder = client
                .post(bootstrap)
                .header("user-agent", UA)
                .header("content-type", "application/json")
                .body(json!({"client":client_id}).to_string());
            let response = super::send_request(client, builder.build()?, proxy).await?.error_for_status()?;
            let value: Value = serde_json::from_slice(&response.bytes().await?)?;
            let tok = value
                .get("jwt")
                .and_then(Value::as_str)
                .context("MiMo bootstrap returned no JWT")?
                .to_string();
            set_cached_jwt(cache_key.clone(), tok.clone());
            tok
        };
        let builder = client
            .post(endpoint)
            .header("user-agent", UA)
            .header("x-mimo-source", "mimocode-cli-free")
            .header("x-session-affinity", super::opencode::session())
            .header(
                "accept",
                if stream {
                    "text/event-stream"
                } else {
                    "application/json"
                },
            )
            .header("content-type", "application/json")
            .bearer_auth(&token)
            .body(serde_json::to_vec(body)?);
        let response = super::send_request(client, builder.build()?, proxy).await?;
        if attempt == 1 || !matches!(response.status().as_u16(), 401 | 403) {
            return Ok(response);
        }
        clear_cached_jwt(&cache_key);
    }
    bail!("MiMo authentication failed")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, body::Bytes, http::HeaderMap, routing::post};
    #[tokio::test]
    async fn mimo_bootstrap_and_chat_use_relay() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let relay = format!("http://{}/relay", listener.local_addr().unwrap());
        let app = Router::new().route("/relay", post(|headers: HeaderMap| async move {
            assert_eq!(headers["x-relay-target"], "https://example.com");
            if headers["x-relay-path"] == "/bootstrap" { json!({"jwt":"relay-token"}).to_string() }
            else { assert_eq!(headers["x-relay-path"], "/chat"); assert_eq!(headers["authorization"], "Bearer relay-token"); "ok".to_string() }
        }));
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let proxy = json!({"vercelRelayUrl":relay});
        let response = execute_with(&reqwest::Client::new(), "https://example.com/chat", &mut json!({"messages":[]}), true, "relay-test-client", "https://example.com/bootstrap", Some(&proxy)).await.unwrap();
        assert_eq!(response.text().await.unwrap(), "ok");
        task.abort();
    }
    #[tokio::test]
    async fn mimo_free_mock_request() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new()
            .route(
                "/bootstrap",
                post(|| async { json!({"jwt":"test"}).to_string() }),
            )
            .route(
                "/chat",
                post(|headers: HeaderMap, body: Bytes| async move {
                    assert_eq!(headers["authorization"], "Bearer test");
                    let v: Value = serde_json::from_slice(&body).unwrap();
                    assert_eq!(v["messages"][0]["content"], MARKER);
                    "ok"
                }),
            );
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let response = execute_with(
            &reqwest::Client::new(),
            &format!("{base}/chat"),
            &mut json!({"messages":[]}),
            true,
            "test-client",
            &format!("{base}/bootstrap"),
            None,
        )
        .await
        .unwrap();
        assert!(response.status().is_success());
        task.abort();
    }
}
