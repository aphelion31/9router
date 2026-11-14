use serde_json::{json, Map, Value};
use std::collections::HashMap;

use crate::translator::concerns::{
    adjust_max_tokens, build_chunk, collapse_text_parts, encode_data_uri, ensure_tool_call_ids,
    extract_reasoning_text, fallback_tool_call_id, fix_missing_tool_responses, from_openai_finish,
    generate_tool_call_id, now_unix_secs, parse_data_uri, parse_json_or_empty, reasoning_delta,
    to_openai_finish, to_openai_usage,
};
use crate::translator::formats::{
    FORMAT_ANTIGRAVITY, FORMAT_GEMINI, FORMAT_GEMINI_CLI, FORMAT_VERTEX,
};
use crate::translator::schema::{defaults, gemini_role, openai_block, role};
use crate::translator::TranslateState;

// ============================================================================
// Request: Gemini -> OpenAI
// ============================================================================

pub fn gemini_to_openai_request(
    model: &str,
    body: &Value,
    stream: bool,
    _credentials: Option<&Value>,
) -> Value {
    let mut result = Map::new();
    result.insert("model".to_string(), Value::String(model.to_string()));
    result.insert("stream".to_string(), Value::Bool(stream));

    if let Some(gen_cfg) = body.get("generationConfig") {
        if let Some(v) = gen_cfg.get("temperature") {
            result.insert("temperature".to_string(), v.clone());
        }
        if let Some(v) = gen_cfg.get("topP") {
            result.insert("top_p".to_string(), v.clone());
        }
        if let Some(v) = gen_cfg.get("maxOutputTokens") {
            result.insert("max_tokens".to_string(), v.clone());
        }
        if let Some(v) = gen_cfg.get("stopSequences") {
            result.insert("stop".to_string(), v.clone());
        }
        if let Some(tc) = gen_cfg.get("thinkingConfig") {
            if let Some(level) = tc.get("thinkingLevel").and_then(|v| v.as_str()) {
                result.insert("reasoning_effort".to_string(), json!(level));
            } else if let Some(budget) = tc.get("thinkingBudget").and_then(|v| v.as_u64()) {
                let effort = if budget <= 1024 {
                    "low"
                } else if budget <= 8192 {
                    "medium"
                } else {
                    "high"
                };
                result.insert("reasoning_effort".to_string(), json!(effort));
            }
        }
    }

    let mut messages: Vec<Value> = Vec::new();
    let mut call_name_to_ids: HashMap<String, Vec<String>> = HashMap::new();

    if let Some(sys) = body.get("systemInstruction") {
        if let Some(parts) = sys.get("parts").and_then(|v| v.as_array()) {
            let texts: Vec<&str> = parts
                .iter()
                .filter_map(|p| p.get("text").and_then(|v| v.as_str()))
                .filter(|s| !s.is_empty())
                .collect();
            if !texts.is_empty() {
                messages.push(json!({
                    "role": role::SYSTEM,
                    "content": texts.join("\n")
                }));
            }
        }
    }

    if let Some(contents) = body.get("contents").and_then(|v| v.as_array()) {
        for item in contents {
            let g_role = item
                .get("role")
                .and_then(|v| v.as_str())
                .unwrap_or(gemini_role::USER);
            let Some(parts) = item.get("parts").and_then(|v| v.as_array()) else {
                continue;
            };

            if g_role == gemini_role::USER {
                let mut content_parts: Vec<Value> = Vec::new();
                let mut tool_msgs: Vec<Value> = Vec::new();

                for part in parts {
                    if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                        content_parts.push(json!({ "type": openai_block::TEXT, "text": text }));
                    } else if let Some(inline) = part.get("inlineData") {
                        let mime = inline.get("mimeType").and_then(|v| v.as_str());
                        let data = inline.get("data").and_then(|v| v.as_str()).unwrap_or("");
                        let url = encode_data_uri(mime, data);
                        content_parts.push(json!({
                            "type": openai_block::IMAGE_URL,
                            "image_url": { "url": url }
                        }));
                    } else if let Some(fr) = part.get("functionResponse") {
                        let name = fr.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        let tc_id = call_name_to_ids
                            .get_mut(name)
                            .and_then(|q| if q.is_empty() { None } else { Some(q.remove(0)) })
                            .unwrap_or_else(generate_tool_call_id);
                        let resp_val = fr.get("response").unwrap_or(&Value::Null);
                        let content_str = if let Some(s) = resp_val.get("result").and_then(|v| v.as_str()) {
                            s.to_string()
                        } else if let Some(s) = resp_val.as_str() {
                            s.to_string()
                        } else {
                            serde_json::to_string(resp_val).unwrap_or_default()
                        };
                        tool_msgs.push(json!({
                            "role": role::TOOL,
                            "tool_call_id": tc_id,
                            "content": content_str
                        }));
                    }
                }

                for tm in tool_msgs {
                    messages.push(tm);
                }
                if !content_parts.is_empty() {
                    let collapsed = collapse_text_parts(&Value::Array(content_parts));
                    messages.push(json!({ "role": role::USER, "content": collapsed }));
                }
            } else {
                // model -> assistant
                let mut text_parts: Vec<String> = Vec::new();
                let mut reasoning_parts: Vec<String> = Vec::new();
                let mut tool_calls: Vec<Value> = Vec::new();

                for part in parts {
                    let is_thought = part
                        .get("thought")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                        if is_thought {
                            reasoning_parts.push(text.to_string());
                        } else if !text.is_empty() {
                            text_parts.push(text.to_string());
                        }
                    } else if let Some(fc) = part.get("functionCall") {
                        let name = fc.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        let args_val = fc.get("args").cloned().unwrap_or_else(|| json!({}));
                        let args_str =
                            serde_json::to_string(&args_val).unwrap_or_else(|_| "{}".to_string());
                        let tc_id = generate_tool_call_id();
                        call_name_to_ids
                            .entry(name.to_string())
                            .or_default()
                            .push(tc_id.clone());
                        tool_calls.push(json!({
                            "id": tc_id,
                            "type": openai_block::FUNCTION,
                            "function": {
                                "name": name,
                                "arguments": args_str
                            }
                        }));
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
                    asst.insert("content".to_string(), json!(""));
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
        let mut openai_tools: Vec<Value> = Vec::new();
        for t in tools {
            if let Some(decls) = t.get("functionDeclarations").and_then(|v| v.as_array()) {
                for d in decls {
                    let name = d.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    if name.is_empty() {
                        continue;
                    }
                    openai_tools.push(json!({
                        "type": openai_block::FUNCTION,
                        "function": {
                            "name": name,
                            "description": d.get("description").cloned().unwrap_or(json!("")),
                            "parameters": d.get("parameters").cloned().unwrap_or_else(|| json!({ "type": "object", "properties": {} }))
                        }
                    }));
                }
            }
        }
        if !openai_tools.is_empty() {
            result.insert("tools".to_string(), Value::Array(openai_tools));
        }
    }

    Value::Object(result)
}

// ============================================================================
// Request: OpenAI -> Gemini / Gemini CLI / Antigravity
// ============================================================================

fn default_safety_settings() -> Value {
    json!([
        { "category": "HARM_CATEGORY_HARASSMENT", "threshold": "OFF" },
        { "category": "HARM_CATEGORY_HATE_SPEECH", "threshold": "OFF" },
        { "category": "HARM_CATEGORY_SEXUALLY_EXPLICIT", "threshold": "OFF" },
        { "category": "HARM_CATEGORY_DANGEROUS_CONTENT", "threshold": "OFF" },
        { "category": "HARM_CATEGORY_CIVIC_INTEGRITY", "threshold": "BLOCK_NONE" }
    ])
}

pub fn clean_json_schema_for_gemini(schema: &Value) -> Value {
    let Some(obj) = schema.as_object() else {
        return schema.clone();
    };
    let mut cleaned = Map::new();
    const UNSUPPORTED: &[&str] = &[
        "$schema",
        "additionalProperties",
        "default",
        "exclusiveMaximum",
        "exclusiveMinimum",
        "$defs",
        "definitions",
        "$id",
        "$ref",
        "title",
    ];
    for (k, v) in obj {
        if UNSUPPORTED.contains(&k.as_str()) {
            continue;
        }
        if k == "type" {
            if let Some(arr) = v.as_array() {
                if let Some(first_non_null) = arr
                    .iter()
                    .find_map(|x| x.as_str().filter(|s| *s != "null"))
                {
                    cleaned.insert(k.clone(), json!(first_non_null));
                }
                continue;
            }
        }
        if k == "properties" {
            if let Some(props) = v.as_object() {
                let mut new_props = Map::new();
                for (pk, pv) in props {
                    new_props.insert(pk.clone(), clean_json_schema_for_gemini(pv));
                }
                cleaned.insert(k.clone(), Value::Object(new_props));
                continue;
            }
        }
        if k == "items" {
            cleaned.insert(k.clone(), clean_json_schema_for_gemini(v));
            continue;
        }
        cleaned.insert(k.clone(), v.clone());
    }
    if !cleaned.contains_key("type") && cleaned.contains_key("properties") {
        cleaned.insert("type".to_string(), json!("object"));
    }
    Value::Object(cleaned)
}

fn convert_openai_content_to_gemini_parts(content: &Value) -> Vec<Value> {
    if let Some(s) = content.as_str() {
        if s.is_empty() {
            return vec![];
        }
        return vec![json!({ "text": s })];
    }
    let Some(arr) = content.as_array() else {
        return vec![];
    };
    let mut parts = Vec::new();
    for p in arr {
        let ptype = p.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match ptype {
            openai_block::TEXT => {
                if let Some(t) = p.get("text").and_then(|v| v.as_str()) {
                    if !t.is_empty() {
                        parts.push(json!({ "text": t }));
                    }
                }
            }
            openai_block::IMAGE_URL => {
                let url = p
                    .get("image_url")
                    .and_then(|i| i.get("url"))
                    .and_then(|v| v.as_str())
                    .or_else(|| p.get("image_url").and_then(|v| v.as_str()));
                if let Some(url) = url {
                    if let Some((mime, data)) = parse_data_uri(url) {
                        parts.push(json!({
                            "inlineData": { "mimeType": mime, "data": data }
                        }));
                    }
                }
            }
            _ => {}
        }
    }
    parts
}

fn build_gemini_core(model: &str, body: &Value, is_cli_or_ag: bool) -> Value {
    let mut req = Map::new();

    let mut raw_messages = body
        .get("messages")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    ensure_tool_call_ids(&mut raw_messages);
    let fixed_messages = fix_missing_tool_responses(&raw_messages);

    let mut id_to_name: HashMap<String, String> = HashMap::new();
    for msg in &fixed_messages {
        if msg.get("role").and_then(|v| v.as_str()) == Some(role::ASSISTANT) {
            if let Some(tcs) = msg.get("tool_calls").and_then(|v| v.as_array()) {
                for tc in tcs {
                    if let (Some(id), Some(name)) = (
                        tc.get("id").and_then(|v| v.as_str()),
                        tc.get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|v| v.as_str()),
                    ) {
                        id_to_name.insert(id.to_string(), name.to_string());
                    }
                }
            }
        }
    }

    let mut system_texts: Vec<String> = Vec::new();
    let mut contents: Vec<Value> = Vec::new();

    for msg in &fixed_messages {
        let r = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
        match r {
            role::SYSTEM | role::DEVELOPER => {
                if let Some(c) = msg.get("content") {
                    if let Some(s) = c.as_str() {
                        if !s.is_empty() {
                            system_texts.push(s.to_string());
                        }
                    } else if let Some(arr) = c.as_array() {
                        for p in arr {
                            if let Some(t) = p.get("text").and_then(|v| v.as_str()) {
                                if !t.is_empty() {
                                    system_texts.push(t.to_string());
                                }
                            }
                        }
                    }
                }
            }
            role::USER => {
                let mut parts = msg
                    .get("content")
                    .map(convert_openai_content_to_gemini_parts)
                    .unwrap_or_default();
                if parts.is_empty() {
                    parts.push(json!({ "text": "" }));
                }
                push_or_merge_gemini(&mut contents, gemini_role::USER, parts);
            }
            role::ASSISTANT => {
                let mut parts: Vec<Value> = Vec::new();
                if let Some(rc) = msg.get("reasoning_content").and_then(|v| v.as_str()) {
                    if !rc.is_empty() {
                        parts.push(json!({
                            "text": rc,
                            "thought": true,
                            "thoughtSignature": defaults::DEFAULT_THINKING_GEMINI_CLI_SIGNATURE
                        }));
                    }
                }
                if let Some(c) = msg.get("content") {
                    parts.extend(convert_openai_content_to_gemini_parts(c));
                }
                if let Some(tcs) = msg.get("tool_calls").and_then(|v| v.as_array()) {
                    for tc in tcs {
                        let name = tc
                            .get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let args = match tc.get("function").and_then(|f| f.get("arguments")) {
                            Some(Value::String(s)) => parse_json_or_empty(Some(s)),
                            Some(v) if v.is_object() => v.clone(),
                            _ => json!({}),
                        };
                        parts.push(json!({
                            "functionCall": { "name": name, "args": args },
                            "thoughtSignature": defaults::DEFAULT_THINKING_GEMINI_CLI_SIGNATURE
                        }));
                    }
                }
                if !parts.is_empty() {
                    push_or_merge_gemini(&mut contents, gemini_role::MODEL, parts);
                }
            }
            role::TOOL => {
                let tc_id = msg
                    .get("tool_call_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let name = id_to_name
                    .get(tc_id)
                    .map(|s| s.as_str())
                    .or_else(|| msg.get("name").and_then(|v| v.as_str()))
                    .unwrap_or("unknown");
                let raw_content = msg.get("content").unwrap_or(&Value::Null);
                let response_obj = if let Some(s) = raw_content.as_str() {
                    if let Ok(parsed) = serde_json::from_str::<Value>(s) {
                        if parsed.is_object() {
                            parsed
                        } else {
                            json!({ "result": s })
                        }
                    } else {
                        json!({ "result": s })
                    }
                } else if raw_content.is_object() {
                    raw_content.clone()
                } else {
                    json!({ "result": raw_content })
                };

                let part = json!({
                    "functionResponse": {
                        "name": name,
                        "response": response_obj
                    }
                });
                push_or_merge_gemini(&mut contents, gemini_role::USER, vec![part]);
            }
            _ => {}
        }
    }

    if !system_texts.is_empty() {
        req.insert(
            "systemInstruction".to_string(),
            json!({
                "role": gemini_role::USER,
                "parts": [{ "text": system_texts.join("\n") }]
            }),
        );
    }

    req.insert("contents".to_string(), Value::Array(contents));

    let mut gen_cfg = Map::new();
    gen_cfg.insert("maxOutputTokens".to_string(), json!(adjust_max_tokens(body)));
    if model.starts_with("gemini-4-") && model.ends_with("-high") {
        gen_cfg.insert("maxOutputTokens".to_string(), json!(65535));
        gen_cfg.insert("thinkingConfig".to_string(), json!({"thinkingLevel":"high","includeThoughts":true}));
    }
    if let Some(v) = body.get("temperature") {
        gen_cfg.insert("temperature".to_string(), v.clone());
    }
    if let Some(v) = body.get("top_p") {
        gen_cfg.insert("topP".to_string(), v.clone());
    }
    if let Some(stop) = body.get("stop") {
        if stop.is_array() {
            gen_cfg.insert("stopSequences".to_string(), stop.clone());
        } else if let Some(s) = stop.as_str() {
            gen_cfg.insert("stopSequences".to_string(), json!([s]));
        }
    }

    // Thinking config
    if let Some(effort) = body.get("reasoning_effort").and_then(|v| v.as_str()) {
        if effort != "none" {
            let is_gemini_3 = model.contains("gemini-3");
            if is_gemini_3 {
                let level = match effort {
                    "minimal" => "minimal",
                    "low" => "low",
                    "medium" => "medium",
                    _ => "high",
                };
                gen_cfg.insert(
                    "thinkingConfig".to_string(),
                    json!({ "thinkingLevel": level, "includeThoughts": true }),
                );
            } else {
                let budget = match effort {
                    "minimal" => 1024,
                    "low" => 2048,
                    "medium" => 8192,
                    "high" => 16384,
                    "xhigh" | "max" => 24576,
                    _ => defaults::DEFAULT_THINKING_BUDGET,
                };
                gen_cfg.insert(
                    "thinkingConfig".to_string(),
                    json!({ "thinkingBudget": budget, "includeThoughts": true }),
                );
            }
        }
    }

    req.insert("generationConfig".to_string(), Value::Object(gen_cfg));
    req.insert("safetySettings".to_string(), default_safety_settings());

    // Tools
    if let Some(tools) = body.get("tools").and_then(|v| v.as_array()) {
        let decls: Vec<Value> = tools
            .iter()
            .filter_map(|t| {
                let f = t.get("function").unwrap_or(t);
                let name = f.get("name").and_then(|v| v.as_str())?;
                let params = f
                    .get("parameters")
                    .map(clean_json_schema_for_gemini)
                    .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
                Some(json!({
                    "name": name,
                    "description": f.get("description").cloned().unwrap_or(json!("")),
                    "parameters": params
                }))
            })
            .collect();
        if !decls.is_empty() {
            req.insert("tools".to_string(), json!([{ "functionDeclarations": decls }]));
            if is_cli_or_ag {
                req.insert(
                    "toolConfig".to_string(),
                    json!({ "functionCallingConfig": { "mode": "VALIDATED" } }),
                );
            }
        }
    }

    Value::Object(req)
}

fn push_or_merge_gemini(contents: &mut Vec<Value>, g_role: &str, parts: Vec<Value>) {
    if parts.is_empty() {
        return;
    }
    if let Some(last) = contents.last_mut() {
        if last.get("role").and_then(|v| v.as_str()) == Some(g_role) {
            if let Some(arr) = last.get_mut("parts").and_then(|v| v.as_array_mut()) {
                arr.extend(parts);
                return;
            }
        }
    }
    contents.push(json!({ "role": g_role, "parts": parts }));
}

pub fn openai_to_gemini_request(
    model: &str,
    body: &Value,
    _stream: bool,
    _credentials: Option<&Value>,
) -> Value {
    build_gemini_core(model, body, false)
}

pub fn openai_to_gemini_cli_request(
    model: &str,
    body: &Value,
    _stream: bool,
    credentials: Option<&Value>,
) -> Value {
    let inner = build_gemini_core(model, body, true);
    let project = credentials
        .and_then(|c| c.get("projectId"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    json!({
        "project": project,
        "model": model,
        "request": inner
    })
}

pub fn openai_to_antigravity_request(
    model: &str,
    body: &Value,
    _stream: bool,
    credentials: Option<&Value>,
) -> Value {
    let mut inner = build_gemini_core(model, body, true);
    if inner.get("systemInstruction").is_none() {
        inner["systemInstruction"] = json!({
            "role": gemini_role::USER,
            "parts": [{ "text": defaults::ANTIGRAVITY_DEFAULT_SYSTEM }]
        });
    }
    let project = credentials
        .and_then(|c| c.get("projectId"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    json!({
        "project": project,
        "model": model,
        "request": inner
    })
}

// ============================================================================
// Response (SSE Chunk): Gemini -> OpenAI
// ============================================================================

pub fn gemini_to_openai_response(chunk: Option<&Value>, state: &mut TranslateState) -> Vec<Value> {
    let Some(chunk) = chunk else {
        return vec![];
    };

    // Unwrap Cloud Code Assist envelope { "response": { ... } } if present
    let payload = chunk.get("response").unwrap_or(chunk);

    if state.message_id.is_none() {
        if let Some(id) = payload.get("responseId").and_then(|v| v.as_str()) {
            state.message_id = Some(id.to_string());
        }
    }
    if state.model.is_none() {
        if let Some(m) = payload.get("modelVersion").and_then(|v| v.as_str()) {
            state.model = Some(m.to_string());
        }
    }
    let created = *state.created.get_or_insert_with(now_unix_secs);
    let id = state
        .message_id
        .clone()
        .unwrap_or_else(|| format!("chatcmpl-{}", created));
    let model = state
        .model
        .clone()
        .unwrap_or_else(|| defaults::MODEL_FALLBACK.to_string());

    if let Some(u) = payload.get("usageMetadata") {
        if let Some(parsed_u) = to_openai_usage(FORMAT_GEMINI, Some(u)) {
            state.usage = Some(parsed_u);
        }
    }

    let mut results: Vec<Value> = Vec::new();
    let candidate = payload
        .get("candidates")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first());

    let Some(cand) = candidate else {
        return results;
    };

    if let Some(parts) = cand
        .get("content")
        .and_then(|c| c.get("parts"))
        .and_then(|v| v.as_array())
    {
        for part in parts {
            let is_thought = part
                .get("thought")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                if !text.is_empty() {
                    if is_thought {
                        let with_role = !state.started;
                        state.started = true;
                        results.push(build_chunk(
                            &id,
                            &model,
                            created,
                            reasoning_delta(text, with_role),
                            None,
                            None,
                        ));
                    } else {
                        let delta = if !state.started {
                            state.started = true;
                            json!({ "role": role::ASSISTANT, "content": text })
                        } else {
                            json!({ "content": text })
                        };
                        results.push(build_chunk(&id, &model, created, delta, None, None));
                    }
                }
            } else if let Some(inline) = part.get("inlineData") {
                let mime = inline.get("mimeType").and_then(|v| v.as_str());
                let data = inline.get("data").and_then(|v| v.as_str()).unwrap_or("");
                let url = encode_data_uri(mime, data);
                let mut delta = json!({
                    "images": [{
                        "type": openai_block::IMAGE_URL,
                        "image_url": { "url": url }
                    }]
                });
                if !state.started {
                    state.started = true;
                    delta["role"] = json!(role::ASSISTANT);
                }
                results.push(build_chunk(&id, &model, created, delta, None, None));
            } else if let Some(fc) = part.get("functionCall") {
                let tc_idx = state.tool_call_index;
                state.tool_call_index += 1;
                state.has_tool_calls = true;
                let name = fc.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let args_val = fc.get("args").cloned().unwrap_or_else(|| json!({}));
                let args_str =
                    serde_json::to_string(&args_val).unwrap_or_else(|_| "{}".to_string());
                let tc_id = fallback_tool_call_id(tc_idx);
                let mut delta = json!({
                    "tool_calls": [{
                        "index": tc_idx,
                        "id": tc_id,
                        "type": openai_block::FUNCTION,
                        "function": {
                            "name": name,
                            "arguments": args_str
                        }
                    }]
                });
                if !state.started {
                    state.started = true;
                    delta["role"] = json!(role::ASSISTANT);
                }
                results.push(build_chunk(&id, &model, created, delta, None, None));
            }
        }
    }

    if let Some(fr) = cand.get("finishReason").and_then(|v| v.as_str()) {
        let finish = to_openai_finish(FORMAT_GEMINI, Some(fr), state.has_tool_calls);
        state.finish_reason = Some(finish.to_string());
        state.finish_reason_sent = true;
        results.push(build_chunk(
            &id,
            &model,
            created,
            json!({}),
            Some(finish),
            state.usage.clone(),
        ));
    }

    results
}

// ============================================================================
// Response (SSE Chunk): OpenAI -> Gemini / Antigravity
// ============================================================================

pub fn openai_to_antigravity_response(
    chunk: Option<&Value>,
    state: &mut TranslateState,
) -> Vec<Value> {
    let Some(chunk) = chunk else {
        return vec![];
    };

    if let Some(u) = chunk.get("usage") {
        if u.is_object() {
            state.usage = Some(u.clone());
        }
    }

    let choice = chunk
        .get("choices")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first());
    let Some(ch) = choice else {
        return vec![];
    };

    let mut parts: Vec<Value> = Vec::new();
    if let Some(delta) = ch.get("delta") {
        let reasoning = extract_reasoning_text(delta);
        if !reasoning.is_empty() {
            parts.push(json!({ "text": reasoning, "thought": true }));
        }
        if let Some(text) = delta.get("content").and_then(|v| v.as_str()) {
            if !text.is_empty() {
                parts.push(json!({ "text": text }));
            }
        }
        if let Some(tcs) = delta.get("tool_calls").and_then(|v| v.as_array()) {
            for tc in tcs {
                let name = tc
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let args = tc
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(|v| v.as_str());
                let parsed_args = parse_json_or_empty(args);
                if !name.is_empty() {
                    parts.push(json!({
                        "functionCall": { "name": name, "args": parsed_args }
                    }));
                }
            }
        }
    }

    let finish_reason = ch
        .get("finish_reason")
        .and_then(|v| v.as_str())
        .map(|fr| from_openai_finish(FORMAT_GEMINI, Some(fr)));

    if parts.is_empty() && finish_reason.is_none() {
        return vec![];
    }

    let mut cand = Map::new();
    if !parts.is_empty() {
        cand.insert(
            "content".to_string(),
            json!({ "role": gemini_role::MODEL, "parts": parts }),
        );
    }
    if let Some(fr) = finish_reason {
        cand.insert("finishReason".to_string(), json!(fr));
    }

    let mut resp = Map::new();
    resp.insert("candidates".to_string(), Value::Array(vec![Value::Object(cand)]));
    if finish_reason.is_some() {
        if let Some(u) = &state.usage {
            let prompt = u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            let completion = u
                .get("completion_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            resp.insert(
                "usageMetadata".to_string(),
                json!({
                    "promptTokenCount": prompt,
                    "candidatesTokenCount": completion,
                    "totalTokenCount": prompt + completion
                }),
            );
        }
    }

    if state.source_format == FORMAT_ANTIGRAVITY
        || state.source_format == FORMAT_GEMINI_CLI
        || state.source_format == FORMAT_VERTEX
    {
        vec![json!({ "response": Value::Object(resp) })]
    } else {
        vec![Value::Object(resp)]
    }
}
