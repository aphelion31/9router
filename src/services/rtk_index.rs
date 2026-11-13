// RTK port: compress tool_result content in LLM request bodies.
// Applied in chatCore on the source-format body (cursor) or translated body (all others).

use serde_json::Value;

use crate::services::rtk_apply_filter::safe_apply;
use crate::services::rtk_autodetect::auto_detect_filter;

use crate::services::rtk_constants::{RAW_CAP, MIN_COMPRESS_SIZE};

#[derive(Debug, Clone)]
pub struct RtkHit {
    pub shape: &'static str,
    pub filter: &'static str,
    pub saved: usize,
}

#[derive(Debug, Clone, Default)]
pub struct RtkStats {
    pub bytes_before: usize,
    pub bytes_after: usize,
    pub hits: Vec<RtkHit>,
}

pub fn compress_messages(body: &mut Value, enabled: bool) -> Option<RtkStats> {
    if !enabled || !body.is_object() {
        return None;
    }

    if body.get("conversationState").is_some_and(Value::is_object) {
        return Some(compress_kiro_format(body));
    }

    let items = if body.get("messages").is_some_and(Value::is_array) {
        body.get_mut("messages").and_then(Value::as_array_mut)
    } else if body.get("input").is_some_and(Value::is_array) {
        body.get_mut("input").and_then(Value::as_array_mut)
    } else {
        None
    }?;

    let mut stats = RtkStats::default();
    for msg in items.iter_mut() {
        let Some(obj) = msg.as_object_mut() else {
            continue;
        };

        if obj.get("type").and_then(Value::as_str) == Some("function_call_output") {
            match obj.get_mut("output") {
                Some(Value::String(s)) => {
                    *s = compress_text(s, &mut stats, "openai-responses-string");
                }
                Some(Value::Array(parts)) => {
                    for part in parts.iter_mut() {
                        if part.get("type").and_then(Value::as_str) == Some("input_text")
                            && let Some(Value::String(s)) = part.get_mut("text")
                        {
                            *s = compress_text(s, &mut stats, "openai-responses-array");
                        }
                    }
                }
                _ => {}
            }
            continue;
        }

        if obj.get("role").and_then(Value::as_str) == Some("tool") {
            match obj.get_mut("content") {
                Some(Value::String(s)) => {
                    *s = compress_text(s, &mut stats, "openai-tool");
                }
                Some(Value::Array(parts)) => {
                    for part in parts.iter_mut() {
                        if part.get("type").and_then(Value::as_str) == Some("text")
                            && let Some(Value::String(s)) = part.get_mut("text")
                        {
                            *s = compress_text(s, &mut stats, "openai-tool-array");
                        }
                    }
                }
                _ => {}
            }
            continue;
        }

        if let Some(Value::Array(blocks)) = obj.get_mut("content") {
            for block in blocks.iter_mut() {
                if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                    continue;
                }
                if block.get("is_error").and_then(Value::as_bool) == Some(true) {
                    continue;
                }
                match block.get_mut("content") {
                    Some(Value::String(s)) => {
                        *s = compress_text(s, &mut stats, "claude-string");
                    }
                    Some(Value::Array(parts)) => {
                        for part in parts.iter_mut() {
                            if part.get("type").and_then(Value::as_str) == Some("text")
                                && let Some(Value::String(s)) = part.get_mut("text")
                            {
                                *s = compress_text(s, &mut stats, "claude-array");
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    Some(stats)
}

fn compress_kiro_format(body: &mut Value) -> RtkStats {
    let mut stats = RtkStats::default();
    let Some(state) = body.get_mut("conversationState").and_then(Value::as_object_mut) else {
        return stats;
    };

    if let Some(Value::Array(hist)) = state.get_mut("history") {
        for msg in hist.iter_mut() {
            compress_kiro_msg(msg, &mut stats);
        }
    }
    if let Some(curr) = state.get_mut("currentMessage") {
        compress_kiro_msg(curr, &mut stats);
    }
    stats
}

fn compress_kiro_msg(msg: &mut Value, stats: &mut RtkStats) {
    let Some(tool_results) = msg
        .get_mut("userInputMessage")
        .and_then(|u| u.get_mut("userInputMessageContext"))
        .and_then(|c| c.get_mut("toolResults"))
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    for tr in tool_results.iter_mut() {
        if tr.get("status").and_then(Value::as_str) == Some("error") {
            continue;
        }
        if let Some(Value::Array(content)) = tr.get_mut("content") {
            for part in content.iter_mut() {
                if let Some(Value::String(s)) = part.get_mut("text") {
                    *s = compress_text(s, stats, "kiro-tool-result");
                }
            }
        }
    }
}

fn compress_text(text: &str, stats: &mut RtkStats, shape: &'static str) -> String {
    let bytes_in = text.len();
    stats.bytes_before += bytes_in;

    if !(MIN_COMPRESS_SIZE..=RAW_CAP).contains(&bytes_in) {
        stats.bytes_after += bytes_in;
        return text.to_owned();
    }

    let Some(kind) = auto_detect_filter(text) else {
        stats.bytes_after += bytes_in;
        return text.to_owned();
    };

    let out = safe_apply(Some(kind.filter_fn()), kind.name(), text);
    if out.is_empty() || out.len() >= bytes_in {
        stats.bytes_after += bytes_in;
        return text.to_owned();
    }

    stats.bytes_after += out.len();
    stats.hits.push(RtkHit {
        shape,
        filter: kind.name(),
        saved: bytes_in - out.len(),
    });
    out
}

pub fn format_rtk_log(stats: &RtkStats) -> Option<String> {
    if stats.hits.is_empty() {
        return None;
    }
    let saved = stats.bytes_before.saturating_sub(stats.bytes_after);
    let pct = if stats.bytes_before > 0 {
        format!("{:.1}", (saved as f64 / stats.bytes_before as f64) * 100.0)
    } else {
        "0".to_owned()
    };
    let mut filters: Vec<&str> = Vec::new();
    for h in &stats.hits {
        if !filters.contains(&h.filter) {
            filters.push(h.filter);
        }
    }
    Some(format!(
        "[RTK] saved {saved}B / {}B ({pct}%) via [{}] hits={}",
        stats.bytes_before,
        filters.join(","),
        stats.hits.len()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn compresses_openai_and_claude_tool_results() {
        let big_log = "INFO repeated line\n".repeat(40);
        let mut body = json!({
            "messages": [
                { "role": "tool", "content": big_log.clone() },
                { "role": "user", "content": [{ "type": "tool_result", "is_error": false, "content": big_log.clone() }] },
                { "role": "user", "content": [{ "type": "tool_result", "is_error": true, "content": big_log }] }
            ]
        });
        let stats = compress_messages(&mut body, true).unwrap();
        assert_eq!(stats.hits.len(), 2);
        assert!(stats.bytes_after < stats.bytes_before);
        assert!(format_rtk_log(&stats).unwrap().contains("[RTK] saved"));
    }
}
