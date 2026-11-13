use anyhow::{Context, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};

pub(crate) fn zed_provider_for_model(model: &str) -> &'static str {
    let m = model.to_ascii_lowercase();
    if m.contains("claude") {
        "anthropic"
    } else if m.contains("gemini") {
        "google"
    } else if m.contains("grok") {
        "x_ai"
    } else {
        "openai"
    }
}

pub(crate) async fn exchange_llm_token(
    client: &reqwest::Client,
    cloud_base: Option<&str>,
    user_id: &str,
    access_token: &str,
) -> anyhow::Result<String> {
    let base = cloud_base
        .unwrap_or("https://cloud.zed.dev")
        .trim_end_matches('/');
    let basic = STANDARD.encode(format!("{user_id}:{access_token}"));
    let res = client
        .post(format!("{base}/client/llm_tokens"))
        .header("Authorization", format!("Basic {basic}"))
        .header("Content-Type", "application/json")
        .body("{}")
        .send()
        .await?;
    if !res.status().is_success() {
        bail!("Zed LLM token exchange HTTP {}", res.status());
    }
    let body: Value = serde_json::from_slice(&res.bytes().await?)?;
    body.get("jwt_token")
        .or_else(|| body.get("token"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .context("Zed LLM token exchange missing jwt_token")
}

pub(super) async fn execute(
    client: &reqwest::Client,
    endpoint: &str,
    model: &str,
    body: &Value,
    token: Option<&str>,
) -> anyhow::Result<reqwest::Response> {
    let provider = zed_provider_for_model(model);
    let token = token.context("Zed LLM token required")?;
    let payload = json!({"thread_id":body.get("thread_id"), "prompt_id":body.get("prompt_id"), "provider":provider, "model":model, "provider_request":body});
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

    #[tokio::test]
    async fn zed_token_exchange_and_provider_routing_work() {
        assert_eq!(zed_provider_for_model("claude-sonnet-4"), "anthropic");
        assert_eq!(zed_provider_for_model("gemini-2.5-pro"), "google");
        assert_eq!(zed_provider_for_model("grok-3"), "x_ai");
        assert_eq!(zed_provider_for_model("gpt-4.1"), "openai");

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new().route(
            "/client/llm_tokens",
            post(|headers: axum::http::HeaderMap| async move {
                assert!(
                    headers["authorization"]
                        .to_str()
                        .unwrap()
                        .starts_with("Basic ")
                );
                axum::Json(json!({"jwt_token": "zed_jwt_123"}))
            }),
        );
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let tok = exchange_llm_token(&reqwest::Client::new(), Some(&base), "u1", "tok1")
            .await
            .unwrap();
        assert_eq!(tok, "zed_jwt_123");
        task.abort();
    }
}
