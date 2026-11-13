#![allow(dead_code)]

use serde_json::Value;

use crate::translator::gemini::gemini_to_openai_request;
use crate::translator::schema::role;

/// Antigravity -> OpenAI request translator.
/// Unwraps Antigravity envelope `{ request: ... }` if present, delegates to `gemini_to_openai_request`,
/// and ensures `role: "tool"` messages have string `content`.
pub fn antigravity_to_openai_request(
    model: &str,
    body: &Value,
    stream: bool,
    credentials: Option<&Value>,
) -> Value {
    let inner = body.get("request").filter(|r| r.is_object()).unwrap_or(body);
    let mut openai_body = gemini_to_openai_request(model, inner, stream, credentials);

    if let Some(msgs) = openai_body.get_mut("messages").and_then(|v| v.as_array_mut()) {
        for msg in msgs.iter_mut() {
            if msg.get("role").and_then(|r| r.as_str()) == Some(role::TOOL) {
                if let Some(c) = msg.get("content").cloned() {
                    if !c.is_string() {
                        let s = if let Some(res) = c.get("result").and_then(|r| r.as_str()) {
                            res.to_string()
                        } else {
                            serde_json::to_string(&c).unwrap_or_default()
                        };
                        if let Some(obj) = msg.as_object_mut() {
                            obj.insert("content".to_string(), Value::String(s));
                        }
                    }
                }
            }
        }
    }

    openai_body
}
