use serde_json::Value;

pub fn get_openai_responses_event_name<'a>(
    event_name: Option<&'a str>,
    chunk: &'a Value,
) -> Option<&'a str> {
    event_name
        .filter(|s| !s.is_empty())
        .or_else(|| chunk["type"].as_str())
}

pub fn is_openai_responses_terminal_event(event_name: Option<&str>, chunk: &Value) -> bool {
    matches!(
        get_openai_responses_event_name(event_name, chunk),
        Some("response.completed" | "response.done" | "response.failed" | "error")
    ) || matches!(
        chunk["response"]["status"].as_str(),
        Some("completed" | "failed")
    )
}

pub fn format_incomplete_openai_responses_stream_failure(now_ms: u64) -> String {
    let data = serde_json::json!({"type":"response.failed","response":{"id":format!("resp_{now_ms}"),"status":"failed","error":{"type":"stream_error","code":"stream_disconnected","message":"stream closed before response.completed"}}});
    format!("event: response.failed\ndata: {data}\n\n")
}

pub fn build_aborted_responses_terminal_bytes(now_ms: u64) -> Vec<u8> {
    format!(
        "{}data: [DONE]\n\n",
        format_incomplete_openai_responses_stream_failure(now_ms)
    )
    .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn detects_and_synthesizes_terminal_events() {
        assert!(is_openai_responses_terminal_event(
            None,
            &json!({"type":"response.completed"})
        ));
        assert!(is_openai_responses_terminal_event(
            None,
            &json!({"response":{"status":"failed"}})
        ));
        assert!(!is_openai_responses_terminal_event(
            None,
            &json!({"type":"response.output_text.delta"})
        ));
        let s = String::from_utf8(build_aborted_responses_terminal_bytes(42)).unwrap();
        assert!(s.contains("resp_42"));
        assert!(s.ends_with("data: [DONE]\n\n"));
    }
}
