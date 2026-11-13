#![allow(dead_code)]

use ring::digest::{digest, SHA256};
use serde_json::{json, Map, Value};

use crate::translator::concerns::{
    build_chunk, encode_data_uri, fallback_tool_call_id, now_unix_millis, now_unix_secs,
    parse_data_uri, parse_json_or_empty, to_openai_finish, to_openai_usage,
};
use crate::translator::formats::FORMAT_COMMANDCODE;
use crate::translator::schema::{claude_block, defaults, openai_block, role};
use crate::translator::TranslateState;

fn flatten_text(content: &Value) -> String {
    match content {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Array(arr) => arr
            .iter()
            .filter_map(|p| match p {
                Value::String(s) => Some(s.as_str()),
                Value::Object(o) => o.get("text").and_then(|t| t.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        other => other.to_string(),
    }
}

fn to_native_image_block(part: &Value) -> Option<Value> {
    let t = part.get("type").and_then(|v| v.as_str()).unwrap_or("");
    if t == openai_block::IMAGE_URL {
        let url = part
            .get("image_url")
            .and_then(|iu| iu.as_str().or_else(|| iu.get("url").and_then(|u| u.as_str())))?;
        let (mime, b64) = parse_data_uri(url)?;
        return Some(json!({
            "type": claude_block::IMAGE,
            "image": encode_data_uri(Some(&mime), &b64),
            "mimeType": mime,
            "mediaType": mime,
        }));
    }
    if t == claude_block::IMAGE {
        if let Some(img) = part.get("image").and_then(|v| v.as_str()) {
            if img.starts_with("data:") {
                let parsed_mime = parse_data_uri(img).map(|(m, _)| m);
                let mime = part
                    .get("mimeType")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .or(parsed_mime)
                    .unwrap_or_else(|| defaults::DEFAULT_IMAGE_MIME.to_string());
                return Some(json!({
                    "type": claude_block::IMAGE,
                    "image": img,
                    "mimeType": mime,
                    "mediaType": mime,
                }));
            }
        }
        if let Some(src) = part.get("source").and_then(|v| v.as_object()) {
            if src.get("type").and_then(|v| v.as_str()) == Some("base64") {
                if let Some(data) = src.get("data").and_then(|v| v.as_str()) {
                    let mime = src
                        .get("media_type")
                        .and_then(|v| v.as_str())
                        .unwrap_or(defaults::DEFAULT_IMAGE_MIME);
                    return Some(json!({
                        "type": claude_block::IMAGE,
                        "image": encode_data_uri(Some(mime), data),
                        "mimeType": mime,
                        "mediaType": mime,
                    }));
                }
            }
        }
    }
    None
}

fn to_content_blocks(content: &Value) -> Vec<Value> {
    match content {
        Value::Null => vec![json!({ "type": openai_block::TEXT, "text": "" })],
        Value::String(s) => vec![json!({ "type": openai_block::TEXT, "text": s })],
        Value::Array(arr) => {
            let mut blocks = Vec::new();
            for part in arr {
                if let Some(s) = part.as_str() {
                    blocks.push(json!({ "type": openai_block::TEXT, "text": s }));
                } else if part.is_object() {
                    if part.get("type").and_then(|v| v.as_str()) == Some(openai_block::TEXT) {
                        if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                            blocks.push(json!({ "type": openai_block::TEXT, "text": t }));
                            continue;
                        }
                    }
                    if let Some(img) = to_native_image_block(part) {
                        blocks.push(img);
                    } else if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                        blocks.push(json!({ "type": openai_block::TEXT, "text": t }));
                    }
                }
            }
            if blocks.is_empty() {
                vec![json!({ "type": openai_block::TEXT, "text": "" })]
            } else {
                blocks
            }
        }
        other => vec![json!({ "type": openai_block::TEXT, "text": other.to_string() })],
    }
}

fn convert_messages(messages: &[Value]) -> (Vec<Value>, String) {
    let mut out = Vec::new();
    let mut system_texts = Vec::new();

    for m in messages {
        if !m.is_object() {
            continue;
        }
        let r = m.get("role").and_then(|v| v.as_str()).unwrap_or("");
        if r == role::SYSTEM {
            let t = flatten_text(m.get("content").unwrap_or(&Value::Null));
            if !t.is_empty() {
                system_texts.push(t);
            }
            continue;
        }

        if r == role::TOOL {
            let val = match m.get("content") {
                Some(Value::String(s)) => s.clone(),
                Some(c) => flatten_text(c),
                None => String::new(),
            };
            out.push(json!({
                "role": role::TOOL,
                "content": [{
                    "type": "tool-result",
                    "toolCallId": m.get("tool_call_id").and_then(|v| v.as_str()).unwrap_or(""),
                    "toolName": m.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                    "output": { "type": "text", "value": val },
                }],
            }));
            continue;
        }

        if r == role::ASSISTANT {
            let mut blocks = Vec::new();
            let rc = m
                .get("reasoning_content")
                .or_else(|| m.get("thought"))
                .or_else(|| m.get("reasoning"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let has_tcs = m
                .get("tool_calls")
                .and_then(|v| v.as_array())
                .map(|a| !a.is_empty())
                .unwrap_or(false);
            if !rc.is_empty() || has_tcs {
                blocks.push(json!({ "type": "reasoning", "text": if rc.is_empty() { " " } else { rc } }));
            }
            let text = flatten_text(m.get("content").unwrap_or(&Value::Null));
            if !text.is_empty() {
                blocks.push(json!({ "type": openai_block::TEXT, "text": text }));
            }
            if let Some(tcs) = m.get("tool_calls").and_then(|v| v.as_array()) {
                for tc in tcs {
                    let fn_obj = tc.get("function");
                    let args_val = match fn_obj.and_then(|f| f.get("arguments")) {
                        Some(Value::String(s)) => parse_json_or_empty(Some(s)),
                        Some(v) => v.clone(),
                        None => json!({}),
                    };
                    blocks.push(json!({
                        "type": "tool-call",
                        "toolCallId": tc.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                        "toolName": fn_obj.and_then(|f| f.get("name")).and_then(|v| v.as_str()).unwrap_or(""),
                        "input": args_val,
                    }));
                }
            }
            if blocks.is_empty() {
                blocks.push(json!({ "type": openai_block::TEXT, "text": "" }));
            }
            out.push(json!({ "role": role::ASSISTANT, "content": blocks }));
            continue;
        }

        out.push(json!({
            "role": role::USER,
            "content": to_content_blocks(m.get("content").unwrap_or(&Value::Null)),
        }));
    }

    (out, system_texts.join("\n\n"))
}

fn convert_tools(tools: Option<&Value>) -> Option<Vec<Value>> {
    let arr = tools?.as_array()?;
    let mut result = Vec::new();
    for t in arr {
        if !t.is_object() {
            continue;
        }
        if t.get("type").and_then(|v| v.as_str()) == Some(openai_block::FUNCTION) {
            if let Some(f) = t.get("function") {
                result.push(json!({
                    "name": f.get("name").cloned().unwrap_or(Value::Null),
                    "description": f.get("description").cloned().unwrap_or(Value::Null),
                    "input_schema": f.get("parameters").cloned().unwrap_or_else(|| json!({ "type": "object" })),
                }));
                continue;
            }
        }
        if let Some(name) = t.get("name").and_then(|v| v.as_str()) {
            if let Some(schema) = t.get("input_schema").or_else(|| t.get("parameters")) {
                result.push(json!({
                    "name": name,
                    "description": t.get("description").cloned().unwrap_or(Value::Null),
                    "input_schema": schema.clone(),
                }));
            }
        }
    }
    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

pub fn to_commandcode_session_uuid(session_id: Option<&str>) -> String {
    let seed = session_id.filter(|s| !s.is_empty()).unwrap_or("default-session");
    let d = digest(&SHA256, seed.as_bytes());
    let b = d.as_ref();
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-4{:01x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3],
        b[4], b[5],
        b[6] & 0x0f, b[7],
        (b[8] & 0x3f) | 0x80, b[9],
        b[10], b[11], b[12], b[13], b[14], b[15]
    )
}

pub fn openai_to_commandcode_request(
    model: &str,
    body: &Value,
    stream: bool,
    credentials: Option<&Value>,
) -> Value {
    let msgs_slice = body
        .get("messages")
        .and_then(|v| v.as_array())
        .map(|v| v.as_slice())
        .unwrap_or(&[]);
    let (messages, system) = convert_messages(msgs_slice);

    let max_tokens = body
        .get("max_tokens")
        .or_else(|| body.get("max_output_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(crate::config::DEFAULT_MAX_TOKENS as u64);
    let temperature = body
        .get("temperature")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.3);

    let mut params = Map::new();
    params.insert("model".to_string(), json!(model));
    params.insert("messages".to_string(), Value::Array(messages));
    params.insert("stream".to_string(), json!(stream));
    params.insert("max_tokens".to_string(), json!(max_tokens));
    params.insert("temperature".to_string(), json!(temperature));
    if !system.is_empty() {
        params.insert("system".to_string(), json!(system));
    }
    if let Some(tools) = convert_tools(body.get("tools")) {
        params.insert("tools".to_string(), Value::Array(tools));
    }
    if let Some(tp) = body.get("top_p").filter(|v| !v.is_null()) {
        params.insert("top_p".to_string(), tp.clone());
    }

    let session_id = credentials
        .and_then(|c| c.get("_clientSessionId"))
        .and_then(|v| v.as_str());
    let working_dir = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "/".to_string());

    json!({
        "threadId": to_commandcode_session_uuid(session_id),
        "memory": "",
        "config": {
            "workingDir": working_dir,
            "date": "2026-01-01",
            "environment": std::env::consts::OS,
            "structure": [],
            "isGitRepo": false,
            "currentBranch": "",
            "mainBranch": "",
            "gitStatus": "",
            "recentCommits": [],
        },
        "params": Value::Object(params),
    })
}

pub fn commandcode_to_openai_response(
    chunk: Option<&Value>,
    state: &mut TranslateState,
) -> Vec<Value> {
    let Some(event) = chunk else {
        return Vec::new();
    };
    if !event.is_object() {
        return Vec::new();
    }
    if event.get("object").and_then(|v| v.as_str()) == Some("chat.completion.chunk") {
        return vec![event.clone()];
    }

    let Some(ev_type) = event.get("type").and_then(|v| v.as_str()) else {
        return Vec::new();
    };

    if state.message_id.is_none() {
        state.message_id = Some(format!("chatcmpl-{}", now_unix_millis()));
        state.created = Some(now_unix_secs());
        state.model = Some(
            event
                .get("model")
                .and_then(|v| v.as_str())
                .unwrap_or("commandcode")
                .to_string(),
        );
    }

    let id = state.message_id.clone().unwrap_or_default();
    let created = state.created.unwrap_or_else(now_unix_secs);
    let model = state
        .model
        .clone()
        .unwrap_or_else(|| "commandcode".to_string());
    let mut out = Vec::new();

    match ev_type {
        "text-delta" => {
            let text = event
                .get("text")
                .or_else(|| event.get("delta"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if !text.is_empty() {
                let mut delta = json!({ "content": text });
                if !state.started {
                    delta["role"] = json!(role::ASSISTANT);
                    state.started = true;
                }
                state.text_block_started = true;
                out.push(build_chunk(&id, &model, created, delta, None, None));
            }
        }
        "reasoning-delta" => {
            let text = event.get("text").and_then(|v| v.as_str()).unwrap_or("");
            if !text.is_empty() {
                let mut delta = json!({ "reasoning_content": text });
                if !state.started {
                    delta["role"] = json!(role::ASSISTANT);
                    state.started = true;
                }
                out.push(build_chunk(&id, &model, created, delta, None, None));
            }
        }
        "tool-input-start" => {
            let tc_id = event
                .get("id")
                .or_else(|| event.get("toolCallId"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .unwrap_or_else(|| fallback_tool_call_id(state.tool_call_index));
            let idx = if let Some(pos) = state
                .tool_calls_map
                .iter()
                .find(|(_, v)| v.get("id").and_then(|x| x.as_str()) == Some(&tc_id))
                .map(|(k, _)| *k)
            {
                pos
            } else {
                let pos = state.tool_call_index;
                state.tool_call_index += 1;
                state
                    .tool_calls_map
                    .insert(pos, json!({ "id": tc_id }));
                pos
            };
            let tool_name = event
                .get("toolName")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let mut delta = json!({
                "tool_calls": [{
                    "index": idx,
                    "id": tc_id,
                    "type": openai_block::FUNCTION,
                    "function": { "name": tool_name, "arguments": "" }
                }]
            });
            if !state.started {
                delta["role"] = json!(role::ASSISTANT);
                state.started = true;
            }
            out.push(build_chunk(&id, &model, created, delta, None, None));
        }
        "tool-input-delta" => {
            let tc_id = event
                .get("id")
                .or_else(|| event.get("toolCallId"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if let Some((&idx, _)) = state
                .tool_calls_map
                .iter()
                .find(|(_, v)| v.get("id").and_then(|x| x.as_str()) == Some(tc_id))
            {
                let args_delta = event
                    .get("delta")
                    .or_else(|| event.get("inputTextDelta"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let delta = json!({
                    "tool_calls": [{
                        "index": idx,
                        "function": { "arguments": args_delta }
                    }]
                });
                out.push(build_chunk(&id, &model, created, delta, None, None));
            }
        }
        "tool-call" => {
            let tc_id = event
                .get("toolCallId")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let already = state
                .tool_calls_map
                .values()
                .any(|v| v.get("id").and_then(|x| x.as_str()) == Some(tc_id));
            if !already {
                let idx = state.tool_call_index;
                state.tool_call_index += 1;
                state
                    .tool_calls_map
                    .insert(idx, json!({ "id": tc_id }));
                let args_str = match event.get("input") {
                    Some(Value::String(s)) => s.clone(),
                    Some(v) => serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string()),
                    None => "{}".to_string(),
                };
                let tool_name = event
                    .get("toolName")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let mut delta = json!({
                    "tool_calls": [{
                        "index": idx,
                        "id": tc_id,
                        "type": openai_block::FUNCTION,
                        "function": { "name": tool_name, "arguments": args_str }
                    }]
                });
                if !state.started {
                    delta["role"] = json!(role::ASSISTANT);
                    state.started = true;
                }
                out.push(build_chunk(&id, &model, created, delta, None, None));
            }
        }
        "finish-step" => {
            let reason = event.get("finishReason").and_then(|v| v.as_str());
            state.finish_reason = Some(to_openai_finish(FORMAT_COMMANDCODE, reason, false).to_string());
            if let Some(u) = event.get("usage") {
                state.usage = Some(u.clone());
            }
        }
        "finish" => {
            let reason_str = state
                .finish_reason
                .as_deref()
                .unwrap_or_else(|| {
                    let r = event
                        .get("finishReason")
                        .and_then(|v| v.as_str())
                        .unwrap_or("stop");
                    to_openai_finish(FORMAT_COMMANDCODE, Some(r), false)
                });
            let total_usage = event
                .get("totalUsage")
                .or(state.usage.as_ref());
            let usage = to_openai_usage(FORMAT_COMMANDCODE, total_usage);
            out.push(build_chunk(&id, &model, created, json!({}), Some(reason_str), usage));
        }
        _ => {}
    }

    out
}
