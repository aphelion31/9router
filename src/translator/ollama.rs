#![allow(dead_code)]

use serde_json::{json, Map, Value};
use std::collections::HashMap;

use crate::translator::concerns::{
    build_chunk, fallback_tool_call_id, now_unix_millis, now_unix_secs, parse_data_uri,
    parse_json_or_empty, to_openai_finish, to_openai_usage,
};
use crate::translator::formats::FORMAT_OLLAMA;
use crate::translator::schema::{openai_block, openai_finish, role};
use crate::translator::TranslateState;

fn normalize_content(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(arr) => arr
            .iter()
            .filter_map(|b| {
                if b.get("type").and_then(|t| t.as_str()) == Some(openai_block::TEXT) {
                    b.get("text").and_then(|t| t.as_str())
                } else {
                    None
                }
            })
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn extract_images_from_content(content: &Value) -> Vec<String> {
    let Some(arr) = content.as_array() else {
        return Vec::new();
    };
    let mut images = Vec::new();
    for block in arr {
        if block.get("type").and_then(|t| t.as_str()) != Some(openai_block::IMAGE_URL) {
            continue;
        }
        let url = block
            .get("image_url")
            .and_then(|iu| iu.as_str().or_else(|| iu.get("url").and_then(|u| u.as_str())))
            .unwrap_or("");
        if url.is_empty() {
            continue;
        }
        if let Some((_mime, data)) = parse_data_uri(url) {
            images.push(data);
        }
    }
    images
}

fn normalize_messages(messages: &[Value]) -> Vec<Value> {
    let mut tool_call_map: HashMap<String, String> = HashMap::new();
    for msg in messages {
        if msg.get("role").and_then(|r| r.as_str()) == Some(role::ASSISTANT) {
            if let Some(tcs) = msg.get("tool_calls").and_then(|v| v.as_array()) {
                for tc in tcs {
                    if let (Some(id), Some(name)) = (
                        tc.get("id").and_then(|v| v.as_str()),
                        tc.get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|v| v.as_str()),
                    ) {
                        if !id.is_empty() && !name.is_empty() {
                            tool_call_map.insert(id.to_string(), name.to_string());
                        }
                    }
                }
            }
        }
    }

    let mut result = Vec::new();
    for msg in messages {
        let r = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
        if r == role::TOOL {
            let tool_result = normalize_content(msg.get("content").unwrap_or(&Value::Null));
            if tool_result.is_empty() {
                continue;
            }
            let tc_id = msg
                .get("tool_call_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let tool_name = tool_call_map
                .get(tc_id)
                .map(|s| s.as_str())
                .or_else(|| msg.get("name").and_then(|v| v.as_str()))
                .unwrap_or("unknown_tool");
            result.push(json!({
                "role": role::TOOL,
                "tool_name": tool_name,
                "content": tool_result,
            }));
            continue;
        }

        if r == role::ASSISTANT {
            if let Some(tcs) = msg.get("tool_calls").and_then(|v| v.as_array()) {
                let content = normalize_content(msg.get("content").unwrap_or(&Value::Null));
                let ollama_tcs: Vec<Value> = tcs
                    .iter()
                    .map(|tc| {
                        let func = tc.get("function");
                        let idx = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
                        let name = func
                            .and_then(|f| f.get("name"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let args = match func.and_then(|f| f.get("arguments")) {
                            Some(Value::String(s)) => parse_json_or_empty(Some(s)),
                            Some(v) if v.is_object() => v.clone(),
                            _ => json!({}),
                        };
                        json!({
                            "type": openai_block::FUNCTION,
                            "function": {
                                "index": idx,
                                "name": name,
                                "arguments": args,
                            }
                        })
                    })
                    .collect();
                result.push(json!({
                    "role": role::ASSISTANT,
                    "content": content,
                    "tool_calls": ollama_tcs,
                }));
                continue;
            }
        }

        let content_val = msg.get("content").unwrap_or(&Value::Null);
        let content = normalize_content(content_val);
        let images = extract_images_from_content(content_val);

        if content.is_empty() && r != role::ASSISTANT {
            continue;
        }

        let mut out = json!({
            "role": r,
            "content": content,
        });
        if !images.is_empty() {
            if let Some(obj) = out.as_object_mut() {
                obj.insert("images".to_string(), json!(images));
            }
        }
        result.push(out);
    }

    result
}

pub fn openai_to_ollama_request(
    model: &str,
    body: &Value,
    stream: bool,
    _credentials: Option<&Value>,
) -> Value {
    let msgs = body
        .get("messages")
        .and_then(|v| v.as_array())
        .map(|a| normalize_messages(a))
        .unwrap_or_default();

    let mut out = Map::new();
    out.insert("model".to_string(), json!(model));
    out.insert("messages".to_string(), Value::Array(msgs));
    out.insert("stream".to_string(), json!(stream));

    let mut options = Map::new();
    if let Some(t) = body.get("temperature") {
        options.insert("temperature".to_string(), t.clone());
    }
    if let Some(mt) = body.get("max_tokens") {
        options.insert("num_predict".to_string(), mt.clone());
    }
    if let Some(tp) = body.get("top_p") {
        options.insert("top_p".to_string(), tp.clone());
    }
    if !options.is_empty() {
        out.insert("options".to_string(), Value::Object(options));
    }

    if let Some(tools) = body.get("tools").filter(|v| v.is_array()) {
        out.insert("tools".to_string(), tools.clone());
    }
    if let Some(tc) = body.get("tool_choice") {
        if !tc.is_null() {
            out.insert("tool_choice".to_string(), tc.clone());
        }
    }

    Value::Object(out)
}

fn convert_tool_calls(tool_calls: &[Value]) -> Vec<Value> {
    tool_calls
        .iter()
        .enumerate()
        .map(|(i, tc)| {
            let func = tc.get("function");
            let idx = func
                .and_then(|f| f.get("index"))
                .and_then(|v| v.as_u64())
                .unwrap_or(i as u64);
            let id = tc
                .get("id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .unwrap_or_else(|| fallback_tool_call_id(i));
            let name = func
                .and_then(|f| f.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let args = match func.and_then(|f| f.get("arguments")) {
                Some(Value::String(s)) => s.clone(),
                Some(v) => serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string()),
                None => "{}".to_string(),
            };
            json!({
                "index": idx,
                "id": id,
                "type": openai_block::FUNCTION,
                "function": {
                    "name": name,
                    "arguments": args,
                }
            })
        })
        .collect()
}

pub fn ollama_to_openai_response(chunk: Option<&Value>, state: &mut TranslateState) -> Vec<Value> {
    let Some(chunk) = chunk else {
        return Vec::new();
    };
    if !chunk.is_object() {
        return Vec::new();
    }

    if state.message_id.is_none() {
        state.message_id = Some(format!("chatcmpl-{}", now_unix_millis()));
        state.created = Some(now_unix_secs());
    }
    if state.model.is_none() {
        state.model = chunk
            .get("model")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
    }

    let id = state.message_id.clone().unwrap_or_default();
    let created = state.created.unwrap_or_else(now_unix_secs);
    let model = state.model.clone().unwrap_or_else(|| "ollama".to_string());

    if chunk.get("done").and_then(|v| v.as_bool()) == Some(true) {
        let usage = to_openai_usage(FORMAT_OLLAMA, Some(chunk));
        let done_reason = chunk.get("done_reason").and_then(|v| v.as_str());
        let mut finish_reason = to_openai_finish(FORMAT_OLLAMA, done_reason, state.has_tool_calls);
        if done_reason == Some(openai_finish::TOOL_CALLS) || state.has_tool_calls {
            finish_reason = openai_finish::TOOL_CALLS;
        }
        return vec![build_chunk(
            &id,
            &model,
            created,
            json!({}),
            Some(finish_reason),
            usage,
        )];
    }

    let Some(message) = chunk.get("message").and_then(|v| v.as_object()) else {
        return Vec::new();
    };
    let content = message
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let thinking = message
        .get("thinking")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let tool_calls = message.get("tool_calls").and_then(|v| v.as_array());

    if content.is_empty() && thinking.is_empty() && tool_calls.is_none() {
        return Vec::new();
    }

    let mut delta = Map::new();
    if !content.is_empty() {
        delta.insert("content".to_string(), json!(content));
    }
    if !thinking.is_empty() {
        delta.insert("reasoning_content".to_string(), json!(thinking));
    }
    if let Some(tcs) = tool_calls {
        state.has_tool_calls = true;
        delta.insert("tool_calls".to_string(), Value::Array(convert_tool_calls(tcs)));
    }

    vec![build_chunk(
        &id,
        &model,
        created,
        Value::Object(delta),
        None,
        None,
    )]
}

pub fn ollama_body_to_openai(body: &Value) -> Value {
    let msg = body.get("message").unwrap_or(&Value::Null);
    let content = msg.get("content").and_then(|v| v.as_str()).unwrap_or("");
    let thinking = msg.get("thinking").and_then(|v| v.as_str()).unwrap_or("");
    let tool_calls = msg
        .get("tool_calls")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut message = Map::new();
    message.insert("role".to_string(), json!(role::ASSISTANT));
    if !content.is_empty() {
        message.insert("content".to_string(), json!(content));
    }
    if !thinking.is_empty() {
        message.insert("reasoning_content".to_string(), json!(thinking));
    }
    if !tool_calls.is_empty() {
        message.insert(
            "tool_calls".to_string(),
            Value::Array(convert_tool_calls(&tool_calls)),
        );
    }
    if !message.contains_key("content") && !message.contains_key("tool_calls") {
        message.insert("content".to_string(), json!(""));
    }

    let done_reason = body.get("done_reason").and_then(|v| v.as_str());
    let mut finish_reason = to_openai_finish(FORMAT_OLLAMA, done_reason, !tool_calls.is_empty());
    if !tool_calls.is_empty() {
        finish_reason = openai_finish::TOOL_CALLS;
    }

    json!({
        "id": format!("chatcmpl-{}", now_unix_millis()),
        "object": "chat.completion",
        "created": now_unix_secs(),
        "model": body.get("model").and_then(|v| v.as_str()).unwrap_or("ollama"),
        "choices": [{
            "index": 0,
            "message": Value::Object(message),
            "finish_reason": finish_reason,
        }],
        "usage": to_openai_usage(FORMAT_OLLAMA, Some(body)),
    })
}
