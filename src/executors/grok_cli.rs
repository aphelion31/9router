use anyhow::Context;
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::{Map, Value, json};

const GROK_CLI_VERSION: &str = "1.0.44";
const GROK_CLI_CLIENT_IDENTIFIER: &str = "grok-shell";
const GROK_CLI_USER_AGENT: &str = "grok-shell/1.0.44 (linux; x86_64)";

const HOSTED_TOOL_TYPES: &[&str] = &[
    "web_search",
    "x_search",
    "web_search_preview",
    "file_search",
    "image_generation",
    "code_interpreter",
    "mcp",
    "local_shell",
];

const RESPONSES_API_ALLOWLIST: &[&str] = &[
    "model",
    "input",
    "instructions",
    "tools",
    "tool_choice",
    "stream",
    "store",
    "reasoning",
    "include",
    "temperature",
    "top_p",
    "max_output_tokens",
    "parallel_tool_calls",
    "text",
    "metadata",
    "prompt_cache_key",
];

const EFFORT_LEVELS: &[&str] = &["low", "medium", "high", "xhigh"];

fn random_uuid() -> String {
    let mut bytes = [0u8; 16];
    let _ = SystemRandom::new().fill(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

fn is_server_id(id: &str) -> bool {
    id.starts_with("rs_")
        || id.starts_with("fc_")
        || id.starts_with("resp_")
        || id.starts_with("msg_")
}

fn is_native_item_id(id: &str) -> bool {
    let rest = if let Some(r) = id.strip_prefix("rs_") {
        r
    } else if let Some(r) = id.strip_prefix("msg_") {
        r
    } else if let Some(r) = id.strip_prefix("fc_") {
        r
    } else {
        return false;
    };
    let parts: Vec<&str> = rest.split('-').collect();
    parts.len() == 5
        && parts[0].len() == 8
        && parts[1].len() == 4
        && parts[2].len() == 4
        && parts[3].len() == 4
        && parts[4].len() == 12
        && parts
            .iter()
            .all(|p| p.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn stringify_tool_output(val: Option<&Value>) -> String {
    match val {
        None => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

fn normalize_effort(val: Option<&str>) -> &'static str {
    let s = val.unwrap_or("").trim().to_ascii_lowercase();
    if s == "max" {
        return "xhigh";
    }
    match s.as_str() {
        "low" => "low",
        "medium" => "medium",
        "high" => "high",
        "xhigh" => "xhigh",
        _ => "high",
    }
}

fn supports_reasoning_effort(model: &str) -> bool {
    model == "grok-4.5" || model.starts_with("grok-4.5-")
}

fn count_user_turns(input: &[Value]) -> usize {
    let n = input
        .iter()
        .filter(|item| {
            let Some(obj) = item.as_object() else {
                return false;
            };
            let role = obj.get("role").and_then(Value::as_str);
            let ty = obj.get("type").and_then(Value::as_str).unwrap_or("");
            role == Some("user") && (ty.is_empty() || ty == "message")
        })
        .count();
    n.max(1)
}

fn normalize_input(object: &mut Map<String, Value>) {
    let Some(arr) = object.get("input").and_then(Value::as_array) else {
        return;
    };
    let mut normalized = Vec::with_capacity(arr.len());
    for item in arr {
        if let Some(s) = item.as_str() {
            if is_server_id(s) {
                continue;
            }
            normalized.push(item.clone());
            continue;
        }
        let Some(obj) = item.as_object() else {
            normalized.push(item.clone());
            continue;
        };
        let ty = obj.get("type").and_then(Value::as_str).unwrap_or("");
        if ty == "item_reference" {
            continue;
        }
        let mut clean = obj.clone();
        clean.remove("internal_chat_message_metadata_passthrough");

        match ty {
            "reasoning" => {
                let valid_id = clean
                    .get("id")
                    .and_then(Value::as_str)
                    .is_some_and(is_native_item_id);
                let has_enc = clean.get("encrypted_content").is_some_and(Value::is_string);
                if !valid_id || !has_enc {
                    continue;
                }
                normalized.push(Value::Object(clean));
            }
            "custom_tool_call" => {
                let call_id = clean
                    .get("call_id")
                    .or_else(|| clean.get("id"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let name = clean
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if call_id.is_empty() || name.is_empty() {
                    continue;
                }
                let args = json!({
                    "input": stringify_tool_output(clean.get("input").or_else(|| clean.get("arguments")))
                })
                .to_string();
                normalized.push(json!({
                    "type": "function_call",
                    "call_id": call_id,
                    "name": name,
                    "arguments": args,
                }));
            }
            "custom_tool_call_output" | "function_call_output" => {
                let call_id = clean
                    .get("call_id")
                    .or_else(|| clean.get("id"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                if call_id.is_empty() {
                    continue;
                }
                normalized.push(json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": stringify_tool_output(clean.get("output")),
                }));
            }
            "function_call" => {
                let call_id = clean
                    .get("call_id")
                    .or_else(|| clean.get("id"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let name = clean
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if call_id.is_empty() || name.is_empty() {
                    continue;
                }
                let mut fc = Map::new();
                fc.insert("type".into(), json!("function_call"));
                if let Some(id) = clean.get("id").and_then(Value::as_str) {
                    if is_native_item_id(id) {
                        fc.insert("id".into(), json!(id));
                    }
                }
                fc.insert("call_id".into(), json!(call_id));
                fc.insert("name".into(), json!(name));
                let args = match clean.get("arguments") {
                    Some(Value::String(s)) => s.clone(),
                    Some(other) => other.to_string(),
                    None => "{}".to_string(),
                };
                fc.insert("arguments".into(), json!(args));
                if let Some(status) = clean.get("status").and_then(Value::as_str) {
                    fc.insert("status".into(), json!(status));
                }
                normalized.push(Value::Object(fc));
            }
            _ => {
                if let Some(id) = clean.get("id").and_then(Value::as_str) {
                    if is_server_id(id) && !is_native_item_id(id) {
                        clean.remove("id");
                    }
                }
                normalized.push(Value::Object(clean));
            }
        }
    }

    let call_ids: std::collections::HashSet<String> = normalized
        .iter()
        .filter_map(|item| {
            let obj = item.as_object()?;
            if obj.get("type").and_then(Value::as_str) == Some("function_call") {
                obj.get("call_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            } else {
                None
            }
        })
        .collect();
    normalized.retain(|item| {
        let Some(obj) = item.as_object() else {
            return true;
        };
        if obj.get("type").and_then(Value::as_str) == Some("function_call_output") {
            obj.get("call_id")
                .and_then(Value::as_str)
                .is_some_and(|id| call_ids.contains(id))
        } else {
            true
        }
    });
    object.insert("input".into(), Value::Array(normalized));
}

fn normalize_tools(object: &mut Map<String, Value>) {
    let Some(tools) = object.get("tools").and_then(Value::as_array) else {
        object.remove("tools");
        object.remove("tool_choice");
        return;
    };
    if tools.is_empty() {
        object.remove("tools");
        object.remove("tool_choice");
        return;
    }

    let mut valid_names = std::collections::HashSet::new();
    let mut hosted_types = std::collections::HashSet::new();
    let mut out_tools = Vec::new();

    for tool in tools {
        let Some(obj) = tool.as_object() else {
            continue;
        };
        let ty = obj.get("type").and_then(Value::as_str).unwrap_or("");
        if HOSTED_TOOL_TYPES.contains(&ty) {
            hosted_types.insert(ty.to_string());
            out_tools.push(tool.clone());
            continue;
        }
        let fn_obj = obj.get("function").and_then(Value::as_object);
        let is_function =
            ty == "function" || ty == "custom" || ty.is_empty() || fn_obj.is_some() || obj.get("name").is_some_and(Value::is_string);
        if !is_function {
            continue;
        }
        let raw_name = obj
            .get("name")
            .and_then(Value::as_str)
            .or_else(|| fn_obj.and_then(|f| f.get("name")).and_then(Value::as_str))
            .unwrap_or("")
            .trim();
        if raw_name.is_empty() {
            continue;
        }
        let name: String = raw_name.chars().take(128).collect();
        let description = obj
            .get("description")
            .and_then(Value::as_str)
            .or_else(|| {
                fn_obj
                    .and_then(|f| f.get("description"))
                    .and_then(Value::as_str)
            })
            .unwrap_or("");
        let parameters = if ty == "custom" {
            json!({
                "type": "object",
                "properties": {"input": {"type": "string"}},
                "required": ["input"],
            })
        } else if let Some(p) = obj.get("parameters").filter(|v| v.is_object()) {
            p.clone()
        } else if let Some(p) = fn_obj
            .and_then(|f| f.get("parameters"))
            .filter(|v| v.is_object())
        {
            p.clone()
        } else {
            json!({"type": "object", "properties": {}})
        };

        let mut new_tool = Map::new();
        new_tool.insert("type".into(), json!("function"));
        new_tool.insert("name".into(), json!(name));
        if !description.is_empty() {
            new_tool.insert("description".into(), json!(description));
        }
        new_tool.insert("parameters".into(), parameters);
        valid_names.insert(name);
        out_tools.push(Value::Object(new_tool));
    }

    if out_tools.is_empty() {
        object.remove("tools");
        object.remove("tool_choice");
        return;
    }
    object.insert("tools".into(), Value::Array(out_tools));

    if let Some(choice) = object.get("tool_choice").and_then(Value::as_object).cloned() {
        let choice_ty = choice.get("type").and_then(Value::as_str).unwrap_or("");
        if matches!(choice_ty, "function" | "custom") {
            let raw_name = choice
                .get("name")
                .and_then(Value::as_str)
                .or_else(|| {
                    choice
                        .get("function")
                        .and_then(Value::as_object)
                        .and_then(|f| f.get("name"))
                        .and_then(Value::as_str)
                })
                .unwrap_or("")
                .trim();
            let name: String = raw_name.chars().take(128).collect();
            if name.is_empty() || !valid_names.contains(&name) {
                object.remove("tool_choice");
            } else {
                object.insert(
                    "tool_choice".into(),
                    json!({"type": "function", "name": name}),
                );
            }
        } else if !hosted_types.contains(choice_ty) {
            object.remove("tool_choice");
        }
    }
}

pub(super) fn prepare(
    model: &str,
    body: &mut Value,
    provider_specific_data: Option<&Value>,
) -> anyhow::Result<Vec<(&'static str, String)>> {
    let object = body
        .as_object_mut()
        .context("Grok CLI request body must be an object")?;

    let session_id = object
        .get("prompt_cache_key")
        .or_else(|| object.get("session_id"))
        .or_else(|| object.get("conversation_id"))
        .or_else(|| {
            object
                .get("metadata")
                .and_then(|m| m.get("session_id"))
        })
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            provider_specific_data
                .and_then(|d| d.get("workspaceId"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(random_uuid);

    let input_empty = match object.get("input") {
        None | Some(Value::Null) => true,
        Some(Value::Array(a)) => a.is_empty(),
        _ => false,
    };
    if input_empty {
        if let Some(messages) = object.get("messages").and_then(Value::as_array) {
            if !messages.is_empty() {
                let mapped: Vec<Value> = messages
                    .iter()
                    .map(|m| {
                        let role = m.get("role").and_then(Value::as_str).unwrap_or("user");
                        let content = match m.get("content") {
                            Some(Value::String(s)) => s.clone(),
                            Some(other) => other.to_string(),
                            None => String::new(),
                        };
                        json!({"type": "message", "role": role, "content": content})
                    })
                    .collect();
                object.insert("input".into(), Value::Array(mapped));
                object.remove("messages");
            } else {
                object.insert(
                    "input".into(),
                    json!([{"type": "message", "role": "user", "content": "..."}]),
                );
            }
        } else {
            object.insert(
                "input".into(),
                json!([{"type": "message", "role": "user", "content": "..."}]),
            );
        }
    }

    normalize_input(object);
    normalize_tools(object);

    let turn_idx = object
        .get("input")
        .and_then(Value::as_array)
        .map(|a| count_user_turns(a))
        .unwrap_or(1);

    object.insert("stream".into(), Value::Bool(true));
    object.insert("store".into(), Value::Bool(false));

    let raw_model = object
        .get("model")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(model)
        .to_string();
    let mut model_effort: Option<&str> = None;
    let mut resolved_model = raw_model.clone();
    for level in EFFORT_LEVELS {
        let suffix = format!("-{level}");
        if let Some(stripped) = resolved_model.strip_suffix(&suffix) {
            model_effort = Some(level);
            resolved_model = stripped.to_string();
            break;
        }
    }
    object.insert("model".into(), json!(resolved_model));

    let req_effort = object
        .get("reasoning_effort")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let supports_effort = supports_reasoning_effort(&resolved_model);
    if !object.get("reasoning").is_some_and(Value::is_object) {
        let mut r = Map::new();
        r.insert("summary".into(), json!("concise"));
        if supports_effort {
            let eff = normalize_effort(req_effort.as_deref().or(model_effort));
            r.insert("effort".into(), json!(eff));
        }
        object.insert("reasoning".into(), Value::Object(r));
    } else if let Some(r) = object.get_mut("reasoning").and_then(Value::as_object_mut) {
        if supports_effort {
            let existing = r.get("effort").and_then(Value::as_str).map(str::to_owned);
            let eff = normalize_effort(
                existing
                    .as_deref()
                    .or(req_effort.as_deref())
                    .or(model_effort),
            );
            r.insert("effort".into(), json!(eff));
        } else {
            r.remove("effort");
        }
        if !r.contains_key("summary") {
            r.insert("summary".into(), json!("concise"));
        }
    }
    object.remove("reasoning_effort");

    let effort_is_none = object
        .get("reasoning")
        .and_then(|r| r.get("effort"))
        .and_then(Value::as_str)
        == Some("none");
    if !effort_is_none {
        let mut include = object
            .get("include")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if !include
            .iter()
            .any(|v| v.as_str() == Some("reasoning.encrypted_content"))
        {
            include.push(json!("reasoning.encrypted_content"));
        }
        object.insert("include".into(), Value::Array(include));
    }

    object.retain(|k, _| RESPONSES_API_ALLOWLIST.contains(&k.as_str()));

    let mut headers = vec![
        ("user-agent", GROK_CLI_USER_AGENT.to_string()),
        (
            "x-grok-client-identifier",
            GROK_CLI_CLIENT_IDENTIFIER.to_string(),
        ),
        ("x-grok-client-version", GROK_CLI_VERSION.to_string()),
        ("x-grok-session-id", session_id.clone()),
        ("x-grok-conv-id", session_id),
        ("x-grok-req-id", random_uuid()),
        ("x-grok-turn-idx", turn_idx.to_string()),
        ("x-grok-model-override", resolved_model),
        ("accept", "text/event-stream".to_string()),
    ];

    if let Some(psd) = provider_specific_data {
        if let Some(agent_id) = psd
            .get("deviceId")
            .or_else(|| psd.get("agentId"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            headers.push(("x-grok-agent-id", agent_id.to_string()));
        }
        if let Some(email) = psd.get("email").and_then(Value::as_str) {
            if !email.is_empty() {
                headers.push(("x-email", email.to_string()));
            }
        }
        if let Some(user_id) = psd
            .get("userId")
            .or_else(|| psd.get("providerUserId"))
            .and_then(Value::as_str)
        {
            if !user_id.is_empty() {
                headers.push(("x-userid", user_id.to_string()));
            }
        }
    }

    Ok(headers)
}
