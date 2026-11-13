use anyhow::{Context, bail};
use serde_json::{Value, json};

pub(super) async fn execute(
    client: &reqwest::Client,
    endpoint: &str,
    model: &str,
    body: &Value,
    token: Option<&str>,
) -> anyhow::Result<reqwest::Response> {
    // The xAI lane is OpenAI-shaped. Other lanes require streaming translators.
    if !model.to_ascii_lowercase().contains("grok") {
        bail!("Unsupported: Zed model requires a provider stream translator");
    }
    let token = token.context("Zed LLM token required")?;
    let payload = json!({"thread_id":body.get("thread_id"), "prompt_id":body.get("prompt_id"), "provider":"x_ai", "model":model, "provider_request":body});
    let response = client
        .post(endpoint)
        .header("content-type", "application/json")
        .header("accept", "application/x-ndjson, text/event-stream, */*")
        .header("user-agent", "9router/zed")
        .header("x-zed-version", "0.200.0")
        .bearer_auth(token)
        .body(payload.to_string())
        .send()
        .await?;
    if !response.status().is_success() {
        return Ok(response);
    }
    let bytes = response.bytes().await?;
    let text = std::str::from_utf8(&bytes)?;
    let mut output = String::new();
    for line in text.lines() {
        let line = line.trim().strip_prefix("data:").unwrap_or(line).trim();
        if line == "[DONE]" {
            break;
        }
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            if v.get("status").is_some() {
                continue;
            }
            let event = v.get("event").unwrap_or(&v);
            output.push_str(&format!("data: {event}\n\n"));
        }
    }
    output.push_str("data: [DONE]\n\n");
    Ok(reqwest::Response::from(
        axum::http::Response::builder()
            .status(200)
            .header("content-type", "text/event-stream")
            .body(reqwest::Body::from(output))?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, body::Bytes, routing::post};
    #[tokio::test]
    async fn zed_mock_request() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new().fallback(post(|body: Bytes| async move {
            let v: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(v["provider"], "x_ai");
            "{\"event\":{\"choices\":[]}}\n[DONE]\n"
        }));
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let response = execute(
            &reqwest::Client::new(),
            &url,
            "grok-test",
            &json!({"messages":[]}),
            Some("test"),
        )
        .await
        .unwrap();
        assert_eq!(
            response.text().await.unwrap(),
            "data: {\"choices\":[]}\n\ndata: [DONE]\n\n"
        );
        task.abort();
    }
}
