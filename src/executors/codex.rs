use serde_json::{Map, Value, json};
use std::collections::HashSet;

const RESPONSES_API_ALLOWLIST: &[&str] = &[
    "model",
    "input",
    "instructions",
    "tools",
    "tool_choice",
    "stream",
    "store",
    "reasoning",
    "service_tier",
    "include",
    "prompt_cache_key",
    "client_metadata",
    "text",
    "parallel_tool_calls",
];

const CODEX_HOSTED_TOOL_TYPES: &[&str] = &[
    "image_generation",
    "web_search",
    "web_search_preview",
    "file_search",
    "computer",
    "computer_use_preview",
    "code_interpreter",
    "mcp",
    "local_shell",
    "tool_search",
    "custom",
];

fn is_server_id(s: &str) -> bool {
    s.starts_with("rs_")
        || s.starts_with("fc_")
        || s.starts_with("resp_")
        || s.starts_with("msg_")
}

pub fn convert_system_to_developer_role(body: &mut Map<String, Value>) {
    let Some(input) = body.get_mut("input").and_then(Value::as_array_mut) else {
        return;
    };
    for item in input {
        let Some(obj) = item.as_object_mut() else {
            continue;
        };
        let is_msg = obj
            .get("type")
            .and_then(Value::as_str)
            .map(|t| t == "message")
            .unwrap_or(true);
        if is_msg && obj.get("role").and_then(Value::as_str) == Some("system") {
            obj.insert("role".into(), json!("developer"));
        }
    }
}

pub fn strip_stored_item_references(body: &mut Map<String, Value>) {
    let Some(input) = body.get_mut("input").and_then(Value::as_array_mut) else {
        return;
    };
    input.retain_mut(|item| {
        if let Some(s) = item.as_str() {
            return !is_server_id(s);
        }
        if let Some(obj) = item.as_object_mut() {
            if obj.get("type").and_then(Value::as_str) == Some("item_reference") {
                return false;
            }
            if let Some(id) = obj.get("id").and_then(Value::as_str)
                && is_server_id(id)
            {
                obj.remove("id");
            }
        }
        true
    });
}

pub fn normalize_codex_tools(body: &mut Map<String, Value>) {
    let mut valid_names = HashSet::new();
    if let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) {
        let mut out = Vec::with_capacity(tools.len());
        for tool in tools.drain(..) {
            let Some(obj) = tool.as_object().cloned() else {
                continue;
            };
            let tool_type = obj.get("type").and_then(Value::as_str).unwrap_or("");
            if tool_type == "namespace" {
                if let Some(sub) = obj.get("tools").and_then(Value::as_array) {
                    for st in sub {
                        if let Some(name) = st.get("name").and_then(Value::as_str) {
                            let trimmed = name.trim();
                            if !trimmed.is_empty() {
                                valid_names.insert(trimmed.chars().take(128).collect::<String>());
                            }
                        }
                    }
                }
                out.push(Value::Object(obj));
                continue;
            }
            if tool_type != "function" {
                if CODEX_HOSTED_TOOL_TYPES.contains(&tool_type) {
                    out.push(Value::Object(obj));
                }
                continue;
            }
            let fn_obj = obj.get("function").and_then(Value::as_object).cloned();
            let raw_name = obj
                .get("name")
                .and_then(Value::as_str)
                .or_else(|| fn_obj.as_ref()?.get("name").and_then(Value::as_str))
                .unwrap_or("")
                .trim();
            if raw_name.is_empty() {
                continue;
            }
            let name: String = raw_name.chars().take(128).collect();
            let description = obj
                .get("description")
                .and_then(Value::as_str)
                .or_else(|| fn_obj.as_ref()?.get("description").and_then(Value::as_str))
                .map(str::to_string);
            let parameters = obj
                .get("parameters")
                .and_then(Value::as_object)
                .cloned()
                .or_else(|| fn_obj.as_ref()?.get("parameters").and_then(Value::as_object).cloned())
                .map(Value::Object)
                .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
            let strict = obj
                .get("strict")
                .and_then(Value::as_bool)
                .or_else(|| fn_obj.as_ref()?.get("strict").and_then(Value::as_bool));

            let mut flat = Map::new();
            flat.insert("type".into(), json!("function"));
            flat.insert("name".into(), json!(name));
            if let Some(desc) = description
                && !desc.is_empty()
            {
                flat.insert("description".into(), json!(desc));
            }
            flat.insert("parameters".into(), parameters);
            if let Some(s) = strict {
                flat.insert("strict".into(), json!(s));
            }
            valid_names.insert(name);
            out.push(Value::Object(flat));
        }
        *tools = out;
    }

    if let Some(tc) = body.get("tool_choice").and_then(Value::as_object)
        && tc.get("type").and_then(Value::as_str) == Some("function")
    {
        let name = tc.get("name").and_then(Value::as_str).unwrap_or("").trim();
        if name.is_empty() || !valid_names.contains(name) {
            body.remove("tool_choice");
        }
    }
}

pub fn prepare(
    body: &mut Value,
    provider_specific_data: Option<&Value>,
) -> anyhow::Result<Vec<(&'static str, String)>> {
    let obj = body
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("Codex request body must be an object"))?;

    convert_system_to_developer_role(obj);
    strip_stored_item_references(obj);
    normalize_codex_tools(obj);

    let model = obj.get("model").and_then(Value::as_str).unwrap_or("").to_owned();
    let lite = matches!(model.split('[').next().unwrap_or(""), "gpt-6.1-sol" | "gpt-6.1-terra" | "gpt-6-sol" | "gpt-6-luna")
        && !obj.get("tools").and_then(Value::as_array).is_some_and(|tools| tools.iter().any(|t| t["type"] == "web_search"));
    let session_body = Value::Object(obj.clone());
    let session = crate::services::systemone_router::client_session_id(&Default::default(), &session_body)
        .or_else(|| provider_specific_data.and_then(|d| d.get("workspaceId").or_else(|| d.get("connectionId"))).and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_else(|| crate::services::systemone_router::session_id(&Default::default(), &session_body));
    obj.entry("prompt_cache_key").or_insert_with(|| json!(session));

    obj.insert("stream".into(), json!(true));
    obj.insert("store".into(), json!(false));
    if lite {
        let instructions = obj.get("instructions").and_then(Value::as_str).filter(|s| !s.trim().is_empty()).map(str::to_owned);
        let tools = obj.get("tools").and_then(Value::as_array).cloned().unwrap_or_default();
        let input = obj.entry("input").or_insert_with(|| json!([]));
        if let Some(input) = input.as_array_mut() {
            if !input.iter().any(|item| item["type"] == "additional_tools") {
                if let Some(text) = instructions { input.insert(0, json!({"type":"message","role":"developer","content":[{"type":"input_text","text":text}]})); }
                input.insert(0, json!({"type":"additional_tools","role":"developer","tools":tools}));
            }
        }
        obj.insert("instructions".into(), json!(""));
        obj.insert("tools".into(), Value::Null);
        obj.entry("tool_choice").or_insert_with(|| json!("auto"));
        obj.insert("parallel_tool_calls".into(), json!(false));
        let reasoning = obj.entry("reasoning").or_insert_with(|| json!({"effort":"medium"}));
        if let Some(reasoning) = reasoning.as_object_mut() {
            reasoning.entry("effort").or_insert_with(|| json!("medium"));
            reasoning.insert("context".into(), json!("all_turns"));
        }
    } else {
        obj.entry("instructions").or_insert_with(|| json!("You are a helpful assistant."));
    }

    if obj.get("reasoning").is_some_and(Value::is_object) {
        let include = obj
            .entry("include")
            .or_insert_with(|| json!([]))
            .as_array_mut();
        if let Some(arr) = include
            && !arr
                .iter()
                .any(|v| v.as_str() == Some("reasoning.encrypted_content"))
        {
            arr.push(json!("reasoning.encrypted_content"));
        }
    }

    obj.retain(|k, _| RESPONSES_API_ALLOWLIST.contains(&k.as_str()));

    let mut headers = vec![
        ("originator", "codex_cli_rs".to_string()),
        ("session_id", session),
        ("user-agent", "codex_cli_rs/0.159.0".to_string()),
        ("version", "0.159.0".to_string()),
        ("accept-language", "*".to_string()),
        ("sec-fetch-mode", "cors".to_string()),
    ];
    if lite { headers.push(("x-openai-internal-codex-responses-lite", "true".to_string())); }
    if let Some(account_id) = provider_specific_data.and_then(|d| {
        d.get("workspaceId")
            .or_else(|| d.get("chatgptAccountId"))
            .or_else(|| d.get("accountId"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    }) {
        headers.push(("ChatGPT-Account-ID", account_id.to_string()));
    }
    Ok(headers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lite_wire_request_matches_codex_cli() {
        let mut body = json!({"model":"gpt-6.1-terra","instructions":"policy", "input":[{"type":"message","role":"user","content":[{"type":"input_text","text":"pong"}]}]});
        let headers = prepare(&mut body, None).unwrap();
        assert_eq!(body["input"][0], json!({"type":"additional_tools","role":"developer","tools":[]}));
        assert_eq!(body["input"][1], json!({"type":"message","role":"developer","content":[{"type":"input_text","text":"policy"}]}));
        assert_eq!(body["instructions"], "");
        assert_eq!(body["tools"], Value::Null);
        assert_eq!(body["tool_choice"], "auto");
        assert_eq!(body["parallel_tool_calls"], false);
        assert_eq!(body["reasoning"], json!({"effort":"medium","context":"all_turns"}));
        assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
        assert!(headers.iter().any(|(k,v)| *k == "session_id" && body["prompt_cache_key"] == *v));
        assert!(headers.iter().any(|(k,v)| *k == "x-openai-internal-codex-responses-lite" && v == "true"));
        assert!(headers.iter().any(|(k,v)| *k == "user-agent" && v == "codex_cli_rs/0.159.0"));
    }

    #[test]
    fn codex_prepare_shapes_input_tools_and_headers() {
        let mut body = json!({
            "model": "gpt-5.3-codex",
            "input": [
                {"role": "system", "content": "Be concise"},
                {"id": "rs_123", "role": "user", "content": "Hi"},
                {"type": "item_reference", "id": "fc_456"},
                "msg_789"
            ],
            "tools": [
                {"type": "function", "function": {"name": "read_file", "description": "Read", "parameters": {"type": "object"}, "strict": true}},
                {"type": "web_search"}
            ],
            "tool_choice": {"type": "function", "name": "missing_tool"},
            "temperature": 0.5,
            "reasoning": {"effort": "high"}
        });
        let psd = json!({"chatgptAccountId": "acct_42"});
        let headers = prepare(&mut body, Some(&psd)).unwrap();

        assert_eq!(body["stream"], true);
        assert_eq!(body["store"], false);
        assert_eq!(body["instructions"], "You are a helpful assistant.");
        assert!(body.get("temperature").is_none());
        assert!(body.get("tool_choice").is_none());
        assert_eq!(body["input"].as_array().unwrap().len(), 2);
        assert_eq!(body["input"][0]["role"], "developer");
        assert!(body["input"][1].get("id").is_none());
        assert_eq!(body["tools"][0]["name"], "read_file");
        assert!(body["tools"][0].get("function").is_none());
        assert_eq!(body["tools"][1]["type"], "web_search");
        assert_eq!(body["include"][0], "reasoning.encrypted_content");
        assert!(headers.iter().any(|(k, v)| *k == "originator" && v == "codex_cli_rs"));
        assert!(headers.iter().any(|(k, v)| *k == "ChatGPT-Account-ID" && v == "acct_42"));
    }
}
