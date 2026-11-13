use axum::body::{Body, Bytes};
use futures_core::Stream;
use serde_json::{Value, json};
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::sync::mpsc;

use crate::translator::formats::{FORMAT_CLAUDE, FORMAT_OPENAI_RESPONSE, FORMAT_OPENAI_RESPONSES};
use crate::translator::{init_state, translate_response};

use crate::services::stream_helpers::{
    SSE_DONE, build_stream_error_bytes, ensure_stream_tool_call_ids, fix_invalid_id,
    has_valuable_content,
};

#[derive(Debug, PartialEq)]
pub enum SseLine {
    Done,
    Json(Value),
}

/// Parse a single SSE line (or raw NDJSON line).
pub fn parse_sse_line(line: &str) -> Option<SseLine> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with(':') {
        return None;
    }
    if trimmed.starts_with('{') {
        return serde_json::from_str::<Value>(trimmed)
            .ok()
            .map(SseLine::Json);
    }
    if let Some(rest) = trimmed.strip_prefix("data:") {
        let data = rest.trim();
        if data == "[DONE]" {
            return Some(SseLine::Done);
        }
        return serde_json::from_str::<Value>(data).ok().map(SseLine::Json);
    }
    None
}

/// Format a translated JSON event as an SSE frame for `source_format`.
pub fn format_sse(data: &Value, source_format: &str) -> String {
    if (source_format == FORMAT_OPENAI_RESPONSES || source_format == FORMAT_OPENAI_RESPONSE)
        && let Some(ev_type) = data.get("type").and_then(|v| v.as_str())
    {
        return crate::services::stream_helpers::format_sse(
            &json!({"event": ev_type, "data": data}),
            source_format,
        );
    }
    crate::services::stream_helpers::format_sse(data, source_format)
}

struct MpscByteStream {
    rx: mpsc::Receiver<Result<Bytes, std::io::Error>>,
}

impl Stream for MpscByteStream {
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx)
    }
}

/// Stream provider SSE bytes through `translate_response` and return an axum `Body`.
pub fn translate_sse_to_body(
    mut upstream_rx: mpsc::Receiver<Result<Vec<u8>, String>>,
    target_format: String,
    source_format: String,
) -> Body {
    let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(64);

    tokio::spawn(async move {
        let mut raw_buf: Vec<u8> = Vec::new();
        let mut state = init_state(&source_format);
        let mut done_sent = false;
        let mut seen_tool_call_ids = std::collections::HashMap::new();
        let responses_passthrough = matches!(
            source_format.as_str(),
            FORMAT_OPENAI_RESPONSES | FORMAT_OPENAI_RESPONSE
        ) && target_format == source_format;
        let mut responses_terminal = false;
        let mut event_name = String::new();

        let flush_terminal =
            |state: &mut crate::translator::TranslateState, done_sent: &mut bool| -> Vec<Bytes> {
                let mut frames = Vec::new();
                let tail = translate_response(&target_format, &source_format, None, state);
                for item in tail {
                    frames.push(Bytes::from(format_sse(&item, &source_format)));
                }
                if !*done_sent
                    && source_format != FORMAT_CLAUDE
                    && source_format != FORMAT_OPENAI_RESPONSES
                    && source_format != FORMAT_OPENAI_RESPONSE
                {
                    frames.push(Bytes::from_static(SSE_DONE.as_bytes()));
                    *done_sent = true;
                }
                frames
            };

        let mut first_chunk = true;
        loop {
            let timeout = if first_chunk { crate::config::STREAM_FIRST_CHUNK_TIMEOUT_MS } else { crate::config::STREAM_STALL_TIMEOUT_MS };
            let chunk_res = match tokio::time::timeout(std::time::Duration::from_millis(timeout), upstream_rx.recv()).await {
                Ok(Some(chunk)) => chunk,
                Ok(None) => break,
                Err(_) => Err("Stream stalled".to_owned()),
            };
            first_chunk = false;
            match chunk_res {
                Ok(bytes) => {
                    raw_buf.extend_from_slice(&bytes);
                    while let Some(pos) = raw_buf.iter().position(|&b| b == b'\n') {
                        let line_bytes: Vec<u8> = raw_buf.drain(..=pos).collect();
                        let line = String::from_utf8_lossy(&line_bytes);
                        if let Some(event) = line.trim().strip_prefix("event:") {
                            event_name = event.trim().to_owned();
                        }
                        if let Some(parsed) = parse_sse_line(&line) {
                            match parsed {
                                SseLine::Done => {
                                    if responses_passthrough && !responses_terminal {
                                        let now = std::time::SystemTime::now()
                                            .duration_since(std::time::UNIX_EPOCH)
                                            .unwrap_or_default()
                                            .as_millis()
                                            as u64;
                                        let _ = tx.send(Ok(Bytes::from(crate::services::responses_stream_helpers::build_aborted_responses_terminal_bytes(now)))).await;
                                        return;
                                    }
                                    for frame in flush_terminal(&mut state, &mut done_sent) {
                                        if tx.send(Ok(frame)).await.is_err() {
                                            return;
                                        }
                                    }
                                    return;
                                }
                                SseLine::Json(mut val) => {
                                    if responses_passthrough {
                                        responses_terminal |= crate::services::responses_stream_helpers::is_openai_responses_terminal_event(Some(&event_name), &val);
                                        event_name.clear();
                                    }
                                    if target_format == "openai" && source_format == "openai" {
                                        fix_invalid_id(&mut val);
                                        ensure_stream_tool_call_ids(
                                            &mut val,
                                            &mut seen_tool_call_ids,
                                        );
                                        if !has_valuable_content(&val, "openai") {
                                            continue;
                                        }
                                    }
                                    let translated = translate_response(
                                        &target_format,
                                        &source_format,
                                        Some(&val),
                                        &mut state,
                                    );
                                    for item in translated {
                                        let frame = Bytes::from(format_sse(&item, &source_format));
                                        if tx.send(Ok(frame)).await.is_err() {
                                            return;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                Err(err_msg) => {
                    let frame = if responses_passthrough {
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64;
                        Bytes::from(crate::services::responses_stream_helpers::build_aborted_responses_terminal_bytes(now))
                    } else {
                        Bytes::from(build_stream_error_bytes(502, &err_msg, &source_format))
                    };
                    let _ = tx.send(Ok(frame)).await;
                    return;
                }
            }
        }

        // Process any remaining buffered line without trailing newline
        if !raw_buf.is_empty() {
            let line = String::from_utf8_lossy(&raw_buf);
            if let Some(SseLine::Json(val)) = parse_sse_line(&line) {
                if responses_passthrough {
                    responses_terminal |= crate::services::responses_stream_helpers::is_openai_responses_terminal_event(Some(&event_name), &val);
                }
                let translated =
                    translate_response(&target_format, &source_format, Some(&val), &mut state);
                for item in translated {
                    let frame = Bytes::from(format_sse(&item, &source_format));
                    if tx.send(Ok(frame)).await.is_err() {
                        return;
                    }
                }
            }
        }

        if responses_passthrough && !responses_terminal {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            let _ = tx.send(Ok(Bytes::from(crate::services::responses_stream_helpers::format_incomplete_openai_responses_stream_failure(now)))).await;
            return;
        }
        for frame in flush_terminal(&mut state, &mut done_sent) {
            if tx.send(Ok(frame)).await.is_err() {
                return;
            }
        }
    });

    Body::from_stream(MpscByteStream { rx })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn incomplete_responses_stream_emits_failure() {
        let (tx, rx) = mpsc::channel(1);
        tx.send(Ok(b"event: response.created\ndata: {\"type\":\"response.created\"}\n\n".to_vec())).await.unwrap();
        drop(tx);
        let body = translate_sse_to_body(rx, FORMAT_OPENAI_RESPONSES.into(), FORMAT_OPENAI_RESPONSES.into());
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        let output = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(output.contains("event: response.failed"));
        assert!(output.contains("stream_disconnected"));
    }
}
