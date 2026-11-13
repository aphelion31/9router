//! Port of open-sse/utils/streamHelpers.js (+ buildErrorBody from utils/error.js).
use serde_json::{Map, Value, json};
use std::collections::HashMap;

pub const SSE_DONE: &str = "data: [DONE]\n\n";

pub fn sse_chunk(data: &Value) -> String {
    format!("data: {data}\n\n")
}

pub fn chat_chunk_sse(
    id: &str,
    created: i64,
    model: &str,
    delta: Value,
    finish_reason: Option<&str>,
) -> String {
    // serde_json maps sort keys, so construct this frame in upstream's declared order.
    format!(
        "data: {{\"id\":{},\"object\":\"chat.completion.chunk\",\"created\":{},\"model\":{},\"choices\":[{{\"index\":0,\"delta\":{},\"finish_reason\":{}}}]}}\n\n",
        serde_json::json!(id),
        created,
        serde_json::json!(model),
        delta,
        serde_json::json!(finish_reason)
    )
}

pub fn parse_sse_line(line: &str) -> Option<Value> {
    parse_sse_line_fmt(line, None)
}

/// `format == Some("ollama")`: NDJSON only (no `data:` frames), like upstream.
pub fn parse_sse_line_fmt(line: &str, format: Option<&str>) -> Option<Value> {
    if line.is_empty() {
        return None;
    }
    let trimmed = line.trim();
    if format == Some("ollama") {
        return trimmed.starts_with('{').then(|| serde_json::from_str(trimmed).ok()).flatten();
    }
    if trimmed.starts_with('{') {
        return serde_json::from_str(trimmed).ok();
    }
    if !line.starts_with('d') || line.len() < 5 || !line.is_char_boundary(5) {
        return None;
    }
    let data = line[5..].trim();
    if data == "[DONE]" {
        return Some(serde_json::json!({"done": true}));
    }
    serde_json::from_str(data).ok()
}

fn non_empty_str(v: &Value) -> bool {
    v.as_str().is_some_and(|s| !s.is_empty())
}

/// Whether a parsed chunk carries anything worth forwarding.
pub fn has_valuable_content(chunk: &Value, format: &str) -> bool {
    if format == "openai"
        && let Some(delta) = chunk.pointer("/choices/0/delta").filter(|d| d.is_object())
    {
        return non_empty_str(&delta["content"])
            || non_empty_str(&delta["reasoning_content"])
            || delta["tool_calls"].as_array().is_some_and(|a| !a.is_empty())
            || chunk.pointer("/choices/0/finish_reason").is_some_and(|f| !f.is_null() && f != false)
            || delta.get("role").is_some_and(|r| !r.is_null() && r != "");
    }
    if format == "claude" {
        let block_delta = chunk["type"] == "content_block_delta";
        let d = &chunk["delta"];
        if block_delta
            && !non_empty_str(&d["text"])
            && !non_empty_str(&d["thinking"])
            && !non_empty_str(&d["partial_json"])
        {
            return false;
        }
    }
    true
}

/// Give every streamed tool call an id, stable per index. Returns true if modified.
pub fn ensure_stream_tool_call_ids(chunk: &mut Value, seen: &mut HashMap<i64, String>) -> bool {
    let Some(choices) = chunk.get_mut("choices").and_then(Value::as_array_mut) else {
        return false;
    };
    let mut changed = false;
    for choice in choices {
        let Some(calls) = choice.pointer_mut("/delta/tool_calls").and_then(Value::as_array_mut) else {
            continue;
        };
        for tc in calls.iter_mut().filter(|t| t.is_object()) {
            let index = tc["index"].as_i64();
            if let Some(id) = tc["id"].as_str().filter(|s| !s.is_empty()) {
                if let Some(i) = index {
                    seen.insert(i, id.to_owned());
                }
                continue;
            }
            let id = index
                .and_then(|i| seen.get(&i).cloned())
                .unwrap_or_else(|| match index {
                    Some(i) => crate::translator::concerns::fallback_tool_call_id(i.max(0) as usize),
                    None => format!("call_{}", now_millis()),
                });
            if let Some(i) = index {
                seen.insert(i, id.clone());
            }
            tc["id"] = json!(id);
            changed = true;
        }
    }
    changed
}

fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn base36(mut n: u128) -> String {
    const D: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    loop {
        out.push(D[(n % 36) as usize]);
        n /= 36;
        if n == 0 {
            break;
        }
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

/// Replace generic or too-short ids ("chat", "completion", < 8 chars). Returns true if fixed.
pub fn fix_invalid_id(parsed: &mut Value) -> bool {
    let Some(id) = parsed["id"].as_str() else {
        return false;
    };
    if id.is_empty() || !(id == "chat" || id == "completion" || id.chars().count() < 8) {
        return false;
    }
    let fallback = parsed
        .pointer("/extend_fields/requestId")
        .or_else(|| parsed.pointer("/extend_fields/traceId"))
        .and_then(|v| v.as_str().map(str::to_owned).or_else(|| (!v.is_null()).then(|| v.to_string())))
        .unwrap_or_else(|| base36(now_millis()));
    parsed["id"] = json!(format!("chatcmpl-{fallback}"));
    true
}

/// Drop `usage: null` and `usage.perf_metrics: null`, also inside `response`.
fn clean_usage_payload(payload: &Value) -> Value {
    let Some(obj) = payload.as_object() else {
        return payload.clone();
    };
    let mut out: Map<String, Value> = obj.clone();
    match out.get("usage") {
        Some(Value::Null) => {
            out.remove("usage");
        }
        Some(Value::Object(u)) if u.get("perf_metrics") == Some(&Value::Null) => {
            let mut u = u.clone();
            u.remove("perf_metrics");
            out.insert("usage".into(), Value::Object(u));
        }
        _ => {}
    }
    if let Some(resp) = out.get("response").filter(|r| r.is_object()) {
        let cleaned = clean_usage_payload(resp);
        out.insert("response".into(), cleaned);
    }
    Value::Object(out)
}

/// Serialize one frame for the client's format (Responses events, Claude events, or plain data).
pub fn format_sse(data: &Value, source_format: &str) -> String {
    if data.is_null() {
        return "data: null\n\n".into();
    }
    if data["done"].as_bool() == Some(true) {
        return SSE_DONE.into();
    }
    if let (Some(event), Some(inner)) = (data["event"].as_str(), data.get("data").filter(|d| !d.is_null())) {
        return format!("event: {event}\ndata: {}\n\n", clean_usage_payload(inner));
    }
    let data = clean_usage_payload(data);
    if source_format == "claude"
        && let Some(t) = data["type"].as_str()
    {
        return format!("event: {t}\ndata: {data}\n\n");
    }
    format!("data: {data}\n\n")
}

/// OpenAI-compatible error body with upstream's type/code table.
pub fn build_error_body(status: u16, message: &str) -> Value {
    let (typ, code) = match status {
        400 => ("invalid_request_error", "bad_request"),
        401 => ("authentication_error", "invalid_api_key"),
        402 => ("billing_error", "payment_required"),
        403 => ("permission_error", "insufficient_quota"),
        404 => ("invalid_request_error", "model_not_found"),
        406 => ("invalid_request_error", "model_not_supported"),
        429 => ("rate_limit_error", "rate_limit_exceeded"),
        500 => ("server_error", "internal_server_error"),
        502 => ("server_error", "bad_gateway"),
        503 => ("server_error", "service_unavailable"),
        504 => ("server_error", "gateway_timeout"),
        s if s >= 500 => ("server_error", "internal_server_error"),
        _ => ("invalid_request_error", ""),
    };
    let default = match status {
        400 => "Bad request",
        401 => "Invalid API key provided",
        402 => "Payment required",
        403 => "You exceeded your current quota",
        404 => "Model not found",
        406 => "Model not supported",
        429 => "Rate limit exceeded",
        500 => "Internal server error",
        502 => "Bad gateway - upstream provider error",
        503 => "Service temporarily unavailable",
        504 => "Gateway timeout",
        _ => "An error occurred",
    };
    let message = if message.is_empty() { default } else { message };
    json!({"error": {"message": message, "type": typ, "code": code}})
}

/// Terminal frames for a stream that failed after 200 was sent: Claude gets `event: error`,
/// everyone else gets an error data frame followed by `[DONE]`.
pub fn build_stream_error_bytes(status: u16, message: &str, client_format: &str) -> Vec<u8> {
    let error = build_error_body(status, message)["error"].clone();
    let sse = if client_format == "claude" {
        format_sse(&json!({"type": "error", "error": error}), "claude")
    } else {
        format_sse(&json!({"error": error}), client_format) + SSE_DONE
    };
    sse.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ollama_ndjson_only() {
        assert_eq!(parse_sse_line_fmt("{\"a\":1}", Some("ollama")).unwrap()["a"], 1);
        assert!(parse_sse_line_fmt("data: {\"a\":1}", Some("ollama")).is_none());
    }

    #[test]
    fn valuable_content() {
        assert!(!has_valuable_content(&json!({"choices":[{"delta":{"content":""}}]}), "openai"));
        assert!(has_valuable_content(&json!({"choices":[{"delta":{"content":"x"}}]}), "openai"));
        assert!(has_valuable_content(&json!({"choices":[{"delta":{},"finish_reason":"stop"}]}), "openai"));
        assert!(!has_valuable_content(&json!({"type":"content_block_delta","delta":{"text":""}}), "claude"));
        assert!(has_valuable_content(&json!({"type":"message_stop"}), "claude"));
    }

    #[test]
    fn tool_call_ids_are_stable_per_index() {
        let mut seen = HashMap::new();
        let mut a = json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_x"}]}}]});
        assert!(!ensure_stream_tool_call_ids(&mut a, &mut seen));
        let mut b = json!({"choices":[{"delta":{"tool_calls":[{"index":0},{"index":1}]}}]});
        assert!(ensure_stream_tool_call_ids(&mut b, &mut seen));
        assert_eq!(b["choices"][0]["delta"]["tool_calls"][0]["id"], "call_x");
        let minted = b["choices"][0]["delta"]["tool_calls"][1]["id"].as_str().unwrap().to_owned();
        assert!(minted.starts_with("call_1_"));
        let mut c = json!({"choices":[{"delta":{"tool_calls":[{"index":1}]}}]});
        ensure_stream_tool_call_ids(&mut c, &mut seen);
        assert_eq!(c["choices"][0]["delta"]["tool_calls"][0]["id"], minted);
    }

    #[test]
    fn invalid_ids_fixed() {
        let mut v = json!({"id":"chat","extend_fields":{"requestId":"r1"}});
        assert!(fix_invalid_id(&mut v));
        assert_eq!(v["id"], "chatcmpl-r1");
        let mut ok = json!({"id":"chatcmpl-long-enough"});
        assert!(!fix_invalid_id(&mut ok));
    }

    #[test]
    fn format_sse_variants() {
        assert_eq!(format_sse(&json!({"done":true}), "openai"), SSE_DONE);
        assert_eq!(
            format_sse(&json!({"event":"response.completed","data":{"usage":null,"x":1}}), "openai-responses"),
            "event: response.completed\ndata: {\"x\":1}\n\n"
        );
        assert_eq!(
            format_sse(&json!({"type":"ping"}), "claude"),
            "event: ping\ndata: {\"type\":\"ping\"}\n\n"
        );
        assert_eq!(
            format_sse(&json!({"usage":{"perf_metrics":null,"t":2}}), "openai"),
            "data: {\"usage\":{\"t\":2}}\n\n"
        );
    }

    #[test]
    fn stream_error_frames() {
        let openai = String::from_utf8(build_stream_error_bytes(429, "", "openai")).unwrap();
        assert!(openai.contains("\"code\":\"rate_limit_exceeded\""));
        assert!(openai.ends_with(SSE_DONE));
        let claude = String::from_utf8(build_stream_error_bytes(500, "boom", "claude")).unwrap();
        assert!(claude.starts_with("event: error\n"));
        assert!(claude.contains("boom"));
    }
    #[test]
    fn formats_sse_chunks() {
        assert_eq!(
            sse_chunk(&serde_json::json!({"a":1})),
            "data: {\"a\":1}\n\n"
        );
        assert_eq!(
            chat_chunk_sse("id", 1, "model", serde_json::json!({"content":"x"}), None),
            "data: {\"id\":\"id\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"x\"},\"finish_reason\":null}]}\n\n"
        );
    }
    #[test]
    fn parses_sse_and_ndjson() {
        assert_eq!(parse_sse_line("data: [DONE]").unwrap()["done"], true);
        assert_eq!(parse_sse_line("data: {\"x\":1}").unwrap()["x"], 1);
        assert_eq!(parse_sse_line(" {\"x\":2} ").unwrap()["x"], 2);
        assert!(parse_sse_line("event: message").is_none());
        assert!(parse_sse_line("data: bad").is_none());
    }
}
