#![allow(dead_code)]

use serde_json::{json, Map, Value};
use std::collections::HashMap;

use crate::translator::schema::{claude_block, openai_block, role};
use crate::translator::TranslateState;

fn extract_content(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(arr) => arr
            .iter()
            .filter_map(|p| {
                if p.get("type").and_then(|t| t.as_str()) == Some(openai_block::TEXT) {
                    p.get("text").and_then(|t| t.as_str())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

fn sanitize_tool_result_text(text: &str) -> String {
    text.chars()
        .filter(|&c| {
            let code = c as u32;
            !((code <= 0x08) || code == 0x0B || code == 0x0C || (0x0E..=0x1F).contains(&code) || code == 0x7F)
        })
        .collect()
}

fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn build_tool_result_block(tool_name: &str, tool_call_id: &str, result_text: &str) -> String {
    let clean = sanitize_tool_result_text(result_text);
    format!(
        "<tool_result>\n<tool_name>{}</tool_name>\n<tool_call_id>{}</tool_call_id>\n<result>{}</result>\n</tool_result>",
        escape_xml(if tool_name.is_empty() { "tool" } else { tool_name }),
        escape_xml(tool_call_id),
        escape_xml(&clean)
    )
}

fn normalize_tool_call_id(id: &str) -> &str {
    id.split('\n').next().unwrap_or("")
}

fn convert_messages(messages: &[Value]) -> Vec<Value> {
    let mut tool_meta: HashMap<String, String> = HashMap::new();
    let mut remember = |id: &str, name: &str| {
        if id.is_empty() {
            return;
        }
        let n = if name.is_empty() { "tool" } else { name }.to_string();
        tool_meta.insert(id.to_string(), n.clone());
        let norm = normalize_tool_call_id(id);
        if !norm.is_empty() && norm != id {
            tool_meta.insert(norm.to_string(), n);
        }
    };

    for msg in messages {
        if msg.get("role").and_then(|r| r.as_str()) == Some(role::ASSISTANT) {
            if let Some(tcs) = msg.get("tool_calls").and_then(|v| v.as_array()) {
                for tc in tcs {
                    let id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    let name = tc
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("tool");
                    remember(id, name);
                }
            }
            if let Some(parts) = msg.get("content").and_then(|v| v.as_array()) {
                for p in parts {
                    if p.get("type").and_then(|v| v.as_str()) == Some(claude_block::TOOL_USE) {
                        let id = p.get("id").and_then(|v| v.as_str()).unwrap_or("");
                        let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("tool");
                        remember(id, name);
                    }
                }
            }
        }
    }

    let mut result = Vec::new();
    for msg in messages {
        let r = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
        if r == role::SYSTEM {
            let txt = extract_content(msg.get("content").unwrap_or(&Value::Null));
            result.push(json!({
                "role": role::USER,
                "content": format!("[System Instructions]\n{}", txt),
            }));
            continue;
        }

        if r == role::TOOL {
            let tool_content = extract_content(msg.get("content").unwrap_or(&Value::Null));
            let tc_id = msg.get("tool_call_id").and_then(|v| v.as_str()).unwrap_or("");
            let tool_name = msg
                .get("name")
                .and_then(|v| v.as_str())
                .or_else(|| tool_meta.get(tc_id).map(|s| s.as_str()))
                .unwrap_or("tool");
            result.push(json!({
                "role": role::USER,
                "content": build_tool_result_block(tool_name, tc_id, &tool_content),
            }));
            continue;
        }

        if r == role::USER || r == role::ASSISTANT {
            if r == role::USER {
                if let Some(arr) = msg.get("content").and_then(|v| v.as_array()) {
                    let mut parts = Vec::new();
                    for block in arr {
                        let bt = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
                        if bt == claude_block::TEXT {
                            if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                                if !t.is_empty() {
                                    parts.push(t.to_string());
                                }
                            }
                        } else if bt == claude_block::TOOL_RESULT {
                            let tc_id = block
                                .get("tool_use_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            let tool_name = tool_meta
                                .get(tc_id)
                                .or_else(|| tool_meta.get(normalize_tool_call_id(tc_id)))
                                .map(|s| s.as_str())
                                .unwrap_or("tool");
                            let tc_txt =
                                extract_content(block.get("content").unwrap_or(&Value::Null));
                            parts.push(build_tool_result_block(tool_name, tc_id, &tc_txt));
                        }
                    }
                    let joined = parts.join("\n");
                    if !joined.is_empty() {
                        result.push(json!({ "role": role::USER, "content": joined }));
                    }
                    continue;
                }
            }

            let content = extract_content(msg.get("content").unwrap_or(&Value::Null));
            if r == role::ASSISTANT {
                if let Some(tcs) = msg
                    .get("tool_calls")
                    .and_then(|v| v.as_array())
                    .filter(|a| !a.is_empty())
                {
                    let cleaned: Vec<Value> = tcs
                        .iter()
                        .map(|tc| {
                            let mut obj = tc.as_object().cloned().unwrap_or_default();
                            obj.remove("index");
                            Value::Object(obj)
                        })
                        .collect();
                    result.push(json!({
                        "role": role::ASSISTANT,
                        "content": content,
                        "tool_calls": cleaned,
                    }));
                    continue;
                }
                if let Some(arr) = msg.get("content").and_then(|v| v.as_array()) {
                    let extracted: Vec<Value> = arr
                        .iter()
                        .filter(|b| {
                            b.get("type").and_then(|v| v.as_str()) == Some(claude_block::TOOL_USE)
                        })
                        .filter_map(|b| {
                            let id = b.get("id").and_then(|v| v.as_str()).unwrap_or("");
                            if id.is_empty() {
                                return None;
                            }
                            let name = b.get("name").and_then(|v| v.as_str()).unwrap_or("tool");
                            let input = b.get("input").cloned().unwrap_or_else(|| json!({}));
                            Some(json!({
                                "id": id,
                                "type": openai_block::FUNCTION,
                                "function": {
                                    "name": name,
                                    "arguments": serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_string()),
                                }
                            }))
                        })
                        .collect();
                    if !extracted.is_empty() {
                        result.push(json!({
                            "role": role::ASSISTANT,
                            "content": content,
                            "tool_calls": extracted,
                        }));
                        continue;
                    }
                }
            }

            if !content.is_empty() {
                result.push(json!({ "role": r, "content": content }));
            }
        }
    }
    result
}

pub fn openai_to_cursor_request(
    _model: &str,
    body: &Value,
    _stream: bool,
    _credentials: Option<&Value>,
) -> Value {
    let msgs = body
        .get("messages")
        .and_then(|v| v.as_array())
        .map(|a| convert_messages(a))
        .unwrap_or_default();

    let mut out = Map::new();
    if let Some(obj) = body.as_object() {
        for (k, v) in obj {
            if matches!(
                k.as_str(),
                "user" | "metadata" | "tool_choice" | "stream_options" | "system"
            ) {
                continue;
            }
            out.insert(k.clone(), v.clone());
        }
    }
    out.insert("messages".to_string(), Value::Array(msgs));
    out.insert("max_tokens".to_string(), json!(32000));
    Value::Object(out)
}

pub fn cursor_to_openai_response(chunk: Option<&Value>, _state: &mut TranslateState) -> Vec<Value> {
    match chunk {
        Some(c) if !c.is_null() => vec![c.clone()],
        _ => Vec::new(),
    }
}
