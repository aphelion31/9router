use super::stream_helpers::parse_sse_line;
use serde_json::Value;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct StreamState {
    pub saw_content: bool,
    pub finish_reason: Option<String>,
    pub pending: String,
}

fn nonempty(v: &Value) -> bool {
    v.as_str().is_some_and(|s| !s.is_empty())
}

pub fn delta_has_client_content(delta: &Value) -> bool {
    nonempty(&delta["content"])
        || delta["content"].as_array().is_some_and(|v| !v.is_empty())
        || delta["tool_calls"]
            .as_array()
            .is_some_and(|v| !v.is_empty())
        || nonempty(&delta["output_text"])
}

pub fn inspect_sse_line(line: &str) -> (bool, Option<String>) {
    let Some(value) = parse_sse_line(line.trim_start()) else {
        return (false, None);
    };
    if value["done"] == true {
        return (false, None);
    }
    if let Some(kind) = value["type"]
        .as_str()
        .filter(|_| !value["choices"].is_array())
    {
        let mut finish = None;
        let content = match kind {
            "content_block_delta" => match value["delta"]["type"].as_str() {
                Some("text_delta") => nonempty(&value["delta"]["text"]),
                Some("input_json_delta") => nonempty(&value["delta"]["partial_json"]),
                _ => false,
            },
            "content_block_start" => {
                value["content_block"]["type"] == "tool_use"
                    || (value["content_block"]["type"] == "text"
                        && nonempty(&value["content_block"]["text"]))
            }
            "message_delta" => {
                finish = value["delta"]["stop_reason"].as_str().map(|s| {
                    match s {
                        "refusal" => "content_filter",
                        "end_turn" => "stop",
                        "max_tokens" => "length",
                        "tool_use" => "tool_calls",
                        other => other,
                    }
                    .to_owned()
                });
                false
            }
            "response.output_text.delta"
            | "response.function_call_arguments.delta"
            | "response.custom_tool_call_input.delta" => nonempty(&value["delta"]),
            "response.output_text.done" => nonempty(&value["text"]),
            "response.output_item.added" | "response.output_item.done" => matches!(
                value["item"]["type"].as_str(),
                Some("function_call" | "custom_tool_call")
            ),
            "response.completed" => {
                finish = Some("stop".into());
                false
            }
            "response.failed" => {
                finish = Some("failed".into());
                false
            }
            "response.incomplete" => {
                finish = Some(
                    match value["response"]["incomplete_details"]["reason"].as_str() {
                        Some("content_filter") => "content_filter",
                        Some("max_output_tokens") => "length",
                        _ => "incomplete",
                    }
                    .into(),
                );
                false
            }
            _ => false,
        };
        return (content, finish);
    }
    let mut content = false;
    let mut finish = None;
    if let Some(choices) = value["choices"].as_array() {
        for choice in choices {
            let delta = choice
                .get("delta")
                .filter(|v| !v.is_null())
                .or_else(|| choice.get("message"))
                .unwrap_or(&Value::Null);
            content |= delta_has_client_content(delta);
            if let Some(reason) = choice["finish_reason"].as_str().filter(|s| !s.is_empty()) {
                finish = Some(reason.into());
            }
        }
    }
    (content, finish)
}

pub fn scan_sse_buffer(text: &str, state: &mut StreamState) {
    let mut lines = text.split('\n').peekable();
    while let Some(line) = lines.next() {
        if lines.peek().is_none() {
            state.pending = line.to_owned();
            break;
        }
        let (content, finish) = inspect_sse_line(line);
        state.saw_content |= content;
        if finish.is_some() {
            state.finish_reason = finish;
        }
    }
}

pub fn is_no_retry_finish(reason: &str) -> bool {
    reason.eq_ignore_ascii_case("content_filter")
}

#[derive(Debug, PartialEq, Eq)]
pub enum PeekOutcome {
    // Content seen; `head` chunks must be replayed before the rest of the stream.
    Content { head_chunks: usize },
    // Stream ended with no content frame; caller may fail over.
    Empty { finish_reason: Option<String> },
}

// Peek buffered upstream chunks in arrival order, stopping at the first content
// frame. Mirrors peekStreamForContent: partial lines carry across chunk boundaries
// and the trailing unterminated line is flushed at end of stream. Deadline and
// reader handoff are the caller's job (async); this is the pure decision.
pub fn peek_chunks(chunks: &[&[u8]]) -> PeekOutcome {
    let mut decoder = String::new();
    let mut state = StreamState::default();
    for (i, chunk) in chunks.iter().enumerate() {
        decoder.push_str(&String::from_utf8_lossy(chunk));
        scan_sse_buffer(&decoder, &mut state);
        decoder = std::mem::take(&mut state.pending);
        if state.saw_content {
            return PeekOutcome::Content { head_chunks: i + 1 };
        }
    }
    if !decoder.is_empty() {
        let mut tail = StreamState::default();
        scan_sse_buffer(&format!("{decoder}\n"), &mut tail);
        state.saw_content |= tail.saw_content;
        state.finish_reason = tail.finish_reason.or(state.finish_reason);
    }
    if state.saw_content {
        PeekOutcome::Content {
            head_chunks: chunks.len(),
        }
    } else {
        PeekOutcome::Empty {
            finish_reason: state.finish_reason,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn peek_stops_at_first_content_across_chunk_boundary() {
        let a = b"data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\ndata: {\"choi";
        let b = b"ces\":[{\"delta\":{\"content\":\"hi\"}}]}\n";
        assert_eq!(
            peek_chunks(&[a, b]),
            PeekOutcome::Content { head_chunks: 2 }
        );
    }
    #[test]
    fn peek_reports_empty_with_finish_reason() {
        let a = b"data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"},\"finish_reason\":\"content_filter\"}]}\n\ndata: [DONE]\n";
        assert_eq!(
            peek_chunks(&[a]),
            PeekOutcome::Empty {
                finish_reason: Some("content_filter".into())
            }
        );
    }
    #[test]
    fn peek_flushes_unterminated_final_line() {
        let a = b"data: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}";
        assert_eq!(
            peek_chunks(&[a]),
            PeekOutcome::Content { head_chunks: 1 }
        );
    }
    #[test]
    fn content_and_reasoning_are_distinct() {
        assert!(!delta_has_client_content(
            &json!({"role":"assistant","reasoning_content":"thinking"})
        ));
        assert!(delta_has_client_content(&json!({"tool_calls":[{}]})));
        assert!(
            inspect_sse_line(
                "data: {\"type\":\"content_block_start\",\"content_block\":{\"type\":\"tool_use\"}}"
            )
            .0
        );
        assert_eq!(inspect_sse_line("data: {\"type\":\"response.incomplete\",\"response\":{\"incomplete_details\":{\"reason\":\"content_filter\"}}}").1.as_deref(), Some("content_filter"));
        assert!(is_no_retry_finish("CONTENT_FILTER"));
        assert!(!is_no_retry_finish("stop"));
    }
    #[test]
    fn scan_retains_partial_frame() {
        let mut state = StreamState::default();
        scan_sse_buffer(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\ndata: {",
            &mut state,
        );
        assert!(state.saw_content);
        assert_eq!(state.pending, "data: {");
    }
}
