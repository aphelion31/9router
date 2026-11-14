use serde_json::{Value, json};
use std::time::Duration;

// Native protocol blocks stay in place; only plain-text tool-output leaves travel.
fn collect(value: &Value, path: &str, depth: usize, targets: &mut Vec<(String, String)>) {
    if depth > 8 {
        return;
    }
    match value {
        Value::String(s) if !s.trim().is_empty() && serde_json::from_str::<Value>(s).is_err() => {
            targets.push((path.to_owned(), s.clone()))
        }
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                collect(item, &format!("{path}/{i}"), depth + 1, targets);
            }
        }
        Value::Object(map)
            if !map
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|t| t != "text" && t != "input_text") =>
        {
            for key in ["result", "output", "content", "text", "value"] {
                if let Some(item) = map.get(key) {
                    collect(item, &format!("{path}/{key}"), depth + 1, targets);
                }
            }
        }
        _ => {}
    }
}

fn targets(body: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (base, messages) in [
        ("/messages", body.get("messages")),
        ("/input", body.get("input")),
        ("/params/messages", body.pointer("/params/messages")),
        ("/contents", body.get("contents")),
        ("/request/contents", body.pointer("/request/contents")),
    ] {
        let Some(messages) = messages.and_then(Value::as_array) else {
            continue;
        };
        for (i, msg) in messages.iter().enumerate() {
            let p = format!("{base}/{i}");
            if matches!(msg["role"].as_str(), Some("tool" | "function")) {
                collect(&msg["content"], &format!("{p}/content"), 0, &mut out);
            }
            if msg["type"] == "function_call_output" {
                collect(&msg["output"], &format!("{p}/output"), 0, &mut out);
            }
            if let Some(blocks) = msg["content"].as_array() {
                for (j, block) in blocks.iter().enumerate() {
                    let q = format!("{p}/content/{j}");
                    match block["type"].as_str() {
                        Some("tool_result") => {
                            collect(&block["content"], &format!("{q}/content"), 0, &mut out)
                        }
                        Some("tool-result") if block["output"]["type"] == "text" => collect(
                            &block["output"]["value"],
                            &format!("{q}/output/value"),
                            0,
                            &mut out,
                        ),
                        _ => {}
                    }
                }
            }
            if let Some(parts) = msg["parts"].as_array() {
                for (j, part) in parts.iter().enumerate() {
                    if let Some(response) = part.pointer("/functionResponse/response") {
                        collect(
                            response,
                            &format!("{p}/parts/{j}/functionResponse/response"),
                            0,
                            &mut out,
                        );
                    }
                }
            }
        }
    }
    if let Some(state) = body.get("conversationState") {
        for (base, items) in [
            (
                "/conversationState/history",
                state["history"].as_array().cloned().unwrap_or_default(),
            ),
            (
                "/conversationState/currentMessage",
                vec![state["currentMessage"].clone()],
            ),
        ] {
            for (i, item) in items.iter().enumerate() {
                let p = if base.ends_with("history") {
                    format!("{base}/{i}")
                } else {
                    base.to_owned()
                };
                if let Some(results) = item
                    .pointer("/userInputMessage/userInputMessageContext/toolResults")
                    .and_then(Value::as_array)
                {
                    for (j, result) in results.iter().enumerate() {
                        collect(
                            &result["content"],
                            &format!(
                                "{p}/userInputMessage/userInputMessageContext/toolResults/{j}/content"
                            ),
                            0,
                            &mut out,
                        );
                    }
                }
            }
        }
    }
    out
}

pub async fn compress_with_headroom(
    body: &mut Value,
    url: &str,
    model: &str,
    timeout_ms: u64,
    compress_user_messages: bool,
) -> Result<Value, &'static str> {
    if url.is_empty() {
        return Err("missing proxy URL");
    }
    let targets = targets(body);
    if targets.is_empty() {
        return Err("no eligible plain-text tool output");
    }
    let mut endpoint = reqwest::Url::parse(url).map_err(|_| "invalid proxy URL")?;
    if !matches!(endpoint.scheme(), "http" | "https") {
        return Err("invalid proxy URL");
    }
    endpoint.set_path(&format!(
        "{}/v1/compress",
        endpoint.path().trim_end_matches('/')
    ));
    endpoint.set_fragment(None);
    let messages: Vec<Value> = targets.iter().enumerate().map(|(i, (_, text))| json!({"role":"tool", "tool_call_id":format!("headroom-tool-{i}"), "content":text})).collect();
    let mut payload = json!({"messages":messages, "model":model});
    if compress_user_messages {
        payload["config"] = json!({"compress_user_messages":true});
    }
    let before = serde_json::to_vec(body).map_or(0, |v| v.len());
    let message_before = body
        .get("messages")
        .or_else(|| body.get("input"))
        .map_or(0, |v| serde_json::to_vec(v).map_or(0, |v| v.len()));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(if timeout_ms == 0 {
            3000
        } else {
            timeout_ms
        }))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "client unavailable")?;
    let response = client
        .post(endpoint)
        .header("Content-Type", "application/json")
        .body(payload.to_string())
        .send()
        .await
        .map_err(|_| "request failed")?;
    if !response.status().is_success() {
        return Err("proxy returned an error");
    }
    let bytes = response.bytes().await.map_err(|_| "response failed")?;
    let data: Value = serde_json::from_slice(&bytes).map_err(|_| "invalid proxy response")?;
    let compressed = data["messages"]
        .as_array()
        .ok_or("proxy response missing messages[]")?;
    if compressed.len() != messages.len() {
        return Err("proxy response did not match tool-output count");
    }
    let mut updates = Vec::new();
    for (i, msg) in compressed.iter().enumerate() {
        if msg["role"] != "tool" || msg["tool_call_id"] != messages[i]["tool_call_id"] {
            return Err("proxy response did not preserve tool-output IDs, order or roles");
        }
        let text = msg["content"]
            .as_str()
            .ok_or("proxy response missing tool-output text")?;
        if !text.trim().is_empty()
            && text.encode_utf16().count() < targets[i].1.encode_utf16().count()
        {
            updates.push((targets[i].0.clone(), text.to_owned()));
        }
    }
    for (path, text) in updates {
        if let Some(value) = body.pointer_mut(&path) {
            *value = Value::String(text);
        }
    }
    let after = serde_json::to_vec(body).map_or(0, |v| v.len());
    let message_after = body
        .get("messages")
        .or_else(|| body.get("input"))
        .map_or(0, |v| serde_json::to_vec(v).map_or(0, |v| v.len()));
    let saved = data["tokens_saved"].as_f64().unwrap_or(0.0);
    tracing::info!(target: "headroom", tokens_before = %data["tokens_before"], tokens_after = %data["tokens_after"], tokens_saved = %data["tokens_saved"], body_before = before, body_after = after, "reported token delta");
    if saved > 0.0
        && [(before, after), (message_before, message_after)]
            .iter()
            .all(|&(a, b)| a == 0 || (b as f64) > (a as f64) * 0.95)
    {
        tracing::warn!(target: "headroom", "reported token delta, but messages and outbound JSON shrank <5%");
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, routing::post};
    use tokio::net::TcpListener;

    async fn mock(mode: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new().route(
            "/v1/compress",
            post(move |Json(mut payload): Json<Value>| async move {
                if mode == "timeout" {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                if mode == "invalid" {
                    payload["messages"][0]["tool_call_id"] = json!("wrong");
                } else {
                    payload["messages"][0]["content"] = json!(if mode == "long" {
                        "this is a much longer replacement"
                    } else {
                        "short"
                    });
                }
                payload["tokens_saved"] = json!(10);
                Json(payload)
            }),
        );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn eligible_tool_output_compressed_and_applied() {
        let mut body = json!({"messages":[{"role":"tool","content":"a long plain tool output"}],"tools":[{"name":"keep"}]});
        let result = compress_with_headroom(&mut body, &mock("ok").await, "model", 1000, false)
            .await
            .unwrap();
        assert_eq!(body["messages"][0]["content"], "short");
        assert_eq!(body["tools"][0]["name"], "keep");
        assert_eq!(result["tokens_saved"], 10);
    }

    #[tokio::test]
    async fn ineligible_skipped_and_failures_preserve_body() {
        let mut ineligible =
            json!({"messages":[{"role":"tool","content":"{\"structured\":true}"}]});
        assert_eq!(
            compress_with_headroom(&mut ineligible, "http://127.0.0.1:1", "m", 10, false)
                .await
                .unwrap_err(),
            "no eligible plain-text tool output"
        );
        for (url, timeout) in [
            ("http://127.0.0.1:1".to_owned(), 100),
            (mock("timeout").await, 10),
            (mock("invalid").await, 1000),
        ] {
            let original =
                json!({"messages":[{"role":"tool","content":"a long plain tool output"}]});
            let mut body = original.clone();
            assert!(
                compress_with_headroom(&mut body, &url, "m", timeout, false)
                    .await
                    .is_err()
            );
            assert_eq!(body, original);
        }
    }

    #[tokio::test]
    async fn longer_replacement_guard_keeps_original() {
        let original = json!({"messages":[{"role":"tool","content":"plain text"}]});
        let mut body = original.clone();
        compress_with_headroom(&mut body, &mock("long").await, "m", 1000, false)
            .await
            .unwrap();
        assert_eq!(body, original);
    }
}
