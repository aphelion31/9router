#![allow(dead_code)]

use serde_json::Value;

/// Strip unsupported modalities from OpenAI `messages` (`strip_list` e.g. `["image", "audio"]`).
pub fn strip_content_types(body: &mut Value, strip_list: &[&str]) {
    if strip_list.is_empty() {
        return;
    }
    let strip_image = strip_list.contains(&"image");
    let strip_audio = strip_list.contains(&"audio");
    if !strip_image && !strip_audio {
        return;
    }

    let Some(msgs) = body.get_mut("messages").and_then(|m| m.as_array_mut()) else {
        return;
    };

    for msg in msgs.iter_mut() {
        let Some(arr) = msg.get_mut("content").and_then(|c| c.as_array_mut()) else {
            continue;
        };
        arr.retain(|part| {
            let t = part.get("type").and_then(|v| v.as_str()).unwrap_or("");
            if strip_image && (t == "image_url" || t == "image") {
                return false;
            }
            if strip_audio && (t == "audio_url" || t == "input_audio") {
                return false;
            }
            true
        });
        if arr.is_empty() {
            if let Some(obj) = msg.as_object_mut() {
                obj.insert("content".to_string(), Value::String(String::new()));
            }
        }
    }
}

/// Remove unsupported top-level parameters from a request body.
pub fn filter_unsupported_params(body: &mut Value, unsupported: &[&str]) {
    if unsupported.is_empty() {
        return;
    }
    if let Some(obj) = body.as_object_mut() {
        for key in unsupported {
            obj.remove(*key);
        }
    }
}

/// Extract plain text from a Claude message `content` (string or array of blocks).
pub fn extract_claude_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(arr) => arr
            .iter()
            .filter_map(|b| {
                if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                    b.get("text").and_then(|t| t.as_str()).map(|s| s.to_string())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Returns true when a Claude message `content` has no non-empty text, tool_use, tool_result, or image blocks.
pub fn is_claude_empty_content(content: &Value) -> bool {
    match content {
        Value::Null => true,
        Value::String(s) => s.trim().is_empty(),
        Value::Array(arr) => arr.iter().all(|b| {
            let t = b.get("type").and_then(|v| v.as_str()).unwrap_or("");
            if t == "text" {
                b.get("text")
                    .and_then(|v| v.as_str())
                    .map(|s| s.trim().is_empty())
                    .unwrap_or(true)
            } else {
                false
            }
        }),
        _ => false,
    }
}
