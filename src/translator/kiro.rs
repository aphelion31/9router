#![allow(dead_code)]

use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};

use crate::translator::concerns::{
    build_chunk, fallback_tool_call_id, now_unix_millis, now_unix_secs, parse_data_uri,
    parse_json_or_empty, to_openai_finish, to_openai_usage,
};
use crate::translator::formats::FORMAT_KIRO;
use crate::translator::schema::{claude_block, openai_block, openai_finish, role};
use crate::translator::TranslateState;

pub const KIRO_TOOL_NAME_MAX_LENGTH: usize = 64;
pub const KIRO_TOOL_DESCRIPTION_MAX_LENGTH: usize = 10240;
pub const KIRO_TOOL_ID_MAX_LENGTH: usize = 64;
pub const KIRO_TOOL_RESULTS_PLACEHOLDER: &str = "Tool results provided.";
pub const KIRO_EMPTY_USER_PLACEHOLDER: &str = "continue";
pub const DEFAULT_KIRO_ORIGIN: &str = "AI_EDITOR";
pub const KIRO_MAX_REASONING_EFFORT: &str = "high";

pub fn get_kiro_origin(credentials: Option<&Value>) -> &'static str {
    let method = credentials
        .and_then(|c| c.get("authMethod").or_else(|| c.get("providerSpecificData").and_then(|p| p.get("authMethod"))))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if method == "idc" || method == "builder-id" {
        "AI_EDITOR"
    } else {
        DEFAULT_KIRO_ORIGIN
    }
}

pub fn build_kiro_thinking_prompt(max_tokens: u64, effort: &str) -> String {
    format!(
        "<thinking_mode>enabled</thinking_mode>\n<max_thinking_length>{}</max_thinking_length>\n<reasoning_effort>{}</reasoning_effort>",
        max_tokens, effort
    )
}

fn trim_chars(s: &str, limit: usize) -> String {
    s.chars().take(limit).collect()
}

fn value_as_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn append_text(target: &mut Map<String, Value>, extra: &str) {
    if extra.is_empty() {
        return;
    }
    let existing = target
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let merged = if existing.is_empty() {
        extra.to_string()
    } else {
        format!("{}\n\n{}", existing, extra)
    };
    target.insert("content".to_string(), json!(merged));
}

fn unique_tool_name(raw_name: &str, index: usize, used: &mut HashSet<String>) -> String {
    let cleaned: String = raw_name
        .trim()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect();
    let cleaned = cleaned.trim_matches('_').to_string();
    let base_raw = if cleaned.is_empty() {
        format!("tool_{}", index + 1)
    } else {
        cleaned
    };
    let base = trim_chars(&base_raw, KIRO_TOOL_NAME_MAX_LENGTH);
    let mut candidate = base.clone();
    let mut suffix = 2usize;
    while used.contains(&candidate) {
        let tail = format!("_{}", suffix);
        suffix += 1;
        let prefix = trim_chars(&base, KIRO_TOOL_NAME_MAX_LENGTH.saturating_sub(tail.len()));
        candidate = format!("{}{}", prefix, tail);
    }
    used.insert(candidate.clone());
    candidate
}

fn clean_schema_value(v: &Value) -> Value {
    match v {
        Value::Array(arr) => Value::Array(arr.iter().map(clean_schema_value).collect()),
        Value::Object(obj) => {
            let mut out = Map::new();
            for (k, child) in obj {
                if k == "additionalProperties" {
                    continue;
                }
                if k == "required" {
                    if let Some(arr) = child.as_array() {
                        if arr.is_empty() {
                            continue;
                        }
                    }
                }
                out.insert(k.clone(), clean_schema_value(child));
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

fn normalize_root_schema(schema: &Value) -> Value {
    let mut cleaned = match clean_schema_value(schema) {
        Value::Object(o) => o,
        _ => Map::new(),
    };
    cleaned.insert("type".to_string(), json!("object"));
    if !cleaned
        .get("properties")
        .map(|v| v.is_object())
        .unwrap_or(false)
    {
        cleaned.insert("properties".to_string(), json!({}));
    }
    let prop_keys: HashSet<String> = cleaned
        .get("properties")
        .and_then(|v| v.as_object())
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    if let Some(req) = cleaned.get("required").and_then(|v| v.as_array()) {
        let mut seen = HashSet::new();
        let filtered: Vec<Value> = req
            .iter()
            .filter_map(|v| v.as_str())
            .filter(|name| prop_keys.contains(*name) && seen.insert((*name).to_string()))
            .map(|name| json!(name))
            .collect();
        if filtered.is_empty() {
            cleaned.remove("required");
        } else {
            cleaned.insert("required".to_string(), Value::Array(filtered));
        }
    }
    Value::Object(cleaned)
}

pub fn normalize_kiro_tool_specs(tools: Option<&Vec<Value>>) -> (Vec<Value>, HashMap<String, String>) {
    let mut specs = Vec::new();
    let mut name_map = HashMap::new();
    let mut used_names = HashSet::new();
    let Some(tools) = tools else {
        return (specs, name_map);
    };

    for (index, tool) in tools.iter().enumerate() {
        if !tool.is_object() {
            continue;
        }
        let raw_name = tool
            .get("function")
            .and_then(|f| f.get("name"))
            .or_else(|| tool.get("name"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if raw_name.trim().is_empty() || name_map.contains_key(raw_name) {
            continue;
        }

        let name = unique_tool_name(raw_name, index, &mut used_names);
        name_map.insert(raw_name.to_string(), name.clone());

        let raw_desc = tool
            .get("function")
            .and_then(|f| f.get("description"))
            .or_else(|| tool.get("description"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("Tool: {}", raw_name));
        let description = trim_chars(&raw_desc, KIRO_TOOL_DESCRIPTION_MAX_LENGTH);

        let schema = tool
            .get("function")
            .and_then(|f| f.get("parameters"))
            .or_else(|| tool.get("parameters"))
            .or_else(|| tool.get("input_schema"))
            .cloned()
            .unwrap_or_else(|| json!({}));

        specs.push(json!({
            "toolSpecification": {
                "name": name,
                "description": description,
                "inputSchema": { "json": normalize_root_schema(&schema) },
            }
        }));
    }

    (specs, name_map)
}

fn reserve_tool_id(
    raw_id: &str,
    turn_idx: usize,
    call_idx: usize,
    name: &str,
    used_ids: &mut HashSet<String>,
) -> String {
    let sanitized: String = raw_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    let generated = format!(
        "call_msg{}_tc{}_{}",
        turn_idx,
        call_idx,
        if name.is_empty() { "tool" } else { name }
    );
    let base = trim_chars(
        if sanitized.is_empty() {
            &generated
        } else {
            &sanitized
        },
        KIRO_TOOL_ID_MAX_LENGTH,
    );
    let mut candidate = base.clone();
    let mut suffix = 2usize;
    while used_ids.contains(&candidate) {
        let tail = format!("_{}", suffix);
        suffix += 1;
        let prefix = trim_chars(&base, KIRO_TOOL_ID_MAX_LENGTH.saturating_sub(tail.len()));
        candidate = format!("{}{}", prefix, tail);
    }
    used_ids.insert(candidate.clone());
    candidate
}

fn clean_user_context(user: &mut Map<String, Value>) {
    if let Some(ctx) = user
        .get_mut("userInputMessageContext")
        .and_then(|v| v.as_object_mut())
    {
        if ctx
            .get("toolResults")
            .and_then(|v| v.as_array())
            .map(|a| a.is_empty())
            .unwrap_or(true)
        {
            ctx.remove("toolResults");
        }
        if ctx
            .get("tools")
            .and_then(|v| v.as_array())
            .map(|a| a.is_empty())
            .unwrap_or(true)
        {
            ctx.remove("tools");
        }
        if ctx.is_empty() {
            user.remove("userInputMessageContext");
        }
    }
}

fn merge_user(target: &mut Map<String, Value>, source: &Map<String, Value>) {
    let src_content = source.get("content").and_then(|v| v.as_str()).unwrap_or("");
    append_text(target, src_content);
    if let Some(imgs) = source.get("images").and_then(|v| v.as_array()) {
        if !imgs.is_empty() {
            let mut merged = target
                .get("images")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            merged.extend(imgs.iter().cloned());
            target.insert("images".to_string(), Value::Array(merged));
        }
    }
    if let Some(results) = source
        .get("userInputMessageContext")
        .and_then(|c| c.get("toolResults"))
        .and_then(|v| v.as_array())
    {
        if !results.is_empty() {
            let ctx = target
                .entry("userInputMessageContext".to_string())
                .or_insert_with(|| json!({}));
            if let Some(ctx_obj) = ctx.as_object_mut() {
                let mut merged = ctx_obj
                    .get("toolResults")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                merged.extend(results.iter().cloned());
                ctx_obj.insert("toolResults".to_string(), Value::Array(merged));
            }
        }
    }
}

fn merge_assistant(target: &mut Map<String, Value>, source: &Map<String, Value>) {
    let src_content = source.get("content").and_then(|v| v.as_str()).unwrap_or("");
    append_text(target, src_content);
    if let Some(uses) = source.get("toolUses").and_then(|v| v.as_array()) {
        if !uses.is_empty() {
            let mut merged = target
                .get("toolUses")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            merged.extend(uses.iter().cloned());
            target.insert("toolUses".to_string(), Value::Array(merged));
        }
    }
}

fn normalize_tool_result(result: &Value) -> Value {
    let content = if let Some(arr) = result.get("content").and_then(|v| v.as_array()) {
        let items: Vec<Value> = arr
            .iter()
            .map(|part| {
                let t = part
                    .get("text")
                    .map(value_as_text)
                    .unwrap_or_else(|| value_as_text(part));
                json!({ "text": t })
            })
            .collect();
        if items.is_empty() {
            vec![json!({ "text": "" })]
        } else {
            items
        }
    } else {
        vec![json!({ "text": value_as_text(result.get("content").unwrap_or(&Value::Null)) })]
    };
    json!({
        "toolUseId": result.get("toolUseId").and_then(|v| v.as_str()).unwrap_or(""),
        "status": if result.get("status").and_then(|v| v.as_str()) == Some("error") { "error" } else { "success" },
        "content": content,
    })
}

fn tool_result_text(result: &Value) -> String {
    let content = if let Some(arr) = result.get("content").and_then(|v| v.as_array()) {
        arr.iter()
            .map(|p| {
                p.get("text")
                    .map(value_as_text)
                    .unwrap_or_else(|| value_as_text(p))
            })
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        value_as_text(result.get("content").unwrap_or(&Value::Null))
    };
    let err_tag = if result.get("status").and_then(|v| v.as_str()) == Some("error") {
        " (error)"
    } else {
        ""
    };
    format!("[Tool result{}: {}]", err_tag, content)
}

fn tool_call_text(name: &str, input: Option<&Value>) -> String {
    let inp_str = input.map(value_as_text).unwrap_or_else(|| "{}".to_string());
    format!(
        "[Tool call: {}({})]",
        if name.is_empty() { "unknown" } else { name },
        inp_str
    )
}

pub fn canonicalize_kiro_conversation(
    history: Vec<Value>,
    current_message: Option<Value>,
    model_id: &str,
    tool_specs: &[Value],
    name_map: &HashMap<String, String>,
) -> (Vec<Value>, Value) {
    let mut raw_turns = history;
    if let Some(cm) = current_message {
        raw_turns.push(cm);
    }

    let mut turns: Vec<Value> = Vec::new();
    for raw in raw_turns {
        let has_user = raw.get("userInputMessage").map(|v| v.is_object()).unwrap_or(false);
        let has_asst = raw
            .get("assistantResponseMessage")
            .map(|v| v.is_object())
            .unwrap_or(false);
        if has_user == has_asst {
            continue;
        }
        if has_user {
            let src = raw.get("userInputMessage").and_then(|v| v.as_object()).unwrap();
            if let Some(prev_user) = turns
                .last_mut()
                .and_then(|t| t.get_mut("userInputMessage"))
                .and_then(|v| v.as_object_mut())
            {
                merge_user(prev_user, src);
            } else {
                turns.push(json!({ "userInputMessage": Value::Object(src.clone()) }));
            }
        } else {
            let src = raw
                .get("assistantResponseMessage")
                .and_then(|v| v.as_object())
                .unwrap();
            if let Some(prev_asst) = turns
                .last_mut()
                .and_then(|t| t.get_mut("assistantResponseMessage"))
                .and_then(|v| v.as_object_mut())
            {
                merge_assistant(prev_asst, src);
            } else {
                turns.push(json!({ "assistantResponseMessage": Value::Object(src.clone()) }));
            }
        }
    }

    if turns
        .first()
        .and_then(|t| t.get("assistantResponseMessage"))
        .is_some()
    {
        turns.insert(
            0,
            json!({ "userInputMessage": { "content": KIRO_EMPTY_USER_PLACEHOLDER, "modelId": model_id } }),
        );
    }
    if turns.is_empty()
        || turns
            .last()
            .and_then(|t| t.get("assistantResponseMessage"))
            .is_some()
    {
        turns.push(json!({ "userInputMessage": { "content": KIRO_EMPTY_USER_PLACEHOLDER, "modelId": model_id } }));
    }

    for turn in &mut turns {
        if let Some(u) = turn.get_mut("userInputMessage").and_then(|v| v.as_object_mut()) {
            let trimmed = u
                .get("content")
                .map(value_as_text)
                .unwrap_or_default()
                .trim()
                .to_string();
            let has_tr = u
                .get("userInputMessageContext")
                .and_then(|c| c.get("toolResults"))
                .and_then(|r| r.as_array())
                .map(|a| !a.is_empty())
                .unwrap_or(false);
            let content = if trimmed.is_empty() {
                if has_tr {
                    KIRO_TOOL_RESULTS_PLACEHOLDER.to_string()
                } else {
                    KIRO_EMPTY_USER_PLACEHOLDER.to_string()
                }
            } else {
                trimmed
            };
            u.insert("content".to_string(), json!(content));
            if u.get("modelId")
                .and_then(|v| v.as_str())
                .map(|s| s.is_empty())
                .unwrap_or(true)
            {
                u.insert("modelId".to_string(), json!(model_id));
            }
            if let Some(ctx) = u
                .get_mut("userInputMessageContext")
                .and_then(|v| v.as_object_mut())
            {
                ctx.remove("tools");
            }
        } else if let Some(a) = turn
            .get_mut("assistantResponseMessage")
            .and_then(|v| v.as_object_mut())
        {
            let trimmed = a
                .get("content")
                .map(value_as_text)
                .unwrap_or_default()
                .trim()
                .to_string();
            a.insert(
                "content".to_string(),
                json!(if trimmed.is_empty() { "..." } else { &trimmed }),
            );
        }
    }

    let spec_names: HashSet<String> = tool_specs
        .iter()
        .filter_map(|s| {
            s.get("toolSpecification")
                .and_then(|ts| ts.get("name"))
                .and_then(|n| n.as_str())
                .map(|n| n.to_string())
        })
        .collect();
    let mut used_ids = HashSet::new();

    // Reconcile leading user orphan results + assistant/user tool pairs
    let len = turns.len();
    for idx in (0..len).step_by(2) {
        if idx == 0 {
            if let Some(u) = turns[0]
                .get_mut("userInputMessage")
                .and_then(|v| v.as_object_mut())
            {
                let leading = u
                    .get("userInputMessageContext")
                    .and_then(|c| c.get("toolResults"))
                    .and_then(|r| r.as_array())
                    .cloned()
                    .unwrap_or_default();
                if !leading.is_empty() {
                    for res in &leading {
                        append_text(u, &tool_result_text(res));
                    }
                    if let Some(ctx) = u
                        .get_mut("userInputMessageContext")
                        .and_then(|v| v.as_object_mut())
                    {
                        ctx.remove("toolResults");
                    }
                    clean_user_context(u);
                }
            }
        }
        if idx + 2 < len {
            let (left, right) = turns.split_at_mut(idx + 2);
            let asst_opt = left[idx + 1]
                .get_mut("assistantResponseMessage")
                .and_then(|v| v.as_object_mut());
            let next_user_opt = right[0]
                .get_mut("userInputMessage")
                .and_then(|v| v.as_object_mut());
            if let (Some(asst), Some(next_user)) = (asst_opt, next_user_opt) {
                let calls = asst
                    .get("toolUses")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                let results: Vec<Value> = next_user
                    .get("userInputMessageContext")
                    .and_then(|c| c.get("toolResults"))
                    .and_then(|r| r.as_array())
                    .map(|arr| arr.iter().map(normalize_tool_result).collect())
                    .unwrap_or_default();

                if calls.is_empty() {
                    for res in &results {
                        append_text(next_user, &tool_result_text(res));
                    }
                    if let Some(ctx) = next_user
                        .get_mut("userInputMessageContext")
                        .and_then(|v| v.as_object_mut())
                    {
                        ctx.remove("toolResults");
                    }
                    clean_user_context(next_user);
                } else {
                    let mut result_used = vec![false; results.len()];
                    let mut kept_calls = Vec::new();
                    let mut kept_results = Vec::new();

                    for (call_idx, call) in calls.iter().enumerate() {
                        let key = call.get("toolUseId").and_then(|v| v.as_str()).unwrap_or("");
                        let raw_name = call.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        let mapped_name = name_map
                            .get(raw_name)
                            .map(|s| s.as_str())
                            .unwrap_or(raw_name);
                        let input_val = match call.get("input") {
                            Some(Value::Object(o)) => Some(Value::Object(o.clone())),
                            Some(Value::String(s)) => serde_json::from_str::<Value>(s)
                                .ok()
                                .filter(|v| v.is_object()),
                            None | Some(Value::Null) => Some(json!({})),
                            _ => None,
                        };
                        let matched_res = results
                            .iter()
                            .enumerate()
                            .find(|(ri, r)| {
                                !result_used[*ri]
                                    && r.get("toolUseId").and_then(|v| v.as_str()).unwrap_or("")
                                        == key
                            })
                            .map(|(ri, _)| ri);

                        let has_spec = !mapped_name.is_empty() && spec_names.contains(mapped_name);
                        if let (Some(ri), true, Some(inp)) = (matched_res, has_spec, input_val) {
                            result_used[ri] = true;
                            let new_id = reserve_tool_id(
                                key,
                                idx + 1,
                                call_idx,
                                mapped_name,
                                &mut used_ids,
                            );
                            kept_calls.push(json!({
                                "toolUseId": new_id,
                                "name": mapped_name,
                                "input": inp,
                            }));
                            let mut res_obj = results[ri].as_object().cloned().unwrap_or_default();
                            res_obj.insert("toolUseId".to_string(), json!(new_id));
                            kept_results.push(Value::Object(res_obj));
                        } else {
                            append_text(asst, &tool_call_text(mapped_name, call.get("input")));
                            if let Some(ri) = matched_res {
                                result_used[ri] = true;
                                append_text(next_user, &tool_result_text(&results[ri]));
                            }
                        }
                    }

                    for (ri, res) in results.iter().enumerate() {
                        if !result_used[ri] {
                            append_text(next_user, &tool_result_text(res));
                        }
                    }

                    if kept_calls.is_empty() {
                        asst.remove("toolUses");
                    } else {
                        asst.insert("toolUses".to_string(), Value::Array(kept_calls));
                    }
                    let ctx = next_user
                        .entry("userInputMessageContext".to_string())
                        .or_insert_with(|| json!({}));
                    if let Some(ctx_obj) = ctx.as_object_mut() {
                        if kept_results.is_empty() {
                            ctx_obj.remove("toolResults");
                        } else {
                            ctx_obj.insert("toolResults".to_string(), Value::Array(kept_results));
                        }
                    }
                    clean_user_context(next_user);
                }
            }
        }
    }

    let mut final_current = turns.pop().unwrap_or_else(|| {
        json!({ "userInputMessage": { "content": KIRO_EMPTY_USER_PLACEHOLDER, "modelId": model_id } })
    });
    if let Some(u) = final_current
        .get_mut("userInputMessage")
        .and_then(|v| v.as_object_mut())
    {
        let ctx = u
            .entry("userInputMessageContext".to_string())
            .or_insert_with(|| json!({}));
        if let Some(ctx_obj) = ctx.as_object_mut() {
            if !tool_specs.is_empty() {
                ctx_obj.insert("tools".to_string(), Value::Array(tool_specs.to_vec()));
            }
        }
        clean_user_context(u);
    }

    (turns, final_current)
}

fn extract_claude_images(content: &Value) -> Vec<Value> {
    let Some(arr) = content.as_array() else {
        return Vec::new();
    };
    let mut imgs = Vec::new();
    for block in arr {
        if block.get("type").and_then(|v| v.as_str()) == Some(claude_block::IMAGE) {
            if let Some(src) = block.get("source").and_then(|v| v.as_object()) {
                if src.get("type").and_then(|v| v.as_str()) == Some("base64") {
                    if let Some(data) = src.get("data").and_then(|v| v.as_str()) {
                        let mt = src
                            .get("media_type")
                            .and_then(|v| v.as_str())
                            .unwrap_or("image/png");
                        let fmt = mt.split('/').nth(1).unwrap_or("png");
                        imgs.push(json!({
                            "format": fmt,
                            "source": { "bytes": data },
                        }));
                    }
                }
            }
        }
    }
    imgs
}

fn extract_claude_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(arr) => arr
            .iter()
            .filter_map(|b| {
                if b.get("type").and_then(|v| v.as_str()) == Some(claude_block::TEXT) {
                    b.get("text").and_then(|v| v.as_str())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn claude_tool_result_to_kiro(block: &Value) -> Value {
    let content = match block.get("content") {
        Some(Value::String(s)) => vec![json!({ "text": s })],
        Some(Value::Array(arr)) => {
            let items: Vec<Value> = arr
                .iter()
                .filter_map(|p| {
                    if p.get("type").and_then(|v| v.as_str()) == Some(claude_block::TEXT) {
                        p.get("text").and_then(|v| v.as_str()).map(|t| json!({ "text": t }))
                    } else {
                        None
                    }
                })
                .collect();
            if items.is_empty() {
                vec![json!({ "text": "" })]
            } else {
                items
            }
        }
        _ => vec![json!({ "text": "" })],
    };
    json!({
        "content": content,
        "status": if block.get("is_error").and_then(|v| v.as_bool()) == Some(true) { "error" } else { "success" },
        "toolUseId": block.get("tool_use_id").and_then(|v| v.as_str()).unwrap_or(""),
    })
}

pub fn claude_to_kiro_request(
    model: &str,
    body: &Value,
    _stream: bool,
    credentials: Option<&Value>,
) -> Value {
    let origin = get_kiro_origin(credentials);
    let profile_arn = credentials
        .and_then(|c| {
            c.get("profileArn")
                .or_else(|| c.get("providerSpecificData").and_then(|p| p.get("profileArn")))
        })
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let mut sys_prompt = match body.get("system") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|b| b.get("text").and_then(|v| v.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };

    let thinking_enabled = body
        .get("thinking")
        .and_then(|t| t.get("type"))
        .and_then(|v| v.as_str())
        == Some("enabled");
    if thinking_enabled {
        let budget = body
            .get("thinking")
            .and_then(|t| t.get("budget_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(20000);
        let tp = build_kiro_thinking_prompt(budget, KIRO_MAX_REASONING_EFFORT);
        sys_prompt = if sys_prompt.is_empty() {
            tp
        } else {
            format!("{}\n\n{}", tp, sys_prompt)
        };
    }

    let messages = body
        .get("messages")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let tools_vec = body.get("tools").and_then(|v| v.as_array());
    let (tool_specs, name_map) = normalize_kiro_tool_specs(tools_vec);

    let mut history = Vec::new();
    let mut current_message: Option<Value> = None;

    for (idx, msg) in messages.iter().enumerate() {
        let is_last = idx + 1 == messages.len();
        let r = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
        let content_val = msg.get("content").unwrap_or(&Value::Null);

        if r == role::USER {
            let mut text = extract_claude_text(content_val);
            if idx == 0 && !sys_prompt.is_empty() {
                text = if text.is_empty() {
                    sys_prompt.clone()
                } else {
                    format!("{}\n\n{}", sys_prompt, text)
                };
            }
            let images = extract_claude_images(content_val);
            let tool_results: Vec<Value> = content_val
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter(|b| {
                            b.get("type").and_then(|v| v.as_str()) == Some(claude_block::TOOL_RESULT)
                        })
                        .map(claude_tool_result_to_kiro)
                        .collect()
                })
                .unwrap_or_default();

            let mut user_msg = Map::new();
            user_msg.insert(
                "content".to_string(),
                json!(if text.is_empty() {
                    if tool_results.is_empty() {
                        KIRO_EMPTY_USER_PLACEHOLDER
                    } else {
                        KIRO_TOOL_RESULTS_PLACEHOLDER
                    }
                } else {
                    &text
                }),
            );
            user_msg.insert("modelId".to_string(), json!(model));
            user_msg.insert("origin".to_string(), json!(origin));
            if !images.is_empty() {
                user_msg.insert("images".to_string(), Value::Array(images));
            }
            if !tool_results.is_empty() {
                user_msg.insert(
                    "userInputMessageContext".to_string(),
                    json!({ "toolResults": tool_results }),
                );
            }

            let wrapped = json!({ "userInputMessage": Value::Object(user_msg) });
            if is_last {
                current_message = Some(wrapped);
            } else {
                history.push(wrapped);
            }
        } else if r == role::ASSISTANT {
            let text = extract_claude_text(content_val);
            let tool_uses: Vec<Value> = content_val
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter(|b| {
                            b.get("type").and_then(|v| v.as_str()) == Some(claude_block::TOOL_USE)
                        })
                        .map(|b| {
                            json!({
                                "toolUseId": b.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                                "name": b.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                                "input": b.get("input").cloned().unwrap_or_else(|| json!({})),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();

            let mut asst_msg = Map::new();
            asst_msg.insert(
                "content".to_string(),
                json!(if text.is_empty() { "..." } else { &text }),
            );
            if !tool_uses.is_empty() {
                asst_msg.insert("toolUses".to_string(), Value::Array(tool_uses));
            }
            history.push(json!({ "assistantResponseMessage": Value::Object(asst_msg) }));
        }
    }

    if current_message.is_none() {
        let content = if !sys_prompt.is_empty() && history.is_empty() {
            sys_prompt
        } else {
            KIRO_EMPTY_USER_PLACEHOLDER.to_string()
        };
        current_message = Some(json!({
            "userInputMessage": {
                "content": content,
                "modelId": model,
                "origin": origin,
            }
        }));
    }

    let (canon_history, canon_current) =
        canonicalize_kiro_conversation(history, current_message, model, &tool_specs, &name_map);

    let mut conv_state = json!({
        "chatTriggerType": "MANUAL",
        "currentMessage": canon_current,
        "history": canon_history,
    });
    if let Some(conv_id) = body.get("conversationId").and_then(|v| v.as_str()) {
        conv_state["conversationId"] = json!(conv_id);
    }

    let mut payload = json!({ "conversationState": conv_state });
    if !profile_arn.is_empty() {
        payload["profileArn"] = json!(profile_arn);
    }
    payload
}

fn extract_openai_text_and_images(content: &Value) -> (String, Vec<Value>) {
    match content {
        Value::String(s) => (s.clone(), Vec::new()),
        Value::Array(arr) => {
            let mut texts = Vec::new();
            let mut imgs = Vec::new();
            for p in arr {
                let t = p.get("type").and_then(|v| v.as_str()).unwrap_or("");
                if t == openai_block::TEXT {
                    if let Some(txt) = p.get("text").and_then(|v| v.as_str()) {
                        texts.push(txt.to_string());
                    }
                } else if t == openai_block::IMAGE_URL {
                    let url = p
                        .get("image_url")
                        .and_then(|iu| iu.as_str().or_else(|| iu.get("url").and_then(|u| u.as_str())))
                        .unwrap_or("");
                    if let Some((mime, b64)) = parse_data_uri(url) {
                        let fmt = mime.split('/').nth(1).unwrap_or("png");
                        imgs.push(json!({
                            "format": fmt,
                            "source": { "bytes": b64 },
                        }));
                    }
                }
            }
            (texts.join("\n"), imgs)
        }
        _ => (String::new(), Vec::new()),
    }
}

pub fn openai_to_kiro_request(
    model: &str,
    body: &Value,
    _stream: bool,
    credentials: Option<&Value>,
) -> Value {
    let origin = get_kiro_origin(credentials);
    let profile_arn = credentials
        .and_then(|c| {
            c.get("profileArn")
                .or_else(|| c.get("providerSpecificData").and_then(|p| p.get("profileArn")))
        })
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let messages = body
        .get("messages")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let tools_vec = body.get("tools").and_then(|v| v.as_array());
    let (tool_specs, name_map) = normalize_kiro_tool_specs(tools_vec);

    let mut sys_parts = Vec::new();
    let mut non_sys = Vec::new();
    for m in &messages {
        let r = m.get("role").and_then(|v| v.as_str()).unwrap_or("");
        if r == role::SYSTEM || r == role::DEVELOPER {
            let (t, _) = extract_openai_text_and_images(m.get("content").unwrap_or(&Value::Null));
            if !t.is_empty() {
                sys_parts.push(t);
            }
        } else {
            non_sys.push(m.clone());
        }
    }
    let mut sys_prompt = sys_parts.join("\n");
    if let Some(effort) = body.get("reasoning_effort").and_then(|v| v.as_str()) {
        if effort != "none" {
            let tp = build_kiro_thinking_prompt(20000, effort);
            sys_prompt = if sys_prompt.is_empty() {
                tp
            } else {
                format!("{}\n\n{}", tp, sys_prompt)
            };
        }
    }

    let mut history = Vec::new();
    let mut current_message: Option<Value> = None;

    for (idx, msg) in non_sys.iter().enumerate() {
        let is_last = idx + 1 == non_sys.len();
        let r = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
        let content_val = msg.get("content").unwrap_or(&Value::Null);

        if r == role::TOOL {
            let (txt, _) = extract_openai_text_and_images(content_val);
            let tc_id = msg
                .get("tool_call_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let user_msg = json!({
                "userInputMessage": {
                    "content": KIRO_TOOL_RESULTS_PLACEHOLDER,
                    "modelId": model,
                    "origin": origin,
                    "userInputMessageContext": {
                        "toolResults": [{
                            "content": [{ "text": txt }],
                            "status": "success",
                            "toolUseId": tc_id,
                        }]
                    }
                }
            });
            if is_last {
                current_message = Some(user_msg);
            } else {
                history.push(user_msg);
            }
        } else if r == role::USER {
            let (mut txt, imgs) = extract_openai_text_and_images(content_val);
            if idx == 0 && !sys_prompt.is_empty() {
                txt = if txt.is_empty() {
                    sys_prompt.clone()
                } else {
                    format!("{}\n\n{}", sys_prompt, txt)
                };
            }
            let mut u = Map::new();
            u.insert(
                "content".to_string(),
                json!(if txt.is_empty() {
                    KIRO_EMPTY_USER_PLACEHOLDER
                } else {
                    &txt
                }),
            );
            u.insert("modelId".to_string(), json!(model));
            u.insert("origin".to_string(), json!(origin));
            if !imgs.is_empty() {
                u.insert("images".to_string(), Value::Array(imgs));
            }
            let wrapped = json!({ "userInputMessage": Value::Object(u) });
            if is_last {
                current_message = Some(wrapped);
            } else {
                history.push(wrapped);
            }
        } else if r == role::ASSISTANT {
            let (txt, _) = extract_openai_text_and_images(content_val);
            let tool_uses: Vec<Value> = msg
                .get("tool_calls")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .map(|tc| {
                            let f = tc.get("function");
                            let args = match f.and_then(|x| x.get("arguments")) {
                                Some(Value::String(s)) => parse_json_or_empty(Some(s)),
                                Some(v) if v.is_object() => v.clone(),
                                _ => json!({}),
                            };
                            json!({
                                "toolUseId": tc.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                                "name": f.and_then(|x| x.get("name")).and_then(|v| v.as_str()).unwrap_or(""),
                                "input": args,
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();

            let mut a = Map::new();
            a.insert(
                "content".to_string(),
                json!(if txt.is_empty() { "..." } else { &txt }),
            );
            if !tool_uses.is_empty() {
                a.insert("toolUses".to_string(), Value::Array(tool_uses));
            }
            history.push(json!({ "assistantResponseMessage": Value::Object(a) }));
        }
    }

    if current_message.is_none() {
        let content = if !sys_prompt.is_empty() && history.is_empty() {
            sys_prompt
        } else {
            KIRO_EMPTY_USER_PLACEHOLDER.to_string()
        };
        current_message = Some(json!({
            "userInputMessage": {
                "content": content,
                "modelId": model,
                "origin": origin,
            }
        }));
    }

    let (canon_history, canon_current) =
        canonicalize_kiro_conversation(history, current_message, model, &tool_specs, &name_map);

    let mut conv_state = json!({
        "chatTriggerType": "MANUAL",
        "currentMessage": canon_current,
        "history": canon_history,
    });
    if let Some(conv_id) = body.get("conversationId").and_then(|v| v.as_str()) {
        conv_state["conversationId"] = json!(conv_id);
    }

    let mut payload = json!({ "conversationState": conv_state });
    if !profile_arn.is_empty() {
        payload["profileArn"] = json!(profile_arn);
    }
    payload
}

pub fn kiro_to_claude_response(chunk: Option<&Value>, state: &mut TranslateState) -> Vec<Value> {
    let Some(chunk) = chunk else {
        if state.finished {
            return Vec::new();
        }
        state.finished = true;
        let stop_reason = if state.has_tool_calls {
            "tool_use"
        } else {
            "end_turn"
        };
        return vec![
            json!({
                "type": "message_delta",
                "delta": { "stop_reason": stop_reason, "stop_sequence": Value::Null },
                "usage": { "output_tokens": 0 },
            }),
            json!({ "type": "message_stop" }),
        ];
    };

    if !chunk.is_object() {
        return Vec::new();
    }

    let mut out = Vec::new();
    if !state.started {
        state.started = true;
        let msg_id = state
            .message_id
            .get_or_insert_with(|| format!("msg_{}", now_unix_millis()))
            .clone();
        let model = state
            .model
            .clone()
            .unwrap_or_else(|| "claude-sonnet-4".to_string());
        out.push(json!({
            "type": "message_start",
            "message": {
                "id": msg_id,
                "type": "message",
                "role": role::ASSISTANT,
                "content": [],
                "model": model,
                "stop_reason": Value::Null,
                "stop_sequence": Value::Null,
                "usage": { "input_tokens": 0, "output_tokens": 0 },
            }
        }));
    }

    if let Some(content) = chunk.get("content").and_then(|v| v.as_str()) {
        if !content.is_empty() {
            if !state.text_block_started {
                state.text_block_started = true;
                out.push(json!({
                    "type": "content_block_start",
                    "index": state.claude_next_block_index,
                    "content_block": { "type": claude_block::TEXT, "text": "" },
                }));
            }
            out.push(json!({
                "type": "content_block_delta",
                "index": state.claude_next_block_index,
                "delta": { "type": "text_delta", "text": content },
            }));
        }
    }

    if let Some(name) = chunk.get("name").and_then(|v| v.as_str()) {
        state.has_tool_calls = true;
        if state.text_block_started {
            out.push(json!({
                "type": "content_block_stop",
                "index": state.claude_next_block_index,
            }));
            state.claude_next_block_index += 1;
            state.text_block_started = false;
        }
        let tc_id = chunk
            .get("toolUseId")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| fallback_tool_call_id(state.tool_call_index));
        let idx = state.claude_next_block_index;
        state.claude_next_block_index += 1;
        state.tool_call_index += 1;

        out.push(json!({
            "type": "content_block_start",
            "index": idx,
            "content_block": {
                "type": claude_block::TOOL_USE,
                "id": tc_id,
                "name": name,
                "input": {},
            }
        }));
        if let Some(inp) = chunk.get("input") {
            let partial = match inp {
                Value::String(s) => s.clone(),
                v => serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string()),
            };
            out.push(json!({
                "type": "content_block_delta",
                "index": idx,
                "delta": { "type": "input_json_delta", "partial_json": partial },
            }));
        }
        if chunk.get("stop").and_then(|v| v.as_bool()) == Some(true) {
            out.push(json!({
                "type": "content_block_stop",
                "index": idx,
            }));
        }
    }

    out
}

pub fn kiro_to_openai_response(chunk: Option<&Value>, state: &mut TranslateState) -> Vec<Value> {
    let Some(chunk) = chunk else {
        if state.finished {
            return Vec::new();
        }
        state.finished = true;
        let id = state
            .message_id
            .clone()
            .unwrap_or_else(|| format!("chatcmpl-{}", now_unix_millis()));
        let created = state.created.unwrap_or_else(now_unix_secs);
        let model = state.model.clone().unwrap_or_else(|| "kiro".to_string());
        let finish_reason = to_openai_finish(
            FORMAT_KIRO,
            state.finish_reason.as_deref(),
            state.has_tool_calls,
        );
        let usage = to_openai_usage(FORMAT_KIRO, state.usage.as_ref());
        return vec![build_chunk(
            &id,
            &model,
            created,
            json!({}),
            Some(finish_reason),
            usage,
        )];
    };

    if !chunk.is_object() {
        return Vec::new();
    }

    if state.message_id.is_none() {
        state.message_id = Some(format!("chatcmpl-{}", now_unix_millis()));
        state.created = Some(now_unix_secs());
    }
    let id = state.message_id.clone().unwrap_or_default();
    let created = state.created.unwrap_or_else(now_unix_secs);
    let model = state.model.clone().unwrap_or_else(|| "kiro".to_string());
    let mut out = Vec::new();

    if let Some(content) = chunk.get("content").and_then(|v| v.as_str()) {
        if !content.is_empty() {
            let mut delta = json!({ "content": content });
            if !state.started {
                delta["role"] = json!(role::ASSISTANT);
                state.started = true;
            }
            out.push(build_chunk(&id, &model, created, delta, None, None));
        }
    }

    if let Some(name) = chunk.get("name").and_then(|v| v.as_str()) {
        state.has_tool_calls = true;
        let tc_id = chunk
            .get("toolUseId")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| fallback_tool_call_id(state.tool_call_index));
        let idx = state.tool_call_index;
        state.tool_call_index += 1;

        let args = match chunk.get("input") {
            Some(Value::String(s)) => s.clone(),
            Some(v) => serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string()),
            None => String::new(),
        };
        let mut delta = json!({
            "tool_calls": [{
                "index": idx,
                "id": tc_id,
                "type": openai_block::FUNCTION,
                "function": { "name": name, "arguments": args },
            }]
        });
        if !state.started {
            delta["role"] = json!(role::ASSISTANT);
            state.started = true;
        }
        out.push(build_chunk(&id, &model, created, delta, None, None));
    }

    if chunk.get("usage").is_some() || chunk.get("unit").is_some() {
        state.usage = Some(chunk.clone());
    }

    if chunk.get("stop").and_then(|v| v.as_bool()) == Some(true)
        && chunk.get("name").is_none()
        && !state.finished
    {
        state.finished = true;
        let finish_reason = if state.has_tool_calls {
            openai_finish::TOOL_CALLS
        } else {
            openai_finish::STOP
        };
        let usage = to_openai_usage(FORMAT_KIRO, state.usage.as_ref());
        out.push(build_chunk(
            &id,
            &model,
            created,
            json!({}),
            Some(finish_reason),
            usage,
        ));
    }

    out
}
