use serde_json::{Value, json};
use std::time::Duration;

fn bytes(v: &Value) -> usize {
    serde_json::to_vec(v).map_or(0, |v| v.len())
}
fn collect(value: &Value, path: &str, depth: usize, out: &mut Vec<(String, String)>) {
    if depth > 8 {
        return;
    }
    match value {
        Value::String(s) if !s.trim().is_empty() && serde_json::from_str::<Value>(s).is_err() => {
            out.push((path.into(), s.clone()))
        }
        Value::Array(a) => {
            for (i, v) in a.iter().enumerate() {
                collect(v, &format!("{path}/{i}"), depth + 1, out);
            }
        }
        Value::Object(o)
            if !o
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|t| t != "text" && t != "input_text") =>
        {
            for k in ["result", "output", "content", "text"] {
                if let Some(v) = o.get(k) {
                    collect(v, &format!("{path}/{k}"), depth + 1, out);
                }
            }
        }
        _ => {}
    }
}

#[derive(Default)]
struct Projection {
    messages: Vec<Value>,
    paths: Vec<Option<String>>,
}
impl Projection {
    fn add(&mut self, body: &Value, path: String, role: &Value, extra: Value) {
        if let Some(text) = body.pointer(&path).and_then(Value::as_str) {
            let mut m = json!({"role":role,"content":text});
            if let (Some(dst), Some(src)) = (m.as_object_mut(), extra.as_object()) {
                dst.extend(src.clone());
            }
            self.messages.push(m);
            self.paths.push(Some(path));
        }
    }
    fn content(&mut self, body: &Value, path: String, role: &Value, extra: Value) {
        if let Some(a) = body.pointer(&path).and_then(Value::as_array) {
            for (i, b) in a.iter().enumerate() {
                if b["type"] == "text" {
                    self.add(body, format!("{path}/{i}/text"), role, extra.clone());
                }
            }
        } else {
            self.add(body, path, role, extra);
        }
    }
    fn context(&mut self, m: Value) {
        self.messages.push(m);
        self.paths.push(None);
    }
}
fn projection(body: &Value, format: &str) -> Option<Projection> {
    let mut p = Projection::default();
    if format == "claude" {
        let messages = body.get("messages")?.as_array()?;
        p.content(body, "/system".into(), &json!("system"), json!({}));
        for (i, m) in messages.iter().enumerate() {
            let blocks = m["content"].as_array().cloned().unwrap_or_default();
            let calls: Vec<Value> = blocks.iter().filter(|b| b["type"] == "tool_use").map(|b| json!({"id":b["id"],"type":"function","function":{"name":b["name"],"arguments":b.get("input").filter(|v| !v.is_null()).unwrap_or(&json!({})).to_string()}})).collect();
            if m["role"] == "assistant" && !calls.is_empty() {
                p.context(json!({"role":"assistant","content":"","tool_calls":calls}));
            }
            p.content(
                body,
                format!("/messages/{i}/content"),
                &m["role"],
                json!({}),
            );
            for (j, b) in blocks.iter().enumerate() {
                if b["type"] == "tool_result" {
                    p.content(
                        body,
                        format!("/messages/{i}/content/{j}/content"),
                        &json!("tool"),
                        json!({"tool_call_id":b["tool_use_id"]}),
                    );
                }
            }
        }
    } else if format == "commandcode" {
        let messages = body.pointer("/params/messages")?.as_array()?;
        p.add(body, "/params/system".into(), &json!("system"), json!({}));
        for (i, m) in messages.iter().enumerate() {
            let Some(blocks) = m["content"].as_array() else {
                continue;
            };
            let calls: Vec<Value> = blocks.iter().filter(|b| b["type"] == "tool-call").map(|b| json!({"id":b["toolCallId"],"type":"function","function":{"name":b["toolName"],"arguments":b.get("input").filter(|v| !v.is_null()).unwrap_or(&json!({})).to_string()}})).collect();
            if m["role"] == "assistant" && !calls.is_empty() {
                p.context(json!({"role":"assistant","content":"","tool_calls":calls}));
            }
            for (j, b) in blocks.iter().enumerate() {
                let base = format!("/params/messages/{i}/content/{j}");
                if b["type"] == "text" {
                    p.add(body, format!("{base}/text"), &m["role"], json!({}));
                }
                if b["type"] == "tool-result" && b["output"]["type"] == "text" {
                    p.add(
                        body,
                        format!("{base}/output/value"),
                        &json!("tool"),
                        json!({"tool_call_id":b["toolCallId"],"name":b["toolName"]}),
                    );
                }
            }
        }
    } else if format == "kiro" {
        let state = body.get("conversationState")?;
        let mut paths: Vec<String> = state["history"]
            .as_array()
            .map(|a| {
                (0..a.len())
                    .map(|i| format!("/conversationState/history/{i}"))
                    .collect()
            })
            .unwrap_or_default();
        if !state["currentMessage"].is_null() {
            paths.push("/conversationState/currentMessage".into());
        }
        for base in paths {
            let item = &body.pointer(&base)?;
            if !item["userInputMessage"].is_null() {
                let u = format!("{base}/userInputMessage");
                p.add(
                    body,
                    format!("{u}/systemInstruction"),
                    &json!("system"),
                    json!({}),
                );
                p.add(body, format!("{u}/content"), &json!("user"), json!({}));
                if let Some(results) = item
                    .pointer("/userInputMessage/userInputMessageContext/toolResults")
                    .and_then(Value::as_array)
                {
                    for (j, result) in results.iter().enumerate() {
                        if let Some(parts) = result["content"].as_array() {
                            for (k, _) in parts.iter().enumerate() {
                                let extra = result
                                    .get("toolUseId")
                                    .filter(|v| v.as_str().is_some_and(|s| !s.is_empty()))
                                    .map_or(json!({}), |id| json!({"tool_call_id":id}));
                                p.add(body, format!("{u}/userInputMessageContext/toolResults/{j}/content/{k}/text"), &json!("tool"), extra);
                            }
                        }
                    }
                }
            } else if !item["assistantResponseMessage"].is_null() {
                let a = &item["assistantResponseMessage"];
                let calls: Vec<Value> = a["toolUses"].as_array().into_iter().flatten().filter(|v| !v["toolUseId"].is_null() || v["name"].as_str().is_some_and(|s| !s.is_empty())).map(|v| json!({"id":v["toolUseId"],"type":"function","function":{"name":v["name"].as_str().unwrap_or(""),"arguments":v.get("input").filter(|v| !v.is_null()).unwrap_or(&json!({})).to_string()}})).collect();
                let extra = if calls.is_empty() {
                    json!({})
                } else {
                    json!({"tool_calls":calls})
                };
                p.add(
                    body,
                    format!("{base}/assistantResponseMessage/content"),
                    &json!("assistant"),
                    extra,
                );
            }
        }
    } else {
        return None;
    }
    if p.paths.iter().any(Option::is_some) {
        Some(p)
    } else {
        None
    }
}
fn snapshot(body: &Value) -> [usize; 4] {
    let kiro = if body.get("conversationState").is_some() {
        projection(body, "kiro").map(|p| Value::Array(p.messages))
    } else {
        None
    };
    let messages = [
        body.get("messages"),
        body.get("input"),
        body.pointer("/params/messages"),
        body.pointer("/request/contents"),
        body.get("contents"),
        kiro.as_ref(),
    ]
    .into_iter()
    .flatten()
    .find(|v| v.is_array());
    let history: Vec<Value> = messages
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|m| {
            matches!(m["role"].as_str(), Some("tool" | "function"))
                || m["tool_calls"].as_array().is_some_and(|a| !a.is_empty())
                || m["type"] == "function_call_output"
                || m["parts"].as_array().is_some_and(|a| {
                    a.iter()
                        .any(|p| !p["functionResponse"].is_null() || !p["functionCall"].is_null())
                })
                || m["content"].as_array().is_some_and(|a| {
                    a.iter().any(|p| {
                        matches!(
                            p["type"].as_str(),
                            Some("tool_use" | "tool_result" | "tool-call" | "tool-result")
                        )
                    })
                })
        })
        .cloned()
        .collect();
    let tools = body
        .get("tools")
        .or_else(|| body.pointer("/params/tools"))
        .or_else(|| body.pointer("/request/tools"));
    [
        bytes(body),
        messages.map_or(0, bytes),
        tools.map_or(2, bytes),
        bytes(&json!(history)),
    ]
}
fn size_log(a: [usize; 4], b: [usize; 4]) -> String {
    let pct = if a[0] > 0 {
        (a[0] as f64 - b[0] as f64) * 100.0 / a[0] as f64
    } else {
        0.0
    };
    format!(
        "body={}B→{}B messages={}B→{}B tools={}B→{}B toolHistory={}B→{}B effective={pct:.1}%",
        a[0], b[0], a[1], b[1], a[2], b[2], a[3], b[3]
    )
}
fn phantom_savings(saved: f64, before: [usize; 4], after: [usize; 4]) -> bool {
    saved > 0.0
        && (0..2)
            .all(|i| before[i] > 0 && after[i] > 0 && after[i] as f64 >= before[i] as f64 * 0.95)
}
fn text(m: &Value) -> Option<String> {
    if let Some(s) = m["content"].as_str() {
        return Some(s.to_owned());
    }
    let parts: Vec<&str> = m["content"]
        .as_array()?
        .iter()
        .filter_map(|p| p.as_str().or_else(|| p["text"].as_str()))
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}
pub async fn compress_with_headroom(
    body: &mut Value,
    url: &str,
    model: &str,
    format: &str,
    timeout_ms: u64,
    compress_user_messages: bool,
) -> Result<Value, String> {
    if url.is_empty() {
        return Err("missing proxy URL".into());
    }
    let before = snapshot(body);
    let unsafe_responses = format == "openai-responses"
        && body["input"].as_array().is_some_and(|a| {
            a.iter()
                .any(|i| i.is_object() && i["type"].as_str().is_some_and(|t| t != "message"))
        });
    let native =
        unsafe_responses || matches!(format, "gemini" | "gemini-cli" | "vertex" | "antigravity");
    let mut targets = Vec::new();
    if unsafe_responses {
        if let Some(a) = body["input"].as_array() {
            for (i, v) in a.iter().enumerate() {
                if v["type"] == "function_call_output" {
                    collect(&v["output"], &format!("/input/{i}/output"), 0, &mut targets);
                }
            }
        }
    } else if native {
        let (base, contents) = if body
            .pointer("/request/contents")
            .and_then(Value::as_array)
            .is_some()
        {
            ("/request/contents", body.pointer("/request/contents"))
        } else {
            ("/contents", body.get("contents"))
        };
        if let Some(a) = contents.and_then(Value::as_array) {
            for (i, m) in a.iter().enumerate() {
                if let Some(parts) = m["parts"].as_array() {
                    for (j, part) in parts.iter().enumerate() {
                        if let Some(v) = part.pointer("/functionResponse/response") {
                            collect(
                                v,
                                &format!("{base}/{i}/parts/{j}/functionResponse/response"),
                                0,
                                &mut targets,
                            );
                        }
                    }
                }
            }
        }
    }
    let projected = if matches!(format, "claude" | "kiro" | "commandcode") {
        Some(projection(body, format).ok_or("request has no eligible text content")?)
    } else {
        None
    };
    let key = if body["messages"].is_array() {
        "messages"
    } else {
        "input"
    };
    let mut responses = None;
    let messages = if native {
        if targets.is_empty() {
            return Err("no eligible plain-text tool output".into());
        }
        targets.iter().enumerate().map(|(i, (_, s))| json!({"role":"tool","tool_call_id":format!("headroom-tool-{i}"),"content":s})).collect::<Vec<_>>()
    } else if let Some(p) = &projected {
        p.messages.clone()
    } else if format == "openai-responses" {
        let oai = crate::translator::openai_responses::openai_responses_to_openai_request(
            model, body, false, None,
        );
        let m = oai["messages"]
            .as_array()
            .ok_or("openai-responses request did not translate to messages[]")?
            .clone();
        responses = Some(oai);
        m
    } else {
        body[key]
            .as_array()
            .ok_or("unsupported request shape")?
            .clone()
    };
    let mut endpoint = reqwest::Url::parse(url).map_err(|_| "invalid proxy URL")?;
    if !matches!(endpoint.scheme(), "http" | "https") {
        return Err("invalid proxy URL".into());
    }
    endpoint.set_path(&format!(
        "{}/v1/compress",
        endpoint.path().trim_end_matches('/')
    ));
    endpoint.set_fragment(None);
    let mut payload = json!({"messages":messages,"model":model});
    if compress_user_messages {
        payload["config"] = json!({"compress_user_messages":true});
    }
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
        return Err(format!(
            "proxy returned HTTP {}",
            response.status().as_u16()
        ));
    }
    let data: Value =
        serde_json::from_slice(&response.bytes().await.map_err(|_| "response failed")?)
            .map_err(|_| "invalid proxy response")?;
    let compressed = data["messages"]
        .as_array()
        .ok_or("proxy response missing messages[]")?;
    if native || projected.is_some() {
        if compressed.len() != messages.len() {
            return Err("proxy response did not match message count".into());
        }
        let mut updates = Vec::new();
        for (i, m) in compressed.iter().enumerate() {
            if m["role"] != messages[i]["role"]
                || m.get("tool_call_id") != messages[i].get("tool_call_id")
            {
                return Err("proxy response did not preserve message order".into());
            }
            if native {
                let s = m["content"]
                    .as_str()
                    .ok_or("proxy response missing tool-output text")?;
                if !s.trim().is_empty()
                    && s.encode_utf16().count() < targets[i].1.encode_utf16().count()
                {
                    updates.push((targets[i].0.clone(), s.to_owned()));
                }
            } else {
                let s = text(m).ok_or("proxy response missing text content")?;
                if let Some(path) = &projected.as_ref().unwrap().paths[i] {
                    updates.push((path.clone(), s));
                }
            }
        }
        for (path, s) in updates {
            if let Some(v) = body.pointer_mut(&path) {
                *v = json!(s);
            }
        }
    } else if let Some(mut oai) = responses {
        oai["messages"] = json!(compressed);
        if let Some(o) = oai.as_object_mut() {
            o.remove("input");
        }
        let converted = crate::translator::openai_responses::openai_to_openai_responses_request(
            model, &oai, false, None,
        );
        if converted["input"].is_array() {
            body["input"] = converted["input"].clone();
        }
    } else {
        body[key] = json!(compressed);
    }
    let after = snapshot(body);
    let b = data["tokens_before"].as_f64().unwrap_or(0.0);
    let a = data["tokens_after"].as_f64().unwrap_or(0.0);
    let saved = data["tokens_saved"].as_f64().unwrap_or(0.0);
    let pct = if b > 0.0 {
        format!("{:.1}", saved / b * 100.0)
    } else {
        "0".into()
    };
    let suffix = if a != 0.0 {
        format!(" after={a}")
    } else {
        String::new()
    };
    let sizes = size_log(before, after);
    tracing::info!(target: "headroom", "reported token delta={saved} before={b}{suffix} ({pct}%) | {sizes}");
    if phantom_savings(saved, before, after) {
        tracing::warn!(target: "headroom", "reported token delta, but messages and outbound JSON shrank <5% | {sizes}");
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, routing::post};
    use tokio::sync::mpsc;

    async fn mock(invalid: bool) -> (String, mpsc::Receiver<Value>) {
        let (tx, rx) = mpsc::channel(1);
        let app = Router::new().route(
            "/v1/compress",
            post(move |Json(mut payload): Json<Value>| {
                let tx = tx.clone();
                async move {
                    let _ = tx.send(payload.clone()).await;
                    for m in payload["messages"].as_array_mut().unwrap() {
                        if m["content"].as_str().is_some_and(|s| !s.is_empty()) {
                            m["content"] = json!("short");
                        }
                    }
                    if invalid {
                        payload["messages"][0]["role"] = json!("wrong");
                    }
                    payload["tokens_before"] = json!(100);
                    payload["tokens_after"] = json!(90);
                    payload["tokens_saved"] = json!(10);
                    Json(payload)
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{address}"), rx)
    }

    #[tokio::test]
    async fn openai_sends_full_conversation_and_user_flag() {
        let (url, mut rx) = mock(false).await;
        let mut body = json!({"messages":[{"role":"user","content":"long user message"},{"role":"tool","tool_call_id":"a","content":"long output"}],"tools":[{"name":"x"}]});
        compress_with_headroom(&mut body, &url, "m", "openai", 1000, true)
            .await
            .unwrap();
        let sent = rx.recv().await.unwrap();
        assert_eq!(sent["messages"].as_array().unwrap().len(), 2);
        assert_eq!(sent["config"]["compress_user_messages"], true);
        assert_eq!(body["messages"][0]["content"], "short");
        assert_eq!(body["tools"][0]["name"], "x");
    }

    #[tokio::test]
    async fn native_projections_preserve_metadata_and_validate() {
        for (format, original, path) in [
            (
                "claude",
                json!({"system":"system long", "messages":[{"role":"user","content":[{"type":"text","text":"long user"},{"type":"image","source":{"data":"keep"}}]}]}),
                "/messages/0/content/0/text",
            ),
            (
                "gemini",
                json!({"contents":[{"parts":[{"functionResponse":{"response":{"output":"long tool output"}}}]}]}),
                "/contents/0/parts/0/functionResponse/response/output",
            ),
            (
                "openai-responses",
                json!({"input":[{"type":"function_call_output","call_id":"x","output":"long tool output"},{"type":"reasoning","id":"keep"}]}),
                "/input/0/output",
            ),
            (
                "commandcode",
                json!({"params":{"messages":[{"role":"user","content":[{"type":"text","text":"long user"}]}]}}),
                "/params/messages/0/content/0/text",
            ),
            (
                "kiro",
                json!({"conversationState":{"currentMessage":{"userInputMessage":{"content":"long user"}}}}),
                "/conversationState/currentMessage/userInputMessage/content",
            ),
        ] {
            let (url, _) = mock(false).await;
            let mut body = original.clone();
            compress_with_headroom(&mut body, &url, "m", format, 1000, true)
                .await
                .unwrap();
            assert_eq!(body.pointer(path).unwrap(), "short", "{format}");
            let (url, _) = mock(true).await;
            let mut body = original.clone();
            assert!(
                compress_with_headroom(&mut body, &url, "m", format, 1000, false)
                    .await
                    .is_err()
            );
            assert_eq!(body, original);
        }
    }

    #[tokio::test]
    async fn responses_messages_round_trip() {
        let (url, mut rx) = mock(false).await;
        let mut body = json!({"input":[{"type":"message","role":"user","content":[{"type":"input_text","text":"long user"}]}]});
        compress_with_headroom(&mut body, &url, "m", "openai-responses", 1000, true)
            .await
            .unwrap();
        assert_eq!(rx.recv().await.unwrap()["messages"][0]["role"], "user");
        assert!(body["input"].to_string().contains("short"));
    }

    #[tokio::test]
    async fn network_failures_and_structured_native_output_preserve_body() {
        let original = json!({"messages":[{"role":"user","content":"hello"}]});
        let mut body = original.clone();
        assert!(
            compress_with_headroom(&mut body, "http://127.0.0.1:1", "m", "openai", 50, false)
                .await
                .is_err()
        );
        assert_eq!(body, original);
        let mut body = json!({"contents":[{"parts":[{"functionResponse":{"response":{"output":"{\"ok\":true}"}}}]}]});
        let original = body.clone();
        assert!(
            compress_with_headroom(&mut body, "http://127.0.0.1:1", "m", "gemini", 50, false)
                .await
                .is_err()
        );
        assert_eq!(body, original);
    }

    #[test]
    fn size_snapshot_counts_all_native_payloads() {
        let body = json!({"request":{"contents":[{"parts":[{"functionResponse":{"response":"x"}}]}],"tools":[{"x":1}]}});
        let s = snapshot(&body);
        assert_eq!(s[1], bytes(&body["request"]["contents"]));
        assert_eq!(s[2], bytes(&body["request"]["tools"]));
        assert_eq!(s[3], s[1]);
        assert!(size_log(s, s).contains("effective=0.0%"));
        assert!(phantom_savings(10.0, [100, 100, 0, 0], [95, 96, 0, 0]));
        assert!(!phantom_savings(10.0, [100, 100, 0, 0], [94, 96, 0, 0]));
        assert!(!phantom_savings(0.0, s, s));
    }
}
