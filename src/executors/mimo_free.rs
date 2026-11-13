use anyhow::{Context, bail};
use serde_json::{Value, json};

const MARKER: &str =
    "You are MiMoCode, an interactive CLI tool that helps users with software engineering tasks.";
const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

pub(super) async fn execute(
    client: &reqwest::Client,
    endpoint: &str,
    body: &mut Value,
    stream: bool,
) -> anyhow::Result<reqwest::Response> {
    let client_id =
        std::env::var("MIMO_FREE_CLIENT_ID").context("MIMO_FREE_CLIENT_ID must be configured")?;
    let bootstrap = std::env::var("MIMO_FREE_BOOTSTRAP_URL")
        .unwrap_or_else(|_| "https://api.xiaomimimo.com/api/free-ai/bootstrap".into());
    execute_with(client, endpoint, body, stream, &client_id, &bootstrap).await
}

async fn execute_with(
    client: &reqwest::Client,
    endpoint: &str,
    body: &mut Value,
    stream: bool,
    client_id: &str,
    bootstrap: &str,
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
    for attempt in 0..2 {
        let response = client
            .post(bootstrap)
            .header("user-agent", UA)
            .header("content-type", "application/json")
            .body(json!({"client":client_id}).to_string())
            .send()
            .await?
            .error_for_status()?;
        let value: Value = serde_json::from_slice(&response.bytes().await?)?;
        let token = value
            .get("jwt")
            .and_then(Value::as_str)
            .context("MiMo bootstrap returned no JWT")?;
        let response = client
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
            .bearer_auth(token)
            .body(serde_json::to_vec(body)?)
            .send()
            .await?;
        if attempt == 1 || !matches!(response.status().as_u16(), 401 | 403) {
            return Ok(response);
        }
    }
    bail!("MiMo authentication failed")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, body::Bytes, http::HeaderMap, routing::post};
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
        )
        .await
        .unwrap();
        assert!(response.status().is_success());
        task.abort();
    }
}
