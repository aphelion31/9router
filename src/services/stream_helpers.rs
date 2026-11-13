use serde_json::Value;

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
    let trimmed = line.trim();
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

#[cfg(test)]
mod tests {
    use super::*;
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
