use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::translator::formats::{
    FORMAT_ANTIGRAVITY, FORMAT_CLAUDE, FORMAT_COMMANDCODE, FORMAT_GEMINI, FORMAT_GEMINI_CLI,
    FORMAT_KIRO, FORMAT_OLLAMA, FORMAT_OPENAI_RESPONSES, FORMAT_VERTEX,
};
use crate::translator::schema::{
    claude_stop, defaults, gemini_finish, openai_block, openai_finish, responses_incomplete_reason,
    responses_status, role,
};

static ID_COUNTER: AtomicU64 = AtomicU64::new(1);

pub fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn now_unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

pub fn next_seq() -> u64 {
    ID_COUNTER.fetch_add(1, Ordering::Relaxed)
}

pub fn generate_tool_call_id() -> String {
    format!("call_{}_{}", now_unix_millis(), next_seq())
}

pub fn fallback_tool_call_id(index: usize) -> String {
    format!("call_{}_{}", index, now_unix_millis())
}

// ---------- finishReason ----------

pub fn to_openai_finish(source_format: &str, reason: Option<&str>, has_tool_calls: bool) -> &'static str {
    let Some(r) = reason else {
        return if has_tool_calls {
            openai_finish::TOOL_CALLS
        } else {
            openai_finish::STOP
        };
    };

    match source_format {
        FORMAT_CLAUDE => match r {
            claude_stop::END_TURN | claude_stop::STOP_SEQUENCE => openai_finish::STOP,
            claude_stop::MAX_TOKENS => openai_finish::LENGTH,
            claude_stop::TOOL_USE => openai_finish::TOOL_CALLS,
            _ => openai_finish::STOP,
        },
        FORMAT_GEMINI | FORMAT_GEMINI_CLI | FORMAT_VERTEX | FORMAT_ANTIGRAVITY => {
            if r == gemini_finish::STOP && has_tool_calls {
                return openai_finish::TOOL_CALLS;
            }
            match r {
                gemini_finish::STOP => openai_finish::STOP,
                gemini_finish::MAX_TOKENS => openai_finish::LENGTH,
                gemini_finish::SAFETY | gemini_finish::RECITATION => openai_finish::CONTENT_FILTER,
                _ => openai_finish::STOP,
            }
        }
        FORMAT_OPENAI_RESPONSES => match r {
            responses_status::COMPLETED => {
                if has_tool_calls {
                    openai_finish::TOOL_CALLS
                } else {
                    openai_finish::STOP
                }
            }
            responses_status::INCOMPLETE | responses_incomplete_reason::MAX_OUTPUT_TOKENS => {
                openai_finish::LENGTH
            }
            responses_status::FAILED | responses_incomplete_reason::CONTENT_FILTER => {
                openai_finish::CONTENT_FILTER
            }
            _ => openai_finish::STOP,
        },
        FORMAT_COMMANDCODE => match r {
            "stop" => openai_finish::STOP,
            "length" => openai_finish::LENGTH,
            "tool-calls" | "tool_use" => openai_finish::TOOL_CALLS,
            "content-filter" => openai_finish::CONTENT_FILTER,
            "error" => openai_finish::STOP,
            _ => openai_finish::STOP,
        },
        FORMAT_KIRO | FORMAT_OLLAMA => match r {
            "tool_calls" | "tool_use" => openai_finish::TOOL_CALLS,
            "length" | "max_tokens" => openai_finish::LENGTH,
            _ => openai_finish::STOP,
        },
        _ => openai_finish::STOP,
    }
}

pub fn from_openai_finish(target_format: &str, finish_reason: Option<&str>) -> &'static str {
    let r = finish_reason.unwrap_or(openai_finish::STOP);
    match target_format {
        FORMAT_CLAUDE => match r {
            openai_finish::STOP => claude_stop::END_TURN,
            openai_finish::LENGTH => claude_stop::MAX_TOKENS,
            openai_finish::TOOL_CALLS => claude_stop::TOOL_USE,
            openai_finish::CONTENT_FILTER => claude_stop::END_TURN,
            _ => claude_stop::END_TURN,
        },
        FORMAT_GEMINI | FORMAT_GEMINI_CLI | FORMAT_VERTEX | FORMAT_ANTIGRAVITY => match r {
            openai_finish::STOP | openai_finish::TOOL_CALLS => gemini_finish::STOP,
            openai_finish::LENGTH => gemini_finish::MAX_TOKENS,
            openai_finish::CONTENT_FILTER => gemini_finish::SAFETY,
            _ => gemini_finish::STOP,
        },
        FORMAT_OPENAI_RESPONSES => match r {
            openai_finish::STOP | openai_finish::TOOL_CALLS => responses_status::COMPLETED,
            openai_finish::LENGTH => responses_status::INCOMPLETE,
            openai_finish::CONTENT_FILTER => responses_status::FAILED,
            _ => responses_status::COMPLETED,
        },
        _ => openai_finish::STOP,
    }
}

// ---------- chunk ----------

pub fn build_chunk(
    id: &str,
    model: &str,
    created: u64,
    delta: Value,
    finish_reason: Option<&str>,
    usage: Option<Value>,
) -> Value {
    let mut obj = Map::new();
    obj.insert("id".to_string(), json!(id));
    obj.insert("object".to_string(), json!("chat.completion.chunk"));
    obj.insert("created".to_string(), json!(created));
    obj.insert("model".to_string(), json!(model));
    obj.insert(
        "choices".to_string(),
        json!([{
            "index": 0,
            "delta": delta,
            "finish_reason": finish_reason,
        }]),
    );
    if let Some(u) = usage {
        obj.insert("usage".to_string(), u);
    }
    Value::Object(obj)
}

// ---------- usage ----------

pub fn build_usage(prompt: u64, completion: u64, cached: u64, reasoning: u64) -> Value {
    let mut usage = json!({
        "prompt_tokens": prompt,
        "completion_tokens": completion,
        "total_tokens": prompt + completion,
    });
    if cached > 0 {
        usage["prompt_tokens_details"] = json!({ "cached_tokens": cached });
    }
    if reasoning > 0 {
        usage["completion_tokens_details"] = json!({ "reasoning_tokens": reasoning });
    }
    usage
}

pub fn to_openai_usage(source_format: &str, raw: Option<&Value>) -> Option<Value> {
    let raw = raw?;
    if !raw.is_object() {
        return None;
    }
    match source_format {
        FORMAT_CLAUDE => {
            let input = raw.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            let cache_read = raw
                .get("cache_read_input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let cache_create = raw
                .get("cache_creation_input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let output = raw.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            Some(build_usage(
                input + cache_read + cache_create,
                output,
                cache_read,
                0,
            ))
        }
        FORMAT_GEMINI | FORMAT_GEMINI_CLI | FORMAT_VERTEX | FORMAT_ANTIGRAVITY => {
            let cached = raw
                .get("cachedContentTokenCount")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let prompt_raw = raw
                .get("promptTokenCount")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let prompt = prompt_raw.saturating_sub(cached);
            let candidates = raw
                .get("candidatesTokenCount")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let thoughts = raw
                .get("thoughtsTokenCount")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let completion = candidates + thoughts;
            Some(build_usage(prompt + cached, completion, cached, thoughts))
        }
        FORMAT_OPENAI_RESPONSES => {
            let prompt = raw.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            let completion = raw
                .get("output_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let cached = raw
                .get("input_tokens_details")
                .and_then(|d| d.get("cached_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let reasoning = raw
                .get("output_tokens_details")
                .and_then(|d| d.get("reasoning_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            Some(build_usage(prompt, completion, cached, reasoning))
        }
        FORMAT_KIRO => {
            let input = raw.get("inputTokens").and_then(|v| v.as_u64()).unwrap_or(0);
            let output = raw.get("outputTokens").and_then(|v| v.as_u64()).unwrap_or(0);
            let cached = raw
                .get("cache_read_input_tokens")
                .or_else(|| raw.get("cachedTokens"))
                .or_else(|| raw.get("cached_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let cache_creation = raw
                .get("cache_creation_input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let mut u = build_usage(input, output, cached, 0);
            if cache_creation > 0 {
                if let Some(obj) = u.as_object_mut() {
                    let details = obj
                        .entry("prompt_tokens_details")
                        .or_insert_with(|| json!({}));
                    if let Some(dobj) = details.as_object_mut() {
                        dobj.insert("cache_creation_tokens".to_string(), json!(cache_creation));
                    }
                }
            }
            Some(u)
        }
        FORMAT_OLLAMA => {
            let input = raw.get("prompt_eval_count").and_then(|v| v.as_u64()).unwrap_or(0);
            let output = raw.get("eval_count").and_then(|v| v.as_u64()).unwrap_or(0);
            let cached = raw
                .get("prompt_eval_cached_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            Some(build_usage(input, output, cached, 0))
        }
        FORMAT_COMMANDCODE => {
            let input = raw.get("inputTokens").and_then(|v| v.as_u64()).unwrap_or(0);
            let output = raw.get("outputTokens").and_then(|v| v.as_u64()).unwrap_or(0);
            let total = raw
                .get("totalTokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(input + output);
            let mut u = build_usage(input, output, 0, 0);
            if let Some(obj) = u.as_object_mut() {
                obj.insert("total_tokens".to_string(), json!(total));
            }
            Some(u)
        }
        _ => None,
    }
}

// ---------- json ----------

pub fn parse_json_or_empty(s: Option<&str>) -> Value {
    let Some(raw) = s else {
        return json!({});
    };
    if raw.trim().is_empty() {
        return json!({});
    }
    serde_json::from_str(raw).unwrap_or_else(|_| json!({}))
}

// ---------- message / text ----------

pub fn collapse_text_parts(content: &Value) -> Value {
    if let Some(arr) = content.as_array() {
        let texts: Vec<&str> = arr
            .iter()
            .filter_map(|p| {
                if p.get("type").and_then(|t| t.as_str()) == Some(openai_block::TEXT) {
                    p.get("text").and_then(|t| t.as_str())
                } else {
                    None
                }
            })
            .collect();
        if texts.len() == arr.len() {
            return Value::String(texts.join("\n"));
        }
    }
    content.clone()
}

// ---------- image ----------

pub fn encode_data_uri(mime_type: Option<&str>, data: &str) -> String {
    let mime = mime_type.unwrap_or(defaults::DEFAULT_IMAGE_MIME);
    format!("data:{};base64,{}", mime, data)
}

pub fn parse_data_uri(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(";base64,")?;
    Some((meta.to_string(), data.to_string()))
}

// ---------- reasoning ----------

pub fn reasoning_delta(text: &str, with_role: bool) -> Value {
    if with_role {
        json!({ "role": role::ASSISTANT, "reasoning_content": text })
    } else {
        json!({ "reasoning_content": text })
    }
}

pub fn extract_reasoning_text(delta: &Value) -> String {
    if let Some(s) = delta.get("reasoning_content").and_then(|v| v.as_str()) {
        if !s.is_empty() {
            return s.to_string();
        }
    }
    if let Some(s) = delta.get("reasoning").and_then(|v| v.as_str()) {
        if !s.is_empty() {
            return s.to_string();
        }
    }
    if let Some(details) = delta.get("reasoning_details").and_then(|v| v.as_array()) {
        let mut out = String::new();
        for d in details {
            if let Some(s) = d.as_str() {
                out.push_str(s);
            } else if let Some(s) = d
                .get("text")
                .and_then(|v| v.as_str())
                .or_else(|| d.get("content").and_then(|v| v.as_str()))
            {
                out.push_str(s);
            }
        }
        return out;
    }
    String::new()
}

pub fn lift_reasoning_aliases(delta: &mut Value) {
    let has_rc = delta
        .get("reasoning_content")
        .and_then(|v| v.as_str())
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    if has_rc {
        return;
    }
    let text = extract_reasoning_text(delta);
    if !text.is_empty() {
        if let Some(obj) = delta.as_object_mut() {
            obj.insert("reasoning_content".to_string(), Value::String(text));
        }
    }
}

// ---------- toolCall ----------

pub fn sanitize_tool_id(id: &str) -> String {
    id.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect()
}

#[cfg(test)]
#[test]
fn tool_ids_are_sanitized_consistently() {
    let mut messages = vec![json!({"role":"assistant","tool_calls":[{"id":"call.a","function":{"name":"x"}}]}), json!({"role":"tool","tool_call_id":"call.a"})];
    ensure_tool_call_ids(&mut messages);
    assert_eq!(messages[0]["tool_calls"][0]["id"], "calla");
    assert_eq!(messages[1]["tool_call_id"], "calla");
}

pub fn ensure_tool_call_ids(messages: &mut Vec<Value>) {
    let mut name_to_ids: HashMap<String, Vec<String>> = HashMap::new();
    let mut counter: u64 = 0;

    for msg in messages.iter_mut() {
        if let Some(id) = msg.get_mut("tool_call_id") && let Some(raw) = id.as_str() {
            let clean = sanitize_tool_id(raw);
            if !clean.is_empty() { *id = Value::String(clean); }
        }
        if let Some(blocks) = msg.get_mut("content").and_then(Value::as_array_mut) {
            for block in blocks {
                let field = match block["type"].as_str() { Some("tool_use") => "id", Some("tool_result") => "tool_use_id", _ => continue };
                if let Some(id) = block.get_mut(field) && let Some(raw) = id.as_str() {
                    let clean = sanitize_tool_id(raw);
                    if !clean.is_empty() { *id = Value::String(clean); }
                }
            }
        }
        let is_assistant = msg.get("role").and_then(|r| r.as_str()) == Some(role::ASSISTANT);
        if is_assistant {
            if let Some(tcs) = msg.get_mut("tool_calls").and_then(|v| v.as_array_mut()) {
                for tc in tcs.iter_mut() {
                    if let Some(id) = tc.get_mut("id") && let Some(raw) = id.as_str() {
                        *id = Value::String(sanitize_tool_id(raw));
                    }
                    let has_id = tc
                        .get("id")
                        .and_then(|v| v.as_str())
                        .map(|s| !s.is_empty())
                        .unwrap_or(false);
                    if !has_id {
                        counter += 1;
                        let new_id = format!("call_{}_{}", counter, now_unix_millis());
                        if let Some(obj) = tc.as_object_mut() {
                            obj.insert("id".to_string(), Value::String(new_id.clone()));
                        }
                        if let Some(fname) = tc
                            .get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|n| n.as_str())
                        {
                            name_to_ids
                                .entry(fname.to_string())
                                .or_default()
                                .push(new_id);
                        }
                    }
                }
            }
        }
        let is_tool = msg.get("role").and_then(|r| r.as_str()) == Some(role::TOOL);
        if is_tool {
            let has_id = msg
                .get("tool_call_id")
                .and_then(|v| v.as_str())
                .map(|s| !s.is_empty())
                .unwrap_or(false);
            if !has_id {
                let name = msg
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("");
                let matched = name_to_ids
                    .get_mut(name)
                    .and_then(|q| if q.is_empty() { None } else { Some(q.remove(0)) })
                    .unwrap_or_else(|| {
                        counter += 1;
                        format!("call_{}_{}", counter, now_unix_millis())
                    });
                if let Some(obj) = msg.as_object_mut() {
                    obj.insert("tool_call_id".to_string(), Value::String(matched));
                }
            }
        }
    }
}

pub fn fix_missing_tool_responses(messages: &[Value]) -> Vec<Value> {
    let mut result = Vec::with_capacity(messages.len());
    let mut pending: Vec<String> = Vec::new();
    let mut responded: HashSet<String> = HashSet::new();

    for msg in messages {
        let r = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
        if r == role::TOOL {
            if let Some(id) = msg.get("tool_call_id").and_then(|v| v.as_str()) {
                responded.insert(id.to_string());
            }
            result.push(msg.clone());
            continue;
        }

        if !pending.is_empty() {
            for id in pending.drain(..) {
                if !responded.contains(&id) {
                    result.push(json!({
                        "role": role::TOOL,
                        "tool_call_id": id,
                        "content": "[No tool response]",
                    }));
                }
            }
            responded.clear();
        }

        result.push(msg.clone());

        if r == role::ASSISTANT {
            if let Some(tcs) = msg.get("tool_calls").and_then(|v| v.as_array()) {
                for tc in tcs {
                    if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                        pending.push(id.to_string());
                    }
                }
            }
        }
    }

    if !pending.is_empty() {
        for id in pending {
            if !responded.contains(&id) {
                result.push(json!({
                    "role": role::TOOL,
                    "tool_call_id": id,
                    "content": "[No tool response]",
                }));
            }
        }
    }

    result
}

// ---------- maxTokens ----------

pub fn adjust_max_tokens(body: &Value) -> u64 {
    const DEFAULT_MAX_TOKENS: u64 = crate::config::DEFAULT_MAX_TOKENS as u64;
    const MIN_MAX_TOKENS: u64 = crate::config::DEFAULT_MIN_TOKENS as u64;
    const GEMINI_3_HIGH_THINKING_MIN: u64 = 65535;
    const THINKING_BUFFER: u64 = 8192;

    let mut max_tokens = body
        .get("max_tokens")
        .and_then(|v| v.as_u64())
        .or_else(|| body.get("max_completion_tokens").and_then(|v| v.as_u64()))
        .unwrap_or(DEFAULT_MAX_TOKENS);

    if max_tokens == 0 {
        max_tokens = DEFAULT_MAX_TOKENS;
    }

    let thinking_budget = body
        .get("thinking")
        .and_then(|t| t.get("budget_tokens"))
        .and_then(|v| v.as_u64())
        .or_else(|| {
            body.get("generationConfig")
                .and_then(|g| g.get("thinkingConfig"))
                .and_then(|tc| tc.get("thinkingBudget"))
                .and_then(|v| v.as_u64())
        });

    if let Some(budget) = thinking_budget {
        let min_for_thinking = budget + THINKING_BUFFER;
        if max_tokens < min_for_thinking {
            max_tokens = min_for_thinking;
        }
    } else if body
        .get("reasoning_effort")
        .and_then(|v| v.as_str())
        .map(|s| s == "high" || s == "xhigh")
        .unwrap_or(false)
        && max_tokens < GEMINI_3_HIGH_THINKING_MIN
    {
        max_tokens = GEMINI_3_HIGH_THINKING_MIN;
    }

    if body.get("tools").and_then(|t| t.as_array()).map(|a| !a.is_empty()).unwrap_or(false)
        && max_tokens < MIN_MAX_TOKENS
    {
        max_tokens = MIN_MAX_TOKENS;
    }

    max_tokens
}

// ---------- openai format filter ----------

const VALID_OPENAI_CONTENT_TYPES: &[&str] = &[
    openai_block::TEXT,
    openai_block::IMAGE_URL,
    openai_block::INPUT_AUDIO,
];

pub fn filter_to_openai_format(body: &Value) -> Value {
    let mut result = Map::new();

    if let Some(v) = body.get("model") {
        result.insert("model".to_string(), v.clone());
    }

    if let Some(msgs) = body.get("messages").and_then(|v| v.as_array()) {
        let mut out_msgs = Vec::with_capacity(msgs.len());
        for msg in msgs {
            let mut filtered_msg = Map::new();
            let role_val = msg.get("role").cloned().unwrap_or(Value::Null);
            filtered_msg.insert("role".to_string(), role_val.clone());
            let role_str = role_val.as_str().unwrap_or("");

            if let Some(content) = msg.get("content") {
                if content.is_string() {
                    filtered_msg.insert("content".to_string(), content.clone());
                } else if let Some(arr) = content.as_array() {
                    let valid_parts: Vec<Value> = arr
                        .iter()
                        .filter(|p| {
                            p.get("type")
                                .and_then(|t| t.as_str())
                                .map(|t| VALID_OPENAI_CONTENT_TYPES.contains(&t))
                                .unwrap_or(false)
                        })
                        .map(|p| {
                            if p.get("type").and_then(|t| t.as_str()) == Some(openai_block::TEXT) {
                                json!({ "type": openai_block::TEXT, "text": p.get("text").and_then(|t| t.as_str()).unwrap_or("") })
                            } else if p.get("type").and_then(|t| t.as_str())
                                == Some(openai_block::IMAGE_URL)
                            {
                                json!({ "type": openai_block::IMAGE_URL, "image_url": p.get("image_url").cloned().unwrap_or(Value::Null) })
                            } else {
                                p.clone()
                            }
                        })
                        .collect();

                    if valid_parts.len() == 1
                        && valid_parts[0].get("type").and_then(|t| t.as_str())
                            == Some(openai_block::TEXT)
                    {
                        filtered_msg.insert(
                            "content".to_string(),
                            valid_parts[0].get("text").cloned().unwrap_or(json!("")),
                        );
                    } else if !valid_parts.is_empty() {
                        filtered_msg.insert("content".to_string(), Value::Array(valid_parts));
                    } else {
                        filtered_msg.insert("content".to_string(), json!(""));
                    }
                } else if content.is_null() && role_str == role::ASSISTANT {
                    filtered_msg.insert("content".to_string(), Value::Null);
                }
            } else if role_str == role::ASSISTANT {
                filtered_msg.insert("content".to_string(), Value::Null);
            }

            if let Some(name) = msg.get("name") {
                filtered_msg.insert("name".to_string(), name.clone());
            }
            if role_str == role::ASSISTANT {
                if let Some(tcs) = msg.get("tool_calls").and_then(|v| v.as_array()) {
                    let valid_tcs: Vec<Value> = tcs
                        .iter()
                        .map(|tc| {
                            let args = match tc.get("function").and_then(|f| f.get("arguments")) {
                                Some(Value::String(s)) => s.clone(),
                                Some(v) => serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string()),
                                None => "{}".to_string(),
                            };
                            json!({
                                "id": tc.get("id").cloned().unwrap_or(Value::Null),
                                "type": openai_block::FUNCTION,
                                "function": {
                                    "name": tc.get("function").and_then(|f| f.get("name")).cloned().unwrap_or(Value::Null),
                                    "arguments": args
                                }
                            })
                        })
                        .collect();
                    filtered_msg.insert("tool_calls".to_string(), Value::Array(valid_tcs));
                }
            }
            if role_str == role::TOOL {
                if let Some(id) = msg.get("tool_call_id") {
                    filtered_msg.insert("tool_call_id".to_string(), id.clone());
                }
                if !filtered_msg.contains_key("content") {
                    filtered_msg.insert("content".to_string(), json!(""));
                } else if let Some(c) = filtered_msg.get("content") {
                    if !c.is_string() {
                        let s = serde_json::to_string(c).unwrap_or_default();
                        filtered_msg.insert("content".to_string(), Value::String(s));
                    }
                }
            }

            out_msgs.push(Value::Object(filtered_msg));
        }
        result.insert("messages".to_string(), Value::Array(out_msgs));
    }

    for key in &[
        "temperature",
        "top_p",
        "max_tokens",
        "max_completion_tokens",
        "stream",
        "stop",
        "presence_penalty",
        "frequency_penalty",
        "logit_bias",
        "user",
        "seed",
        "n",
        "response_format",
        "parallel_tool_calls",
        "reasoning_effort",
        "stream_options",
    ] {
        if let Some(v) = body.get(*key) {
            result.insert((*key).to_string(), v.clone());
        }
    }

    if let Some(tools) = body.get("tools").and_then(|v| v.as_array()) {
        let valid_tools: Vec<Value> = tools
            .iter()
            .filter(|t| t.get("type").and_then(|v| v.as_str()) == Some(openai_block::FUNCTION) || t.get("function").is_some())
            .map(|t| {
                let f = t.get("function").unwrap_or(t);
                json!({
                    "type": openai_block::FUNCTION,
                    "function": {
                        "name": f.get("name").cloned().unwrap_or(Value::Null),
                        "description": f.get("description").cloned().unwrap_or(json!("")),
                        "parameters": f.get("parameters").cloned().unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
                    }
                })
            })
            .collect();
        if !valid_tools.is_empty() {
            result.insert("tools".to_string(), Value::Array(valid_tools));
        }
    }

    if let Some(tc) = body.get("tool_choice") {
        result.insert("tool_choice".to_string(), tc.clone());
    }

    Value::Object(result)
}
