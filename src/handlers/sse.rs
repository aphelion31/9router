use axum::body::{Body, Bytes};
use futures_core::Stream;
use serde_json::{Value, json};
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::sync::mpsc;

use crate::translator::formats::{
    FORMAT_CLAUDE, FORMAT_OPENAI_RESPONSE, FORMAT_OPENAI_RESPONSES,
};
use crate::translator::{init_state, translate_response};

pub const SSE_DONE: &str = "data: [DONE]\n\n";

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
        return serde_json::from_str::<Value>(trimmed).ok().map(SseLine::Json);
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
    let json_str = serde_json::to_string(data).unwrap_or_else(|_| "null".to_string());
    if (source_format == FORMAT_CLAUDE
        || source_format == FORMAT_OPENAI_RESPONSES
        || source_format == FORMAT_OPENAI_RESPONSE)
        && let Some(ev_type) = data.get("type").and_then(|v| v.as_str())
    {
        return format!("event: {ev_type}\ndata: {json_str}\n\n");
    }
    format!("data: {json_str}\n\n")
}

pub fn format_sse_error(message: &str, source_format: &str) -> String {
    let err = json!({ "error": { "message": message } });
    if source_format == FORMAT_CLAUDE {
        let ev = json!({ "type": "error", "error": { "message": message } });
        format_sse(&ev, source_format)
    } else {
        format!("{}{}", format_sse(&err, source_format), SSE_DONE)
    }
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

        let flush_terminal = |state: &mut crate::translator::TranslateState,
                              done_sent: &mut bool|
         -> Vec<Bytes> {
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

        while let Some(chunk_res) = upstream_rx.recv().await {
            match chunk_res {
                Ok(bytes) => {
                    raw_buf.extend_from_slice(&bytes);
                    while let Some(pos) = raw_buf.iter().position(|&b| b == b'\n') {
                        let line_bytes: Vec<u8> = raw_buf.drain(..=pos).collect();
                        let line = String::from_utf8_lossy(&line_bytes);
                        if let Some(parsed) = parse_sse_line(&line) {
                            match parsed {
                                SseLine::Done => {
                                    for frame in flush_terminal(&mut state, &mut done_sent) {
                                        if tx.send(Ok(frame)).await.is_err() {
                                            return;
                                        }
                                    }
                                    return;
                                }
                                SseLine::Json(val) => {
                                    let translated = translate_response(
                                        &target_format,
                                        &source_format,
                                        Some(&val),
                                        &mut state,
                                    );
                                    for item in translated {
                                        let frame =
                                            Bytes::from(format_sse(&item, &source_format));
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
                    let frame = Bytes::from(format_sse_error(&err_msg, &source_format));
                    let _ = tx.send(Ok(frame)).await;
                    return;
                }
            }
        }

        // Process any remaining buffered line without trailing newline
        if !raw_buf.is_empty() {
            let line = String::from_utf8_lossy(&raw_buf);
            if let Some(SseLine::Json(val)) = parse_sse_line(&line) {
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

        for frame in flush_terminal(&mut state, &mut done_sent) {
            if tx.send(Ok(frame)).await.is_err() {
                return;
            }
        }
    });

    Body::from_stream(MpscByteStream { rx })
}
