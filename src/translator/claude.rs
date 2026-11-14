use serde_json::{json, Map, Value};
use std::collections::HashSet;

use crate::translator::concerns::{
    adjust_max_tokens, build_chunk, collapse_text_parts, encode_data_uri, ensure_tool_call_ids,
    extract_reasoning_text, fix_missing_tool_responses, from_openai_finish, now_unix_secs,
    parse_data_uri, parse_json_or_empty, reasoning_delta, to_openai_finish, to_openai_usage,
};
use crate::translator::formats::FORMAT_CLAUDE;
use crate::translator::schema::{
    claude_block, claude_delta, claude_event, claude_stop, defaults, openai_block, openai_finish,
    role,
};
use crate::translator::{ClaudeBlockState, TranslateState};

// ============================================================================
// Request: Claude -> OpenAI
// ============================================================================

pub fn claude_to_openai_request(
    model: &str,
    body: &Value,
    stream: bool,
    _credentials: Option<&Value>,
) -> Value {
    let mut result = Map::new();
    result.insert("model".to_string(), Value::String(model.to_string()));
    result.insert("stream".to_string(), Value::Bool(stream));

    if let Some(v) = body.get("max_tokens") {
        result.insert("max_tokens".to_string(), v.clone());
    }
    if let Some(v) = body.get("temperature") {
        result.insert("temperature".to_string(), v.clone());
    }
    if let Some(v) = body.get("top_p") {
        result.insert("top_p".to_string(), v.clone());
    }

    if let Some(thinking) = body.get("thinking") {
        let ttype = thinking.get("type").and_then(|v| v.as_str());
        if ttype == Some("enabled") {
            let budget = thinking
                .get("budget_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(defaults::DEFAULT_THINKING_BUDGET);
            let effort = if budget <= 1024 {
                "low"
            } else if budget <= 8192 {
                "medium"
            } else {
                "high"
            };
            result.insert("reasoning_effort".to_string(), json!(effort));
        } else if ttype == Some("adaptive") {
            let effort = body
                .get("output_config")
                .and_then(|o| o.get("effort"))
                .and_then(|v| v.as_str())
                .unwrap_or("high");
            result.insert("reasoning_effort".to_string(), json!(effort));
        }
    }

    let mut messages: Vec<Value> = Vec::new();

    if let Some(sys) = body.get("system") {
        let sys_text = if let Some(s) = sys.as_str() {
            s.to_string()
        } else if let Some(arr) = sys.as_array() {
            arr.iter()
                .filter_map(|b| {
                    if b.get("type").and_then(|t| t.as_str()) == Some(claude_block::TEXT) {
                        b.get("text").and_then(|t| t.as_str())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            String::new()
        };
        if !sys_text.is_empty() {
            messages.push(json!({
                "role": role::SYSTEM,
                "content": sys_text,
            }));
        }
    }

    if let Some(msgs) = body.get("messages").and_then(|v| v.as_array()) {
        for msg in msgs {
            let msg_role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
            let content = msg.get("content");

            if let Some(c_str) = content.and_then(|v| v.as_str()) {
                messages.push(json!({ "role": msg_role, "content": c_str }));
                continue;
            }

            let Some(blocks) = content.and_then(|v| v.as_array()) else {
                continue;
            };

            if msg_role == role::USER {
                let mut parts: Vec<Value> = Vec::new();
                let mut tool_results: Vec<Value> = Vec::new();

                for block in blocks {
                    let btype = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    match btype {
                        claude_block::TEXT => {
                            if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                                if !text.is_empty() {
                                    parts.push(json!({ "type": openai_block::TEXT, "text": text }));
                                }
                            }
                        }
                        claude_block::IMAGE => {
                            if let Some(src) = block.get("source") {
                                let url = if src.get("type").and_then(|v| v.as_str()) == Some("url") {
                                    src.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string()
                                } else {
                                    let mime = src
                                        .get("media_type")
                                        .and_then(|v| v.as_str());
                                    let data = src.get("data").and_then(|v| v.as_str()).unwrap_or("");
                                    encode_data_uri(mime, data)
                                };
                                parts.push(json!({
                                    "type": openai_block::IMAGE_URL,
                                    "image_url": { "url": url }
                                }));
                            }
                        }
                        claude_block::TOOL_RESULT => {
                            let tc_id = block
                                .get("tool_use_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            let raw_c = block.get("content").unwrap_or(&Value::Null);
                            let tool_text = if let Some(s) = raw_c.as_str() {
                                s.to_string()
                            } else if let Some(arr) = raw_c.as_array() {
                                arr.iter()
                                    .map(|item| {
                                        if item.get("type").and_then(|v| v.as_str())
                                            == Some(claude_block::TEXT)
                                        {
                                            item.get("text")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("")
                                                .to_string()
                                        } else {
                                            serde_json::to_string(item).unwrap_or_default()
                                        }
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            } else {
                                serde_json::to_string(raw_c).unwrap_or_default()
                            };
                            tool_results.push(json!({
                                "role": role::TOOL,
                                "tool_call_id": tc_id,
                                "content": tool_text
                            }));
                        }
                        _ => {}
                    }
                }

                for tr in tool_results {
                    messages.push(tr);
                }
                if !parts.is_empty() {
                    let collapsed = collapse_text_parts(&Value::Array(parts));
                    messages.push(json!({ "role": role::USER, "content": collapsed }));
                }
            } else if msg_role == role::ASSISTANT {
                let mut text_parts: Vec<String> = Vec::new();
                let mut tool_calls: Vec<Value> = Vec::new();
                let mut reasoning_parts: Vec<String> = Vec::new();

                for block in blocks {
                    let btype = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    match btype {
                        claude_block::THINKING => {
                            if let Some(t) = block.get("thinking").and_then(|v| v.as_str()) {
                                if !t.is_empty() {
                                    reasoning_parts.push(t.to_string());
                                }
                            }
                        }
                        claude_block::TEXT => {
                            if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                                if !t.is_empty() {
                                    text_parts.push(t.to_string());
                                }
                            }
                        }
                        claude_block::TOOL_USE => {
                            let id = block.get("id").and_then(|v| v.as_str()).unwrap_or("");
                            let name = block.get("name").and_then(|v| v.as_str()).unwrap_or("");
                            let input = block.get("input").cloned().unwrap_or_else(|| json!({}));
                            let args = serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_string());
                            tool_calls.push(json!({
                                "id": id,
                                "type": openai_block::FUNCTION,
                                "function": {
                                    "name": name,
                                    "arguments": args
                                }
                            }));
                        }
                        _ => {}
                    }
                }

                let mut asst = Map::new();
                asst.insert("role".to_string(), json!(role::ASSISTANT));
                if !reasoning_parts.is_empty() {
                    asst.insert(
                        "reasoning_content".to_string(),
                        Value::String(reasoning_parts.join("")),
                    );
                }
                if !text_parts.is_empty() {
                    asst.insert("content".to_string(), Value::String(text_parts.join("\n")));
                } else if tool_calls.is_empty() {
                    asst.insert("content".to_string(), Value::String(String::new()));
                } else {
                    asst.insert("content".to_string(), Value::Null);
                }
                if !tool_calls.is_empty() {
                    asst.insert("tool_calls".to_string(), Value::Array(tool_calls));
                }
                messages.push(Value::Object(asst));
            }
        }
    }

    result.insert("messages".to_string(), Value::Array(messages));

    if let Some(tools) = body.get("tools").and_then(|v| v.as_array()) {
        let out_tools: Vec<Value> = tools
            .iter()
            .filter_map(|t| {
                let name = t.get("name").and_then(|v| v.as_str())?;
                Some(json!({
                    "type": openai_block::FUNCTION,
                    "function": {
                        "name": name,
                        "description": t.get("description").cloned().unwrap_or(json!("")),
                        "parameters": t.get("input_schema").cloned().unwrap_or_else(|| json!({ "type": "object", "properties": {} }))
                    }
                }))
            })
            .collect();
        if !out_tools.is_empty() {
            result.insert("tools".to_string(), Value::Array(out_tools));
        }
    }

    if let Some(tc) = body.get("tool_choice") {
        let ttype = tc.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let mapped = match ttype {
            "auto" => json!("auto"),
            "any" => json!("required"),
            "none" => json!("none"),
            "tool" => json!({
                "type": openai_block::FUNCTION,
                "function": { "name": tc.get("name").cloned().unwrap_or(Value::Null) }
            }),
            _ => json!("auto"),
        };
        result.insert("tool_choice".to_string(), mapped);
    }

    if let Some(stop) = body.get("stop_sequences") {
        result.insert("stop".to_string(), stop.clone());
    }

    Value::Object(result)
}

// ============================================================================
// Request: OpenAI -> Claude
// ============================================================================

fn extract_openai_text_parts(content: &Value) -> Vec<String> {
    if let Some(s) = content.as_str() {
        if !s.is_empty() {
            return vec![s.to_string()];
        }
        return vec![];
    }
    if let Some(arr) = content.as_array() {
        return arr
            .iter()
            .filter_map(|p| {
                if p.get("type").and_then(|v| v.as_str()) == Some(openai_block::TEXT) {
                    p.get("text")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string())
                } else {
                    None
                }
            })
            .collect();
    }
    vec![]
}

fn convert_openai_content_to_claude_blocks(content: &Value) -> Vec<Value> {
    if let Some(s) = content.as_str() {
        if s.is_empty() {
            return vec![];
        }
        return vec![json!({ "type": claude_block::TEXT, "text": s })];
    }
    let Some(arr) = content.as_array() else {
        return vec![];
    };
    let mut blocks = Vec::new();
    for part in arr {
        let ptype = part.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match ptype {
            openai_block::TEXT => {
                if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                    if !text.is_empty() {
                        blocks.push(json!({ "type": claude_block::TEXT, "text": text }));
                    }
                }
            }
            openai_block::IMAGE_URL => {
                let url = part
                    .get("image_url")
                    .and_then(|i| i.get("url"))
                    .and_then(|v| v.as_str())
                    .or_else(|| part.get("image_url").and_then(|v| v.as_str()));
                if let Some(url) = url {
                    if let Some((mime, data)) = parse_data_uri(url) {
                        blocks.push(json!({
                            "type": claude_block::IMAGE,
                            "source": {
                                "type": "base64",
                                "media_type": mime,
                                "data": data
                            }
                        }));
                    } else if url.starts_with("http://") || url.starts_with("https://") {
                        blocks.push(json!({
                            "type": claude_block::IMAGE,
                            "source": {
                                "type": "url",
                                "url": url
                            }
                        }));
                    }
                }
            }
            _ => {}
        }
    }
    blocks
}

fn is_claude_46_or_later(model: &str) -> bool {
    let lower = model.to_lowercase();
    lower.contains("4-6") || lower.contains("4.6") || lower.contains("4-7") || lower.contains("4.7")
}

fn supports_thinking_claude(model: &str) -> bool {
    let lower = model.to_lowercase();
    lower.contains("opus") || lower.contains("sonnet") || lower.contains("haiku-4")
}

fn openai_to_claude_request_core(
    model: &str,
    body: &Value,
    stream: bool,
    for_antigravity: bool,
) -> Value {
    let mut result = Map::new();
    result.insert("model".to_string(), Value::String(model.to_string()));
    result.insert("max_tokens".to_string(), json!(adjust_max_tokens(body)));
    result.insert("stream".to_string(), Value::Bool(stream));

    if let Some(v) = body.get("temperature") {
        result.insert("temperature".to_string(), v.clone());
    }
    if let Some(v) = body.get("top_p") {
        result.insert("top_p".to_string(), v.clone());
    }
    if let Some(stop) = body.get("stop") {
        if stop.is_array() {
            result.insert("stop_sequences".to_string(), stop.clone());
        } else if let Some(s) = stop.as_str() {
            result.insert("stop_sequences".to_string(), json!([s]));
        }
    }

    let mut raw_messages = body
        .get("messages")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    ensure_tool_call_ids(&mut raw_messages);
    let fixed_messages = fix_missing_tool_responses(&raw_messages);

    let mut system_parts: Vec<String> = Vec::new();
    let mut claude_messages: Vec<Value> = Vec::new();

    for msg in &fixed_messages {
        let r = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
        match r {
            role::SYSTEM | role::DEVELOPER => {
                if let Some(c) = msg.get("content") {
                    system_parts.extend(extract_openai_text_parts(c));
                }
            }
            role::USER => {
                let mut blocks = msg
                    .get("content")
                    .map(convert_openai_content_to_claude_blocks)
                    .unwrap_or_default();
                if blocks.is_empty() {
                    blocks.push(json!({ "type": claude_block::TEXT, "text": "" }));
                }
                push_or_merge_claude_msg(&mut claude_messages, role::USER, blocks);
            }
            role::ASSISTANT => {
                let mut blocks: Vec<Value> = Vec::new();
                let rc = msg
                    .get("reasoning_content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if !rc.is_empty() {
                    let sig = if for_antigravity {
                        defaults::DEFAULT_THINKING_AG_SIGNATURE
                    } else {
                        defaults::DEFAULT_CLAUDE_SIGNATURE
                    };
                    blocks.push(json!({
                        "type": claude_block::THINKING,
                        "thinking": rc,
                        "signature": sig
                    }));
                }

                if let Some(c) = msg.get("content") {
                    blocks.extend(convert_openai_content_to_claude_blocks(c));
                }

                if let Some(tcs) = msg.get("tool_calls").and_then(|v| v.as_array()) {
                    for tc in tcs {
                        let id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("");
                        let name = tc
                            .get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let args_val = tc.get("function").and_then(|f| f.get("arguments"));
                        let input = match args_val {
                            Some(Value::String(s)) => parse_json_or_empty(Some(s)),
                            Some(v) if v.is_object() => v.clone(),
                            _ => json!({}),
                        };
                        blocks.push(json!({
                            "type": claude_block::TOOL_USE,
                            "id": id,
                            "name": name,
                            "input": input
                        }));
                    }
                }

                if !blocks.is_empty() {
                    push_or_merge_claude_msg(&mut claude_messages, role::ASSISTANT, blocks);
                }
            }
            role::TOOL => {
                let tc_id = msg
                    .get("tool_call_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let content_val = match msg.get("content") {
                    Some(Value::String(s)) => Value::String(s.clone()),
                    Some(Value::Array(arr)) => {
                        let mapped: Vec<Value> = arr
                            .iter()
                            .filter_map(|p| {
                                let pt = p.get("type").and_then(|v| v.as_str()).unwrap_or("");
                                if pt == openai_block::TEXT {
                                    Some(json!({ "type": claude_block::TEXT, "text": p.get("text").and_then(|v| v.as_str()).unwrap_or("") }))
                                } else {
                                    None
                                }
                            })
                            .collect();
                        if mapped.is_empty() {
                            Value::String(String::new())
                        } else {
                            Value::Array(mapped)
                        }
                    }
                    Some(v) if !v.is_null() => {
                        Value::String(serde_json::to_string(v).unwrap_or_default())
                    }
                    _ => Value::String(String::new()),
                };

                let tr_block = json!({
                    "type": claude_block::TOOL_RESULT,
                    "tool_use_id": tc_id,
                    "content": content_val
                });
                push_or_merge_claude_msg(&mut claude_messages, role::USER, vec![tr_block]);
            }
            _ => {}
        }
    }

    let mut system = Vec::new();
    if !for_antigravity {
        system.push(json!({"type": "text", "text": "You are Claude Code, Anthropic's official CLI for Claude."}));
    }
    if !system_parts.is_empty() {
        system.push(json!({"type": "text", "text": system_parts.join("\n")}));
    }
    if !system.is_empty() {
        if !for_antigravity {
            system.last_mut().unwrap()["cache_control"] = json!({"type":"ephemeral", "ttl":"1h"});
        }
        result.insert("system".to_string(), Value::Array(system));
    }

    // Thinking configuration
    if let Some(thinking) = body.get("thinking") {
        if thinking.get("type").and_then(|v| v.as_str()) == Some("enabled") {
            let budget = thinking
                .get("budget_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(defaults::DEFAULT_THINKING_BUDGET);
            result.insert(
                "thinking".to_string(),
                json!({ "type": "enabled", "budget_tokens": budget }),
            );
            result.remove("temperature");
        }
    } else if let Some(effort) = body.get("reasoning_effort").and_then(|v| v.as_str()) {
        if effort != "none" && supports_thinking_claude(model) {
            if is_claude_46_or_later(model) && !for_antigravity {
                let mapped_effort = match effort {
                    "minimal" | "low" => "low",
                    "medium" => "medium",
                    "xhigh" | "max" => "max",
                    _ => "high",
                };
                result.insert("thinking".to_string(), json!({ "type": "adaptive" }));
                result.insert(
                    "output_config".to_string(),
                    json!({ "effort": mapped_effort }),
                );
            } else {
                let budget = match effort {
                    "minimal" => 1024,
                    "low" => 2048,
                    "medium" => 8192,
                    "high" => 16384,
                    "xhigh" | "max" => 32768,
                    _ => defaults::DEFAULT_THINKING_BUDGET,
                };
                result.insert(
                    "thinking".to_string(),
                    json!({ "type": "enabled", "budget_tokens": budget }),
                );
            }
            result.remove("temperature");
        }
    }

    // Tools
    if let Some(tools) = body.get("tools").and_then(|v| v.as_array()) {
        let claude_tools: Vec<Value> = tools
            .iter()
            .filter_map(|t| {
                let f = t.get("function").unwrap_or(t);
                let name = f.get("name").and_then(|v| v.as_str())?;
                Some(json!({
                    "name": name,
                    "description": f.get("description").cloned().unwrap_or(json!("")),
                    "input_schema": f.get("parameters").cloned().unwrap_or_else(|| json!({ "type": "object", "properties": {} }))
                }))
            })
            .collect();
        if !claude_tools.is_empty() {
            result.insert("tools".to_string(), Value::Array(claude_tools));
        }
    }

    if let Some(tc) = body.get("tool_choice") {
        if let Some(s) = tc.as_str() {
            match s {
                "auto" => {
                    result.insert("tool_choice".to_string(), json!({ "type": "auto" }));
                }
                "required" => {
                    result.insert("tool_choice".to_string(), json!({ "type": "any" }));
                }
                "none" => {
                    result.insert("tool_choice".to_string(), json!({ "type": "none" }));
                }
                _ => {}
            }
        } else if tc.is_object() {
            if let Some(name) = tc
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(|v| v.as_str())
            {
                result.insert(
                    "tool_choice".to_string(),
                    json!({ "type": "tool", "name": name }),
                );
            }
        }
    }

    claude_messages = fix_tool_use_ordering(claude_messages);
    ensure_trailing_user_turn(&mut claude_messages);
    result.insert("messages".to_string(), Value::Array(claude_messages));

    Value::Object(result)
}

pub fn openai_to_claude_request(
    model: &str,
    body: &Value,
    stream: bool,
    _credentials: Option<&Value>,
) -> Value {
    openai_to_claude_request_core(model, body, stream, false)
}

pub fn openai_to_claude_request_for_antigravity(
    model: &str,
    body: &Value,
    stream: bool,
    _credentials: Option<&Value>,
) -> Value {
    openai_to_claude_request_core(model, body, stream, true)
}

fn push_or_merge_claude_msg(messages: &mut Vec<Value>, msg_role: &str, blocks: Vec<Value>) {
    if blocks.is_empty() {
        return;
    }
    if let Some(last) = messages.last_mut() {
        if last.get("role").and_then(|v| v.as_str()) == Some(msg_role) {
            if let Some(arr) = last.get_mut("content").and_then(|v| v.as_array_mut()) {
                arr.extend(blocks);
                return;
            }
        }
    }
    messages.push(json!({ "role": msg_role, "content": blocks }));
}

/// Ensure every assistant message's tool_use blocks are followed immediately by a user message
/// containing matching tool_result blocks, and filter out orphan tool_results.
pub fn fix_tool_use_ordering(messages: Vec<Value>) -> Vec<Value> {
    let mut valid_tool_use_ids: HashSet<String> = HashSet::new();
    for msg in &messages {
        if msg.get("role").and_then(|v| v.as_str()) == Some(role::ASSISTANT) {
            if let Some(arr) = msg.get("content").and_then(|v| v.as_array()) {
                for b in arr {
                    if b.get("type").and_then(|v| v.as_str()) == Some(claude_block::TOOL_USE) {
                        if let Some(id) = b.get("id").and_then(|v| v.as_str()) {
                            valid_tool_use_ids.insert(id.to_string());
                        }
                    }
                }
            }
        }
    }

    let mut out = Vec::with_capacity(messages.len());
    for mut msg in messages {
        if msg.get("role").and_then(|v| v.as_str()) == Some(role::USER) {
            if let Some(arr) = msg.get_mut("content").and_then(|v| v.as_array_mut()) {
                arr.retain(|b| {
                    if b.get("type").and_then(|v| v.as_str()) == Some(claude_block::TOOL_RESULT) {
                        b.get("tool_use_id")
                            .and_then(|v| v.as_str())
                            .map(|id| valid_tool_use_ids.contains(id))
                            .unwrap_or(false)
                    } else {
                        true
                    }
                });
                if arr.is_empty() {
                    continue;
                }
            }
        }
        out.push(msg);
    }
    out
}

pub fn ensure_trailing_user_turn(messages: &mut Vec<Value>) {
    if let Some(last) = messages.last() {
        if last.get("role").and_then(|v| v.as_str()) == Some(role::ASSISTANT) {
            messages.push(json!({
                "role": role::USER,
                "content": [{ "type": claude_block::TEXT, "text": "Continue" }]
            }));
        }
    }
}

// ============================================================================
// Response (SSE Chunk): Claude -> OpenAI
// ============================================================================

pub fn claude_to_openai_response(chunk: Option<&Value>, state: &mut TranslateState) -> Vec<Value> {
    let Some(chunk) = chunk else {
        return vec![];
    };
    let ev_type = chunk.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let created = *state.created.get_or_insert_with(now_unix_secs);

    match ev_type {
        claude_event::MESSAGE_START => {
            if let Some(msg) = chunk.get("message") {
                if let Some(id) = msg.get("id").and_then(|v| v.as_str()) {
                    state.message_id = Some(id.to_string());
                }
                if let Some(m) = msg.get("model").and_then(|v| v.as_str()) {
                    state.model = Some(m.to_string());
                }
                if let Some(u) = msg.get("usage") {
                    state.usage = to_openai_usage(FORMAT_CLAUDE, Some(u));
                }
            }
            let id = state
                .message_id
                .clone()
                .unwrap_or_else(|| format!("chatcmpl-{}", created));
            let model = state
                .model
                .clone()
                .unwrap_or_else(|| defaults::MODEL_FALLBACK.to_string());
            state.started = true;
            vec![build_chunk(
                &id,
                &model,
                created,
                json!({ "role": role::ASSISTANT }),
                None,
                None,
            )]
        }

        claude_event::CONTENT_BLOCK_START => {
            let index = chunk.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let block = chunk.get("content_block").unwrap_or(&Value::Null);
            let btype = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let id = state
                .message_id
                .clone()
                .unwrap_or_else(|| format!("chatcmpl-{}", created));
            let model = state
                .model
                .clone()
                .unwrap_or_else(|| defaults::MODEL_FALLBACK.to_string());

            match btype {
                claude_block::THINKING => {
                    state.claude_blocks.insert(
                        index,
                        ClaudeBlockState {
                            tool_call_index: None,
                        },
                    );
                    let initial = block.get("thinking").and_then(|v| v.as_str()).unwrap_or("");
                    if !initial.is_empty() {
                        let with_role = !state.started;
                        state.started = true;
                        return vec![build_chunk(
                            &id,
                            &model,
                            created,
                            reasoning_delta(initial, with_role),
                            None,
                            None,
                        )];
                    }
                    vec![]
                }
                claude_block::TEXT => {
                    state.claude_blocks.insert(
                        index,
                        ClaudeBlockState {
                            tool_call_index: None,
                        },
                    );
                    let initial = block.get("text").and_then(|v| v.as_str()).unwrap_or("");
                    if !initial.is_empty() {
                        let delta = if !state.started {
                            state.started = true;
                            json!({ "role": role::ASSISTANT, "content": initial })
                        } else {
                            json!({ "content": initial })
                        };
                        return vec![build_chunk(&id, &model, created, delta, None, None)];
                    }
                    vec![]
                }
                claude_block::TOOL_USE => {
                    let tc_idx = state.tool_call_index;
                    state.tool_call_index += 1;
                    state.has_tool_calls = true;
                    state.claude_blocks.insert(
                        index,
                        ClaudeBlockState {
                            tool_call_index: Some(tc_idx),
                        },
                    );
                    let tc_id = block.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    let tc_name = block.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let with_role = if !state.started {
                        state.started = true;
                        true
                    } else {
                        false
                    };
                    let mut delta = json!({
                        "tool_calls": [{
                            "index": tc_idx,
                            "id": tc_id,
                            "type": openai_block::FUNCTION,
                            "function": {
                                "name": tc_name,
                                "arguments": ""
                            }
                        }]
                    });
                    if with_role {
                        delta["role"] = json!(role::ASSISTANT);
                    }
                    vec![build_chunk(&id, &model, created, delta, None, None)]
                }
                _ => vec![],
            }
        }

        claude_event::CONTENT_BLOCK_DELTA => {
            let index = chunk.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let delta_obj = chunk.get("delta").unwrap_or(&Value::Null);
            let dtype = delta_obj.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let id = state
                .message_id
                .clone()
                .unwrap_or_else(|| format!("chatcmpl-{}", created));
            let model = state
                .model
                .clone()
                .unwrap_or_else(|| defaults::MODEL_FALLBACK.to_string());

            match dtype {
                claude_delta::THINKING_DELTA => {
                    let text = delta_obj
                        .get("thinking")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if text.is_empty() {
                        return vec![];
                    }
                    let with_role = !state.started;
                    state.started = true;
                    vec![build_chunk(
                        &id,
                        &model,
                        created,
                        reasoning_delta(text, with_role),
                        None,
                        None,
                    )]
                }
                claude_delta::TEXT_DELTA => {
                    let text = delta_obj.get("text").and_then(|v| v.as_str()).unwrap_or("");
                    if text.is_empty() {
                        return vec![];
                    }
                    let delta = if !state.started {
                        state.started = true;
                        json!({ "role": role::ASSISTANT, "content": text })
                    } else {
                        json!({ "content": text })
                    };
                    vec![build_chunk(&id, &model, created, delta, None, None)]
                }
                claude_delta::INPUT_JSON_DELTA => {
                    let partial = delta_obj
                        .get("partial_json")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let tc_idx = state
                        .claude_blocks
                        .get(&index)
                        .and_then(|b| b.tool_call_index)
                        .unwrap_or(0);
                    vec![build_chunk(
                        &id,
                        &model,
                        created,
                        json!({
                            "tool_calls": [{
                                "index": tc_idx,
                                "function": { "arguments": partial }
                            }]
                        }),
                        None,
                        None,
                    )]
                }
                _ => vec![],
            }
        }

        claude_event::MESSAGE_DELTA => {
            let stop_reason = chunk
                .get("delta")
                .and_then(|d| d.get("stop_reason"))
                .and_then(|v| v.as_str());
            let finish = to_openai_finish(FORMAT_CLAUDE, stop_reason, state.has_tool_calls);
            state.finish_reason = Some(finish.to_string());

            if let Some(u) = chunk.get("usage") {
                if let Some(new_u) = to_openai_usage(FORMAT_CLAUDE, Some(u)) {
                    // Merge input tokens from message_start if output-only in message_delta
                    if let Some(prev) = &state.usage {
                        let prev_prompt = prev
                            .get("prompt_tokens")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        let new_prompt = new_u
                            .get("prompt_tokens")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        let new_comp = new_u
                            .get("completion_tokens")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        let cached = new_u
                            .get("prompt_tokens_details")
                            .and_then(|d| d.get("cached_tokens"))
                            .and_then(|v| v.as_u64())
                            .or_else(|| {
                                prev.get("prompt_tokens_details")
                                    .and_then(|d| d.get("cached_tokens"))
                                    .and_then(|v| v.as_u64())
                            })
                            .unwrap_or(0);
                        let prompt = if new_prompt > 0 { new_prompt } else { prev_prompt };
                        state.usage = Some(crate::translator::concerns::build_usage(
                            prompt, new_comp, cached, 0,
                        ));
                    } else {
                        state.usage = Some(new_u);
                    }
                }
            }

            let id = state
                .message_id
                .clone()
                .unwrap_or_else(|| format!("chatcmpl-{}", created));
            let model = state
                .model
                .clone()
                .unwrap_or_else(|| defaults::MODEL_FALLBACK.to_string());
            state.finish_reason_sent = true;
            vec![build_chunk(
                &id,
                &model,
                created,
                json!({}),
                Some(finish),
                state.usage.clone(),
            )]
        }

        claude_event::MESSAGE_STOP => {
            if !state.finish_reason_sent {
                state.finish_reason_sent = true;
                let id = state
                    .message_id
                    .clone()
                    .unwrap_or_else(|| format!("chatcmpl-{}", created));
                let model = state
                    .model
                    .clone()
                    .unwrap_or_else(|| defaults::MODEL_FALLBACK.to_string());
                let finish = state
                    .finish_reason
                    .clone()
                    .unwrap_or_else(|| openai_finish::STOP.to_string());
                return vec![build_chunk(
                    &id,
                    &model,
                    created,
                    json!({}),
                    Some(&finish),
                    state.usage.clone(),
                )];
            }
            vec![]
        }

        _ => vec![],
    }
}

// ============================================================================
// Response (SSE Chunk): OpenAI -> Claude
// ============================================================================

pub fn openai_to_claude_response(chunk: Option<&Value>, state: &mut TranslateState) -> Vec<Value> {
    let mut results: Vec<Value> = Vec::new();

    if chunk.is_none() {
        if state.started && !state.finished {
            close_open_claude_blocks(state, &mut results);
            let stop = state
                .finish_reason
                .clone()
                .unwrap_or_else(|| claude_stop::END_TURN.to_string());
            let mut msg_delta = json!({
                "type": claude_event::MESSAGE_DELTA,
                "delta": { "stop_reason": stop, "stop_sequence": null }
            });
            if let Some(u) = &state.usage {
                msg_delta["usage"] = claude_usage_from_openai(u);
            }
            results.push(msg_delta);
            results.push(json!({ "type": claude_event::MESSAGE_STOP }));
            state.finished = true;
        }
        return results;
    }

    let chunk = chunk.unwrap();
    if state.message_id.is_none() {
        if let Some(id) = chunk.get("id").and_then(|v| v.as_str()) {
            state.message_id = Some(id.to_string());
        }
    }
    if state.model.is_none() {
        if let Some(m) = chunk.get("model").and_then(|v| v.as_str()) {
            state.model = Some(m.to_string());
        }
    }

    if let Some(u) = chunk.get("usage") {
        if u.is_object() {
            state.usage = Some(u.clone());
        }
    }

    if !state.started {
        state.started = true;
        let id = state
            .message_id
            .clone()
            .unwrap_or_else(|| format!("msg_{}", now_unix_secs()));
        let model = state
            .model
            .clone()
            .unwrap_or_else(|| defaults::MODEL_FALLBACK.to_string());
        results.push(json!({
            "type": claude_event::MESSAGE_START,
            "message": {
                "id": id,
                "type": "message",
                "role": role::ASSISTANT,
                "content": [],
                "model": model,
                "stop_reason": null,
                "stop_sequence": null,
                "usage": { "input_tokens": 0, "output_tokens": 0 }
            }
        }));
    }

    let choice = chunk
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first());

    if let Some(ch) = choice {
        if let Some(delta) = ch.get("delta") {
            let reasoning = extract_reasoning_text(delta);
            if !reasoning.is_empty() {
                if !state.thinking_block_started {
                    let idx = state.claude_next_block_index;
                    state.claude_next_block_index += 1;
                    state.thinking_block_started = true;
                    state.thinking_block_index = Some(idx);
                    results.push(json!({
                        "type": claude_event::CONTENT_BLOCK_START,
                        "index": idx,
                        "content_block": { "type": claude_block::THINKING, "thinking": "" }
                    }));
                }
                let idx = state.thinking_block_index.unwrap_or(0);
                results.push(json!({
                    "type": claude_event::CONTENT_BLOCK_DELTA,
                    "index": idx,
                    "delta": { "type": claude_delta::THINKING_DELTA, "thinking": reasoning }
                }));
            }

            if let Some(text) = delta.get("content").and_then(|v| v.as_str()) {
                if !text.is_empty() {
                    if state.thinking_block_started && !state.thinking_block_closed {
                        if let Some(t_idx) = state.thinking_block_index {
                            results.push(json!({
                                "type": claude_event::CONTENT_BLOCK_STOP,
                                "index": t_idx
                            }));
                        }
                        state.thinking_block_closed = true;
                    }
                    if !state.text_block_started {
                        let idx = state.claude_next_block_index;
                        state.claude_next_block_index += 1;
                        state.text_block_started = true;
                        state.text_block_index = Some(idx);
                        results.push(json!({
                            "type": claude_event::CONTENT_BLOCK_START,
                            "index": idx,
                            "content_block": { "type": claude_block::TEXT, "text": "" }
                        }));
                    }
                    let idx = state.text_block_index.unwrap_or(0);
                    results.push(json!({
                        "type": claude_event::CONTENT_BLOCK_DELTA,
                        "index": idx,
                        "delta": { "type": claude_delta::TEXT_DELTA, "text": text }
                    }));
                }
            }

            if let Some(tcs) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                if state.thinking_block_started && !state.thinking_block_closed {
                    if let Some(t_idx) = state.thinking_block_index {
                        results.push(json!({
                            "type": claude_event::CONTENT_BLOCK_STOP,
                            "index": t_idx
                        }));
                    }
                    state.thinking_block_closed = true;
                }
                if state.text_block_started && !state.text_block_closed {
                    if let Some(tx_idx) = state.text_block_index {
                        results.push(json!({
                            "type": claude_event::CONTENT_BLOCK_STOP,
                            "index": tx_idx
                        }));
                    }
                    state.text_block_closed = true;
                }

                for tc in tcs {
                    let tc_idx = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    if let Some(tc_id) = tc.get("id").and_then(|v| v.as_str()) {
                        let block_idx = state.claude_next_block_index;
                        state.claude_next_block_index += 1;
                        let name = tc
                            .get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        state.tool_calls_map.insert(
                            tc_idx,
                            json!({ "block_index": block_idx, "id": tc_id, "name": name }),
                        );
                        results.push(json!({
                            "type": claude_event::CONTENT_BLOCK_START,
                            "index": block_idx,
                            "content_block": {
                                "type": claude_block::TOOL_USE,
                                "id": tc_id,
                                "name": name,
                                "input": {}
                            }
                        }));
                    }
                    if let Some(args) = tc
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(|v| v.as_str())
                    {
                        if !args.is_empty() {
                            let block_idx = state
                                .tool_calls_map
                                .get(&tc_idx)
                                .and_then(|m| m.get("block_index"))
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0) as usize;
                            results.push(json!({
                                "type": claude_event::CONTENT_BLOCK_DELTA,
                                "index": block_idx,
                                "delta": { "type": claude_delta::INPUT_JSON_DELTA, "partial_json": args }
                            }));
                        }
                    }
                }
            }
        }

        if let Some(fr) = ch.get("finish_reason").and_then(|v| v.as_str()) {
            let stop = from_openai_finish(FORMAT_CLAUDE, Some(fr));
            state.finish_reason = Some(stop.to_string());
            close_open_claude_blocks(state, &mut results);
            let mut msg_delta = json!({
                "type": claude_event::MESSAGE_DELTA,
                "delta": { "stop_reason": stop, "stop_sequence": null }
            });
            if let Some(u) = &state.usage {
                msg_delta["usage"] = claude_usage_from_openai(u);
            }
            results.push(msg_delta);
            results.push(json!({ "type": claude_event::MESSAGE_STOP }));
            state.finished = true;
        }
    }

    results
}

fn close_open_claude_blocks(state: &mut TranslateState, results: &mut Vec<Value>) {
    if state.thinking_block_started && !state.thinking_block_closed {
        if let Some(idx) = state.thinking_block_index {
            results.push(json!({ "type": claude_event::CONTENT_BLOCK_STOP, "index": idx }));
        }
        state.thinking_block_closed = true;
    }
    if state.text_block_started && !state.text_block_closed {
        if let Some(idx) = state.text_block_index {
            results.push(json!({ "type": claude_event::CONTENT_BLOCK_STOP, "index": idx }));
        }
        state.text_block_closed = true;
    }
    let mut tool_block_indices: Vec<usize> = state
        .tool_calls_map
        .values()
        .filter_map(|v| v.get("block_index").and_then(|i| i.as_u64()).map(|i| i as usize))
        .collect();
    tool_block_indices.sort_unstable();
    for idx in tool_block_indices {
        results.push(json!({ "type": claude_event::CONTENT_BLOCK_STOP, "index": idx }));
    }
    state.tool_calls_map.clear();
}

fn claude_usage_from_openai(u: &Value) -> Value {
    let prompt = u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
    let completion = u
        .get("completion_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let cached = u
        .get("prompt_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    json!({
        "input_tokens": prompt.saturating_sub(cached),
        "output_tokens": completion,
        "cache_read_input_tokens": cached
    })
}
