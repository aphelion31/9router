use serde_json::Value;
use futures_core::Stream;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ResponseSignals {
    pub text: String,
    pub has_tools: bool,
    pub has_reasoning: bool,
}

pub fn resolve_anti_flaky_config(settings: &Value) -> (bool, u8) {
    let retries = settings["antiFlakyMaxRetries"]
        .as_f64()
        .filter(|n| n.is_finite())
        .map(|n| n.floor().clamp(0.0, 5.0) as u8)
        .unwrap_or(2);
    (settings["antiFlakyEnabled"] != false, retries)
}
fn content_text(content: &Value) -> String {
    if let Some(s) = content.as_str() {
        return s.into();
    }
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().or_else(|| v["text"].as_str()))
        .collect()
}
pub fn extract_response_signals(json: &Value) -> ResponseSignals {
    let mut sig = ResponseSignals::default();
    if let Some(choice) = json["choices"].as_array().and_then(|a| a.first()) {
        if let Some(msg) = choice
            .get("message")
            .filter(|v| !v.is_null())
            .or_else(|| choice.get("delta"))
        {
            sig.text = content_text(&msg["content"]);
            sig.has_tools = msg["tool_calls"].as_array().is_some_and(|a| !a.is_empty());
            sig.has_reasoning = msg["reasoning_content"]
                .as_str()
                .is_some_and(|s| !s.trim().is_empty());
            return sig;
        }
    }
    if json["type"] == "message" {
        if let Some(blocks) = json["content"].as_array() {
            for block in blocks {
                match block["type"].as_str() {
                    Some("text") => sig.text.push_str(block["text"].as_str().unwrap_or("")),
                    Some("tool_use") => sig.has_tools = true,
                    Some("thinking") => {
                        sig.has_reasoning |= block["thinking"]
                            .as_str()
                            .is_some_and(|s| !s.trim().is_empty())
                    }
                    _ => {}
                }
            }
            return sig;
        }
    }
    if let Some(items) = json["output"].as_array() {
        for item in items {
            match item["type"].as_str() {
                Some("function_call" | "custom_tool_call") => sig.has_tools = true,
                Some("reasoning") => sig.has_reasoning = true,
                _ if item["content"].is_array() => {
                    sig.text.push_str(&content_text(&item["content"]))
                }
                _ => {}
            }
        }
        return sig;
    }
    if let Some(parts) = json["candidates"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(|c| c["content"]["parts"].as_array())
    {
        for part in parts {
            if part["thought"] != true {
                if let Some(text) = part["text"].as_str() {
                    sig.text.push_str(text);
                }
            }
            sig.has_tools |= !part["functionCall"].is_null() || !part["function_call"].is_null();
            if !part["inlineData"].is_null() || !part["inline_data"].is_null() {
                sig.text.push(' ');
            }
        }
    }
    sig
}

pub fn is_degenerate_text(text: &str) -> bool {
    let t: Vec<char> = text.trim().chars().collect();
    if t.len() < 24 {
        return false;
    }
    for unit in 1..=16.min(t.len()) {
        if t[..unit].iter().all(|c| c.is_whitespace()) {
            continue;
        }
        let mut covered = 0;
        while covered + unit <= t.len() && t[covered..covered + unit] == t[..unit] {
            covered += unit;
        }
        let rest = &t[covered..];
        let tail_ok = !rest.is_empty() && rest.len() < unit && t[..unit].starts_with(rest);
        if (covered == t.len() || tail_ok) && covered * 5 >= t.len() * 4 && covered / unit >= 4 {
            return true;
        }
    }
    let words: Vec<_> = text.split_whitespace().collect();
    if words.len() < 12 {
        return false;
    }
    let (mut run, mut max) = (1, 1);
    for pair in words.windows(2) {
        if pair[0] == pair[1] {
            run += 1;
        } else {
            run = 1;
        }
        max = max.max(run);
    }
    max >= 8 && max * 2 >= words.len()
}

pub fn detect_flaky_response(json: &Value) -> Option<&'static str> {
    if !json.is_object() {
        return None;
    }
    let sig = extract_response_signals(json);
    if sig.has_tools || sig.has_reasoning {
        return None;
    }
    if sig.text.trim().is_empty() {
        return Some("empty");
    }
    if is_degenerate_text(&sig.text) {
        return Some("degenerate");
    }
    None
}

/// Verdict for an SSE stream that ended inside the probe window: any assistant content
/// across deltas is judged together. Tools or reasoning anywhere makes it legitimate.
pub fn sse_ended_verdict(text: &str) -> Option<&'static str> {
    let mut acc = String::new();
    let (mut has_tools, mut has_reasoning, mut saw_payload) = (false, false, false);
    for line in text.lines().map(|l| l.trim_end_matches('\r')) {
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let payload = data.trim();
        if payload.is_empty() || payload == "[DONE]" {
            continue;
        }
        let Ok(json) = serde_json::from_str::<Value>(payload) else {
            continue;
        };
        saw_payload = true;
        let sig = extract_response_signals(&json);
        acc.push_str(&sig.text);
        has_tools |= sig.has_tools;
        has_reasoning |= sig.has_reasoning;
        if let (Some(delta), Some(kind)) = (json["delta"].as_str(), json["type"].as_str()) {
            if kind.contains("output_text") {
                acc.push_str(delta);
            }
        }
    }
    if !saw_payload || has_tools || has_reasoning {
        return None;
    }
    if acc.trim().is_empty() {
        return Some("empty");
    }
    is_degenerate_text(&acc).then_some("degenerate")
}

/// Pulls the raw `"content"` string value out of a body that is not valid JSON
/// (unescaped control chars, trailing junk). Returns the unescaped text when possible.
fn salvage_content(body: &str) -> Option<String> {
    let at = body.find("\"content\"")? + "\"content\"".len();
    let rest = body[at..].trim_start().strip_prefix(':')?.trim_start().strip_prefix('"')?;
    let mut raw = String::new();
    let mut escaped = false;
    for c in rest.chars() {
        if escaped {
            raw.push('\\');
            raw.push(c);
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '"' {
            return Some(serde_json::from_str::<String>(&format!("\"{raw}\"")).unwrap_or(raw));
        } else {
            raw.push(c);
        }
    }
    None
}

/// Judges a finished body (materialized JSON or an SSE stream that closed).
/// `None` means not flaky. Unknown shapes and unusable input fail open.
pub fn judge_finished_text(text: &str) -> Option<&'static str> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Some("empty");
    }
    if trimmed.starts_with('{') {
        return match serde_json::from_str::<Value>(trimmed) {
            Ok(json) => detect_flaky_response(&json),
            Err(_) => {
                if trimmed.contains("\"tool_calls\"") || trimmed.contains("\"function_call\"") {
                    return None;
                }
                let content = salvage_content(trimmed)?;
                if content.trim().is_empty() {
                    Some("empty")
                } else {
                    is_degenerate_text(&content).then_some("degenerate")
                }
            }
        };
    }
    if trimmed.starts_with("data:") || trimmed.starts_with("event:") {
        return sse_ended_verdict(trimmed);
    }
    None
}

/// Result of a bounded probe. The caller must replay buffered frames.
#[derive(Debug)]
pub struct Probe {
    pub flaky: Option<&'static str>,
    pub buffered: Vec<Result<axum::body::Bytes, axum::Error>>,
    pub ended: bool,
}

/// Reads up to `budget` / `max_bytes`. Only a stream that finishes inside the window is judged;
/// a live stream is released as not flaky, fail open.
pub async fn probe_response(
    response: &mut axum::body::BodyDataStream,
    budget: std::time::Duration,
    max_bytes: usize,
) -> Probe {
    let deadline = tokio::time::Instant::now() + budget;
    let mut buffered = Vec::new();
    let mut text = Vec::new();
    let mut ended = false;
    while text.len() <= max_bytes {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, std::future::poll_fn(|cx| std::pin::Pin::new(&mut *response).poll_next(cx))).await {
            Ok(Some(Ok(chunk))) => { text.extend_from_slice(&chunk); buffered.push(Ok(chunk)); },
            Ok(None) => {
                ended = true;
                break;
            }
            Ok(Some(Err(error))) => { buffered.push(Err(error)); break; },
            Err(_) => break,
        }
    }
    let flaky = if ended {
        judge_finished_text(&String::from_utf8_lossy(&text))
    } else {
        None
    };
    Probe {
        flaky,
        buffered,
        ended,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::{Duration, Instant};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn judges_finished_bodies() {
        assert_eq!(judge_finished_text(""), Some("empty"));
        assert_eq!(
            judge_finished_text(r#"{"choices":[{"message":{"content":"ok fine"}}]}"#),
            None
        );
        assert_eq!(
            judge_finished_text(r#"data: {"choices":[{"delta":{"content":""}}]}

data: [DONE]
"#),
            Some("empty")
        );
        assert_eq!(
            judge_finished_text(r#"data: {"choices":[{"delta":{"content":"ab"}}]}

data: {"choices":[{"delta":{"content":"ab"}}]}
"#),
            None
        );
        // Malformed JSON with a salvageable degenerate content field.
        let bad = format!(r#"{{"choices":[{{"message":{{"content":"{}"}}}}],"x":"#, "xyz".repeat(12));
        assert_eq!(judge_finished_text(&bad), Some("degenerate"));
        assert_eq!(
            judge_finished_text(r#"{"choices":[{"message":{"tool_calls":[{}]}}"#),
            None
        );
    }

    #[tokio::test]
    async fn probe_judges_fast_stream_and_releases_live_stream() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            // Connection 1: complete SSE with no content, then close.
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let body = "data: {\"choices\":[{\"delta\":{\"content\":\"\"}}]}\n\ndata: [DONE]\n\n";
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            sock.write_all(resp.as_bytes()).await.unwrap();

            // Connection 2: headers and a partial chunk, then stay open.
            let (mut live, _) = listener.accept().await.unwrap();
            let _ = live.read(&mut buf).await;
            live.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n")
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_secs(3)).await;
        });

        let client = reqwest::Client::new();
        let fast = client.get(format!("http://{addr}/a")).send().await.unwrap();
        let mut fast = axum::body::Body::from_stream(fast.bytes_stream()).into_data_stream();
        let probe = probe_response(&mut fast, Duration::from_millis(500), 2_000_000).await;
        assert!(probe.ended);
        assert_eq!(probe.flaky, Some("empty"));

        let live = client.get(format!("http://{addr}/b")).send().await.unwrap();
        let mut live = axum::body::Body::from_stream(live.bytes_stream()).into_data_stream();
        let started = Instant::now();
        let probe = probe_response(&mut live, Duration::from_millis(150), 2_000_000).await;
        assert!(!probe.ended);
        assert_eq!(probe.flaky, None);
        assert!(!probe.buffered.is_empty(), "buffered bytes must be replayable");
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn classifies_empty_repetition_tools_and_reasoning() {
        assert_eq!(
            detect_flaky_response(&json!({"choices":[{"message":{"content":""}}]})),
            Some("empty")
        );
        assert_eq!(
            detect_flaky_response(
                &json!({"choices":[{"message":{"content":"abcabcabcabcabcabcabcabc"}}]})
            ),
            Some("degenerate")
        );
        assert_eq!(
            detect_flaky_response(&json!({"choices":[{"message":{"tool_calls":[{}]}}]})),
            None
        );
        assert_eq!(
            detect_flaky_response(
                &json!({"type":"message","content":[{"type":"thinking","thinking":"yes"}]})
            ),
            None
        );
        assert_eq!(
            resolve_anti_flaky_config(&json!({"antiFlakyMaxRetries":99})),
            (true, 5)
        );
    }
}
