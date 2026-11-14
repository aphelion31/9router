use serde_json::{json, Map, Value};

use crate::translator::concerns::{
    build_chunk, collapse_text_parts, ensure_tool_call_ids, extract_reasoning_text,
    fix_missing_tool_responses, now_unix_secs, reasoning_delta,
    to_openai_finish, to_openai_usage,
};
use crate::translator::formats::FORMAT_OPENAI_RESPONSES;
use crate::translator::schema::{
    defaults, openai_block, openai_finish, responses_event, responses_incomplete_reason,
    responses_item, responses_status, role,
};
use crate::translator::{ResponsesToolCallState, TranslateState};

// ============================================================================
// Request: OpenAI Responses -> OpenAI Chat Completions
// ============================================================================

pub fn openai_responses_to_openai_request(
    model: &str,
    body: &Value,
    stream: bool,
    _credentials: Option<&Value>,
) -> Value {
    let mut result = Map::new();
    result.insert("model".to_string(), Value::String(model.to_string()));
    result.insert("stream".to_string(), Value::Bool(stream));

    if let Some(v) = body.get("temperature") {
        result.insert("temperature".to_string(), v.clone());
    }
    if let Some(v) = body.get("top_p") {
        result.insert("top_p".to_string(), v.clone());
    }
    if let Some(v) = body.get("max_output_tokens").or_else(|| body.get("max_tokens")) {
        result.insert("max_tokens".to_string(), v.clone());
    }
    if let Some(effort) = body
        .get("reasoning")
        .and_then(|r| r.get("effort"))
        .and_then(|v| v.as_str())
    {
        result.insert("reasoning_effort".to_string(), json!(effort));
    }

    let mut messages: Vec<Value> = Vec::new();
    if let Some(inst) = body.get("instructions").and_then(|v| v.as_str()) {
        if !inst.trim().is_empty() {
            messages.push(json!({ "role": role::SYSTEM, "content": inst }));
        }
    }

    if let Some(input) = body.get("input") {
        if let Some(s) = input.as_str() {
            messages.push(json!({ "role": role::USER, "content": s }));
        } else if let Some(items) = input.as_array() {
            let mut pending_tool_calls: Vec<Value> = Vec::new();

            let flush_tool_calls = |msgs: &mut Vec<Value>, tcs: &mut Vec<Value>| {
                if !tcs.is_empty() {
                    msgs.push(json!({
                        "role": role::ASSISTANT,
                        "content": null,
                        "tool_calls": std::mem::take(tcs)
                    }));
                }
            };

            for item in items {
                let itype = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match itype {
                    responses_item::FUNCTION_CALL => {
                        let call_id = item
                            .get("call_id")
                            .or_else(|| item.get("id"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        let args = match item.get("arguments") {
                            Some(Value::String(s)) => s.clone(),
                            Some(v) => serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string()),
                            None => "{}".to_string(),
                        };
                        pending_tool_calls.push(json!({
                            "id": call_id,
                            "type": openai_block::FUNCTION,
                            "function": { "name": name, "arguments": args }
                        }));
                    }
                    responses_item::FUNCTION_CALL_OUTPUT => {
                        flush_tool_calls(&mut messages, &mut pending_tool_calls);
                        let call_id = item.get("call_id").and_then(|v| v.as_str()).unwrap_or("");
                        let output_str = match item.get("output") {
                            Some(Value::String(s)) => s.clone(),
                            Some(v) if !v.is_null() => {
                                serde_json::to_string(v).unwrap_or_default()
                            }
                            _ => String::new(),
                        };
                        messages.push(json!({
                            "role": role::TOOL,
                            "tool_call_id": call_id,
                            "content": output_str
                        }));
                    }
                    responses_item::REASONING => {}
                    _ => {
                        flush_tool_calls(&mut messages, &mut pending_tool_calls);
                        let msg_role = item
                            .get("role")
                            .and_then(|v| v.as_str())
                            .unwrap_or(role::USER);
                        let normalized_role = if msg_role == role::DEVELOPER {
                            role::SYSTEM
                        } else {
                            msg_role
                        };
                        if let Some(content) = item.get("content") {
                            if let Some(s) = content.as_str() {
                                messages.push(json!({ "role": normalized_role, "content": s }));
                            } else if let Some(arr) = content.as_array() {
                                let parts: Vec<Value> = arr
                                    .iter()
                                    .filter_map(|p| {
                                        let pt = p.get("type").and_then(|v| v.as_str()).unwrap_or("");
                                        if pt == responses_item::INPUT_TEXT
                                            || pt == responses_item::OUTPUT_TEXT
                                            || pt == openai_block::TEXT
                                        {
                                            Some(json!({
                                                "type": openai_block::TEXT,
                                                "text": p.get("text").and_then(|v| v.as_str()).unwrap_or("")
                                            }))
                                        } else if pt == responses_item::INPUT_IMAGE
                                            || pt == openai_block::IMAGE_URL
                                        {
                                            let url = p
                                                .get("image_url")
                                                .and_then(|v| v.as_str())
                                                .or_else(|| {
                                                    p.get("image_url")
                                                        .and_then(|i| i.get("url"))
                                                        .and_then(|v| v.as_str())
                                                })
                                                .unwrap_or("");
                                            Some(json!({
                                                "type": openai_block::IMAGE_URL,
                                                "image_url": { "url": url }
                                            }))
                                        } else {
                                            None
                                        }
                                    })
                                    .collect();
                                let collapsed = collapse_text_parts(&Value::Array(parts));
                                messages.push(json!({
                                    "role": normalized_role,
                                    "content": collapsed
                                }));
                            }
                        }
                    }
                }
            }
            flush_tool_calls(&mut messages, &mut pending_tool_calls);
        }
    }

    result.insert("messages".to_string(), Value::Array(messages));

    if let Some(tools) = body.get("tools").and_then(|v| v.as_array()) {
        let openai_tools: Vec<Value> = tools
            .iter()
            .filter_map(|t| {
                let ttype = t.get("type").and_then(|v| v.as_str()).unwrap_or("");
                if ttype == openai_block::FUNCTION || t.get("name").is_some() {
                    let f = t.get("function").unwrap_or(t);
                    let name = f.get("name").and_then(|v| v.as_str())?;
                    Some(json!({
                        "type": openai_block::FUNCTION,
                        "function": {
                            "name": name,
                            "description": f.get("description").cloned().unwrap_or(json!("")),
                            "parameters": f.get("parameters").cloned().unwrap_or_else(|| json!({ "type": "object", "properties": {} }))
                        }
                    }))
                } else {
                    None
                }
            })
            .collect();
        if !openai_tools.is_empty() {
            result.insert("tools".to_string(), Value::Array(openai_tools));
        }
    }

    if let Some(tc) = body.get("tool_choice") {
        if tc.is_string() {
            result.insert("tool_choice".to_string(), tc.clone());
        } else if tc.is_object() {
            if let Some(name) = tc.get("name").and_then(|v| v.as_str()) {
                result.insert(
                    "tool_choice".to_string(),
                    json!({ "type": openai_block::FUNCTION, "function": { "name": name } }),
                );
            }
        }
    }

    Value::Object(result)
}

// ============================================================================
// Request: OpenAI Chat Completions -> OpenAI Responses
// ============================================================================

fn clamp_call_id(id: &str) -> String {
    if id.len() <= 64 {
        id.to_string()
    } else {
        id[..64].to_string()
    }
}

pub fn openai_to_openai_responses_request(
    model: &str,
    body: &Value,
    stream: bool,
    _credentials: Option<&Value>,
) -> Value {
    let mut result = Map::new();
    result.insert("model".to_string(), Value::String(model.to_string()));
    result.insert("stream".to_string(), Value::Bool(stream));
    result.insert("store".to_string(), Value::Bool(false));

    if let Some(v) = body.get("temperature") {
        result.insert("temperature".to_string(), v.clone());
    }
    if let Some(v) = body.get("top_p") {
        result.insert("top_p".to_string(), v.clone());
    }
    if let Some(v) = body
        .get("max_tokens")
        .or_else(|| body.get("max_completion_tokens"))
    {
        result.insert("max_output_tokens".to_string(), v.clone());
    }
    if let Some(effort) = body.get("reasoning_effort").and_then(|v| v.as_str()) {
        result.insert("reasoning".to_string(), json!({ "effort": effort }));
    } else if let Some(r) = body.get("reasoning") {
        result.insert("reasoning".to_string(), r.clone());
    }

    let mut raw_messages = body
        .get("messages")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    ensure_tool_call_ids(&mut raw_messages);
    let fixed_messages = fix_missing_tool_responses(&raw_messages);

    let mut system_parts: Vec<String> = Vec::new();
    let mut input_items: Vec<Value> = Vec::new();

    for msg in &fixed_messages {
        let r = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
        match r {
            role::SYSTEM | role::DEVELOPER => {
                if let Some(c) = msg.get("content") {
                    if let Some(s) = c.as_str() {
                        if !s.is_empty() {
                            system_parts.push(s.to_string());
                        }
                    } else if let Some(arr) = c.as_array() {
                        for p in arr {
                            if let Some(t) = p.get("text").and_then(|v| v.as_str()) {
                                if !t.is_empty() {
                                    system_parts.push(t.to_string());
                                }
                            }
                        }
                    }
                }
            }
            role::USER => {
                let mut parts: Vec<Value> = Vec::new();
                if let Some(c) = msg.get("content") {
                    if let Some(s) = c.as_str() {
                        if !s.is_empty() {
                            parts.push(json!({ "type": responses_item::INPUT_TEXT, "text": s }));
                        }
                    } else if let Some(arr) = c.as_array() {
                        for p in arr {
                            let pt = p.get("type").and_then(|v| v.as_str()).unwrap_or("");
                            if pt == openai_block::TEXT {
                                if let Some(t) = p.get("text").and_then(|v| v.as_str()) {
                                    parts.push(json!({ "type": responses_item::INPUT_TEXT, "text": t }));
                                }
                            } else if pt == openai_block::IMAGE_URL {
                                let url = p
                                    .get("image_url")
                                    .and_then(|i| i.get("url"))
                                    .and_then(|v| v.as_str())
                                    .or_else(|| p.get("image_url").and_then(|v| v.as_str()))
                                    .unwrap_or("");
                                if !url.is_empty() {
                                    parts.push(json!({ "type": responses_item::INPUT_IMAGE, "image_url": url }));
                                }
                            }
                        }
                    }
                }
                if !parts.is_empty() {
                    input_items.push(json!({
                        "type": responses_item::MESSAGE,
                        "role": role::USER,
                        "content": parts
                    }));
                }
            }
            role::ASSISTANT => {
                if let Some(c) = msg.get("content") {
                    if let Some(s) = c.as_str() {
                        if !s.is_empty() {
                            input_items.push(json!({
                                "type": responses_item::MESSAGE,
                                "role": role::ASSISTANT,
                                "content": [{ "type": responses_item::OUTPUT_TEXT, "text": s }]
                            }));
                        }
                    }
                }
                if let Some(tcs) = msg.get("tool_calls").and_then(|v| v.as_array()) {
                    for tc in tcs {
                        let raw_id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("");
                        let call_id = clamp_call_id(raw_id);
                        let name = tc
                            .get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let args = match tc.get("function").and_then(|f| f.get("arguments")) {
                            Some(Value::String(s)) => s.clone(),
                            Some(v) => serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string()),
                            None => "{}".to_string(),
                        };
                        input_items.push(json!({
                            "type": responses_item::FUNCTION_CALL,
                            "call_id": call_id,
                            "name": name,
                            "arguments": args
                        }));
                    }
                }
            }
            role::TOOL => {
                let raw_id = msg
                    .get("tool_call_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let call_id = clamp_call_id(raw_id);
                let output = match msg.get("content") {
                    Some(Value::String(s)) => s.clone(),
                    Some(v) if !v.is_null() => serde_json::to_string(v).unwrap_or_default(),
                    _ => String::new(),
                };
                input_items.push(json!({
                    "type": responses_item::FUNCTION_CALL_OUTPUT,
                    "call_id": call_id,
                    "output": output
                }));
            }
            _ => {}
        }
    }

    if !system_parts.is_empty() {
        result.insert(
            "instructions".to_string(),
            Value::String(system_parts.join("\n\n")),
        );
    }
    result.insert("input".to_string(), Value::Array(input_items));

    if let Some(tools) = body.get("tools").and_then(|v| v.as_array()) {
        let resp_tools: Vec<Value> = tools
            .iter()
            .filter_map(|t| {
                let f = t.get("function").unwrap_or(t);
                let name = f.get("name").and_then(|v| v.as_str())?;
                Some(json!({
                    "type": openai_block::FUNCTION,
                    "name": name,
                    "description": f.get("description").cloned().unwrap_or(json!("")),
                    "parameters": f.get("parameters").cloned().unwrap_or_else(|| json!({ "type": "object", "properties": {} }))
                }))
            })
            .collect();
        if !resp_tools.is_empty() {
            result.insert("tools".to_string(), Value::Array(resp_tools));
        }
    }

    if let Some(tc) = body.get("tool_choice") {
        if tc.is_string() {
            result.insert("tool_choice".to_string(), tc.clone());
        } else if tc.is_object() {
            if let Some(name) = tc
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(|v| v.as_str())
            {
                result.insert(
                    "tool_choice".to_string(),
                    json!({ "type": openai_block::FUNCTION, "name": name }),
                );
            }
        }
    }

    Value::Object(result)
}

// ============================================================================
// Response (SSE Chunk): OpenAI Responses -> OpenAI Chat Completions
// ============================================================================

pub fn openai_responses_to_openai_response(
    chunk: Option<&Value>,
    state: &mut TranslateState,
) -> Vec<Value> {
    let Some(chunk) = chunk else {
        return vec![];
    };
    let ev_type = chunk.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let created = *state.created.get_or_insert_with(now_unix_secs);

    if state.message_id.is_none() {
        if let Some(id) = chunk
            .get("response")
            .and_then(|r| r.get("id"))
            .and_then(|v| v.as_str())
        {
            state.message_id = Some(id.to_string());
        }
    }
    if state.model.is_none() {
        if let Some(m) = chunk
            .get("response")
            .and_then(|r| r.get("model"))
            .and_then(|v| v.as_str())
        {
            state.model = Some(m.to_string());
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

    match ev_type {
        responses_event::OUTPUT_TEXT_DELTA => {
            let text = chunk.get("delta").and_then(|v| v.as_str()).unwrap_or("");
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

        responses_event::REASONING_SUMMARY_DELTA | responses_event::REASONING_DELTA => {
            let text = chunk.get("delta").and_then(|v| v.as_str()).unwrap_or("");
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

        responses_event::OUTPUT_ITEM_ADDED => {
            let item = chunk.get("item").unwrap_or(&Value::Null);
            if item.get("type").and_then(|v| v.as_str()) == Some(responses_item::FUNCTION_CALL) {
                let tc_idx = state.tool_call_index;
                state.tool_call_index += 1;
                state.has_tool_calls = true;
                let call_id = item
                    .get("call_id")
                    .or_else(|| item.get("id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
                state.current_tool_call_id = Some(call_id.to_string());
                state.current_tool_call_name = Some(name.to_string());

                let mut delta = json!({
                    "tool_calls": [{
                        "index": tc_idx,
                        "id": call_id,
                        "type": openai_block::FUNCTION,
                        "function": {
                            "name": name,
                            "arguments": ""
                        }
                    }]
                });
                if !state.started {
                    state.started = true;
                    delta["role"] = json!(role::ASSISTANT);
                }
                return vec![build_chunk(&id, &model, created, delta, None, None)];
            }
            vec![]
        }

        responses_event::FUNCTION_CALL_ARGS_DELTA => {
            let args = chunk.get("delta").and_then(|v| v.as_str()).unwrap_or("");
            if args.is_empty() {
                return vec![];
            }
            let tc_idx = state.tool_call_index.saturating_sub(1);
            vec![build_chunk(
                &id,
                &model,
                created,
                json!({
                    "tool_calls": [{
                        "index": tc_idx,
                        "function": { "arguments": args }
                    }]
                }),
                None,
                None,
            )]
        }

        responses_event::RESPONSE_COMPLETED
        | responses_event::RESPONSE_DONE
        | responses_event::RESPONSE_INCOMPLETE
        | responses_event::RESPONSE_FAILED => {
            let resp = chunk.get("response").unwrap_or(&Value::Null);
            let status = resp
                .get("status")
                .and_then(|v| v.as_str())
                .or(Some(responses_status::COMPLETED));
            let finish = to_openai_finish(FORMAT_OPENAI_RESPONSES, status, state.has_tool_calls);
            let usage = to_openai_usage(FORMAT_OPENAI_RESPONSES, resp.get("usage"));
            state.finish_reason_sent = true;
            vec![build_chunk(
                &id,
                &model,
                created,
                json!({}),
                Some(finish),
                usage,
            )]
        }

        responses_event::ERROR => {
            let msg = chunk
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown error");
            vec![json!({
                "id": id,
                "object": "chat.completion.chunk",
                "created": created,
                "model": model,
                "error": { "message": msg }
            })]
        }

        _ => vec![],
    }
}

// ============================================================================
// Response (SSE Chunk): OpenAI Chat Completions -> OpenAI Responses
// ============================================================================

fn next_seq_num(state: &mut TranslateState) -> usize {
    let n = state.responses_seq;
    state.responses_seq += 1;
    n
}

pub fn openai_to_openai_responses_response(
    chunk: Option<&Value>,
    state: &mut TranslateState,
) -> Vec<Value> {
    let mut results: Vec<Value> = Vec::new();

    if chunk.is_none() {
        if state.responses_started && !state.responses_completed_sent {
            finalize_responses_stream(state, openai_finish::STOP, &mut results);
        }
        return results;
    }

    let chunk = chunk.unwrap();
    if state.responses_response_id.is_none() {
        let id = chunk
            .get("id")
            .and_then(|v| v.as_str())
            .map(|s| format!("resp_{}", s))
            .unwrap_or_else(|| format!("resp_{}", now_unix_secs()));
        state.responses_response_id = Some(id);
    }
    if state.model.is_none() {
        if let Some(m) = chunk.get("model").and_then(|v| v.as_str()) {
            state.model = Some(m.to_string());
        }
    }
    if let Some(u) = chunk.get("usage") {
        if u.is_object() {
            state.responses_usage = Some(u.clone());
        }
    }

    let resp_id = state
        .responses_response_id
        .clone()
        .unwrap_or_else(|| "resp_0".to_string());
    let model = state
        .model
        .clone()
        .unwrap_or_else(|| defaults::MODEL_FALLBACK.to_string());

    if !state.responses_started {
        state.responses_started = true;
        let seq0 = next_seq_num(state);
        results.push(json!({
            "type": responses_event::RESPONSE_CREATED,
            "sequence_number": seq0,
            "response": {
                "id": resp_id,
                "object": "response",
                "status": responses_status::IN_PROGRESS,
                "model": model,
                "output": []
            }
        }));
    }

    let choice = chunk
        .get("choices")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first());
    let Some(ch) = choice else {
        return results;
    };

    if let Some(delta) = ch.get("delta") {
        let reasoning = extract_reasoning_text(delta);
        if !reasoning.is_empty() {
            let seq = next_seq_num(state);
            state.responses_reasoning_buffer.push_str(&reasoning);
            results.push(json!({
                "type": responses_event::REASONING_SUMMARY_DELTA,
                "sequence_number": seq,
                "delta": reasoning
            }));
        }

        if let Some(text) = delta.get("content").and_then(|v| v.as_str()) {
            if !text.is_empty() {
                if !state.responses_text_started {
                    state.responses_text_started = true;
                    let item_id = format!("msg_{}", now_unix_secs());
                    state.responses_item_id = Some(item_id.clone());
                    let seq_a = next_seq_num(state);
                    results.push(json!({
                        "type": responses_event::OUTPUT_ITEM_ADDED,
                        "sequence_number": seq_a,
                        "output_index": state.responses_output_index,
                        "item": {
                            "id": item_id,
                            "type": responses_item::MESSAGE,
                            "status": responses_status::IN_PROGRESS,
                            "role": role::ASSISTANT,
                            "content": []
                        }
                    }));
                    let seq_p = next_seq_num(state);
                    results.push(json!({
                        "type": responses_event::CONTENT_PART_ADDED,
                        "sequence_number": seq_p,
                        "item_id": item_id,
                        "output_index": state.responses_output_index,
                        "content_index": 0,
                        "part": { "type": responses_item::OUTPUT_TEXT, "text": "" }
                    }));
                }
                state.responses_text_buffer.push_str(text);
                let item_id = state
                    .responses_item_id
                    .clone()
                    .unwrap_or_else(|| "msg_0".to_string());
                let seq = next_seq_num(state);
                results.push(json!({
                    "type": responses_event::OUTPUT_TEXT_DELTA,
                    "sequence_number": seq,
                    "item_id": item_id,
                    "output_index": state.responses_output_index,
                    "content_index": 0,
                    "delta": text
                }));
            }
        }

        if let Some(tcs) = delta.get("tool_calls").and_then(|v| v.as_array()) {
            if state.responses_text_started && !state.responses_text_done {
                close_responses_text_item(state, &mut results);
            }
            for tc in tcs {
                let tc_idx = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                if let Some(call_id) = tc.get("id").and_then(|v| v.as_str()) {
                    let name = tc
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let out_idx = state.responses_output_index;
                    state.responses_output_index += 1;
                    let item_id = format!("fc_{}", call_id);
                    state.responses_tool_calls.insert(
                        tc_idx,
                        ResponsesToolCallState {
                            item_id: item_id.clone(),
                            call_id: call_id.to_string(),
                            name: name.clone(),
                            arguments: String::new(),
                            output_index: out_idx,
                        },
                    );
                    let seq = next_seq_num(state);
                    results.push(json!({
                        "type": responses_event::OUTPUT_ITEM_ADDED,
                        "sequence_number": seq,
                        "output_index": out_idx,
                        "item": {
                            "id": item_id,
                            "type": responses_item::FUNCTION_CALL,
                            "status": responses_status::IN_PROGRESS,
                            "call_id": call_id,
                            "name": name,
                            "arguments": ""
                        }
                    }));
                }
                if let Some(args) = tc
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(|v| v.as_str())
                {
                    if !args.is_empty() {
                        if let Some(tc_state) = state.responses_tool_calls.get_mut(&tc_idx) {
                            tc_state.arguments.push_str(args);
                            let item_id = tc_state.item_id.clone();
                            let out_idx = tc_state.output_index;
                            let seq = next_seq_num(state);
                            results.push(json!({
                                "type": responses_event::FUNCTION_CALL_ARGS_DELTA,
                                "sequence_number": seq,
                                "item_id": item_id,
                                "output_index": out_idx,
                                "delta": args
                            }));
                        }
                    }
                }
            }
        }
    }

    if let Some(fr) = ch.get("finish_reason").and_then(|v| v.as_str()) {
        finalize_responses_stream(state, fr, &mut results);
    }

    results
}

fn close_responses_text_item(state: &mut TranslateState, results: &mut Vec<Value>) {
    if !state.responses_text_started || state.responses_text_done {
        return;
    }
    state.responses_text_done = true;
    let item_id = state
        .responses_item_id
        .clone()
        .unwrap_or_else(|| "msg_0".to_string());
    let text = state.responses_text_buffer.clone();
    let out_idx = state.responses_output_index;
    state.responses_output_index += 1;

    let seq1 = next_seq_num(state);
    results.push(json!({
        "type": responses_event::OUTPUT_TEXT_DONE,
        "sequence_number": seq1,
        "item_id": item_id,
        "output_index": out_idx,
        "content_index": 0,
        "text": text
    }));
    let seq2 = next_seq_num(state);
    results.push(json!({
        "type": responses_event::OUTPUT_ITEM_DONE,
        "sequence_number": seq2,
        "output_index": out_idx,
        "item": {
            "id": item_id,
            "type": responses_item::MESSAGE,
            "status": responses_status::COMPLETED,
            "role": role::ASSISTANT,
            "content": [{ "type": responses_item::OUTPUT_TEXT, "text": text }]
        }
    }));
}

fn finalize_responses_stream(
    state: &mut TranslateState,
    finish_reason: &str,
    results: &mut Vec<Value>,
) {
    if state.responses_completed_sent {
        return;
    }
    state.responses_completed_sent = true;

    if state.responses_text_started && !state.responses_text_done {
        close_responses_text_item(state, results);
    }

    let mut tc_entries: Vec<(usize, ResponsesToolCallState)> = state
        .responses_tool_calls
        .drain()
        .collect();
    tc_entries.sort_by_key(|(k, _)| *k);

    let mut output_items: Vec<Value> = Vec::new();
    if state.responses_text_started {
        output_items.push(json!({
            "id": state.responses_item_id.clone().unwrap_or_else(|| "msg_0".to_string()),
            "type": responses_item::MESSAGE,
            "status": responses_status::COMPLETED,
            "role": role::ASSISTANT,
            "content": [{ "type": responses_item::OUTPUT_TEXT, "text": state.responses_text_buffer }]
        }));
    }

    for (_, tc) in tc_entries {
        let seq1 = next_seq_num(state);
        results.push(json!({
            "type": responses_event::FUNCTION_CALL_ARGS_DONE,
            "sequence_number": seq1,
            "item_id": tc.item_id,
            "output_index": tc.output_index,
            "arguments": tc.arguments
        }));
        let item = json!({
            "id": tc.item_id,
            "type": responses_item::FUNCTION_CALL,
            "status": responses_status::COMPLETED,
            "call_id": tc.call_id,
            "name": tc.name,
            "arguments": tc.arguments
        });
        let seq2 = next_seq_num(state);
        results.push(json!({
            "type": responses_event::OUTPUT_ITEM_DONE,
            "sequence_number": seq2,
            "output_index": tc.output_index,
            "item": item
        }));
        output_items.push(item);
    }

    let is_incomplete = finish_reason == openai_finish::LENGTH;
    let status = if is_incomplete {
        responses_status::INCOMPLETE
    } else {
        responses_status::COMPLETED
    };
    let ev_type = if is_incomplete {
        responses_event::RESPONSE_INCOMPLETE
    } else {
        responses_event::RESPONSE_COMPLETED
    };

    let resp_id = state
        .responses_response_id
        .clone()
        .unwrap_or_else(|| "resp_0".to_string());
    let model = state
        .model
        .clone()
        .unwrap_or_else(|| defaults::MODEL_FALLBACK.to_string());

    let mut response_obj = json!({
        "id": resp_id,
        "object": "response",
        "status": status,
        "model": model,
        "output": output_items
    });
    if is_incomplete {
        response_obj["incomplete_details"] =
            json!({ "reason": responses_incomplete_reason::MAX_OUTPUT_TOKENS });
    }
    if let Some(u) = &state.responses_usage {
        let prompt = u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
        let completion = u
            .get("completion_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        response_obj["usage"] = json!({
            "input_tokens": prompt,
            "output_tokens": completion,
            "total_tokens": prompt + completion
        });
    }

    let seq = next_seq_num(state);
    results.push(json!({
        "type": ev_type,
        "sequence_number": seq,
        "response": response_obj
    }));
}
