//! Claude Code cloaking for OAuth (`sk-ant-oat`) tokens. Port of upstream
//! `open-sse/utils/claudeCloaking.js` (`applyCloaking`) and the claude CLI
//! fingerprint from `open-sse/providers/shared.js` (`CLAUDE_CLI_SPOOF_HEADERS`).
//! Without it Anthropic answers OAuth claude calls with a 429 `{"message":"Error"}`.
use ring::digest::{SHA256, digest};
use serde_json::{Value, json};
use std::collections::HashMap;

const DECOYS: &[&str] = &["Task", "TaskOutput", "TaskStop", "TaskCreate", "TaskGet", "TaskUpdate", "TaskList", "Bash", "Glob", "Grep", "Read", "Edit", "Write", "NotebookEdit", "WebFetch", "WebSearch", "AskUserQuestion", "Skill", "EnterPlanMode", "ExitPlanMode"];

pub type ToolNameMap = HashMap<String, String>;

pub fn cloak_tools_on_oauth(body: &mut Value, provider: &str, token: Option<&str>) -> Option<ToolNameMap> {
    if provider == "claude" && token.is_some_and(is_oauth_token) {
        Some(cloak_tools(body))
    } else {
        None
    }
}

pub fn cloak_tools(body: &mut Value) -> ToolNameMap {
    let mut map = ToolNameMap::new();
    let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) else { return map };
    if tools.is_empty() { return map; }
    for tool in tools.iter_mut() {
        if tool.get("type").is_some_and(|v| !v.is_null() && v != "" && v != false) { continue; }
        if let Some(name) = tool.get("name").and_then(Value::as_str).map(str::to_owned) {
            let cloaked = format!("{name}_ide");
            map.insert(cloaked.clone(), name);
            tool["name"] = json!(cloaked);
        }
    }
    tools.extend(DECOYS.iter().map(|name| json!({"name": name, "description": "This tool is currently unavailable.", "input_schema": {"type":"object", "properties":{}}})));
    if let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) {
        for block in messages.iter_mut().filter_map(|m| m.get_mut("content").and_then(Value::as_array_mut)).flatten() {
            if block["type"] == "tool_use" && let Some(name) = block.get("name").and_then(Value::as_str) {
                block["name"] = json!(format!("{name}_ide"));
            } else if block["type"] == "tool_result" && let Some(name) = block.get("name").and_then(Value::as_str) {
                let cloaked = format!("{name}_ide");
                if map.contains_key(&cloaked) { block["name"] = json!(cloaked); }
            }
        }
    }
    if body["tool_choice"]["type"] == "tool" && let Some(name) = body["tool_choice"]["name"].as_str() {
        let cloaked = format!("{name}_ide");
        if map.contains_key(&cloaked) { body["tool_choice"]["name"] = json!(cloaked); }
    }
    map
}

fn decloak_block(block: &mut Value, map: &ToolNameMap) {
    if block["type"] != "tool_use" { return; }
    if let Some(name) = block.get("name").and_then(Value::as_str) {
        let original = map.get(name).map(String::as_str).or_else(|| {
            if DECOYS.contains(&name) { None } else { name.strip_suffix("_ide").filter(|s| !s.is_empty()) }
        });
        if let Some(original) = original { block["name"] = json!(original); }
    }
}

pub fn decloak_tool_names(body: &mut Value, map: &ToolNameMap) {
    if let Some(content) = body.get_mut("content").and_then(Value::as_array_mut) {
        for block in content { decloak_block(block, map); }
    }
}

pub fn decloak_stream_chunk(chunk: &mut Value, map: &ToolNameMap) {
    if chunk["type"] == "content_block_start" && let Some(block) = chunk.get_mut("content_block") {
        decloak_block(block, map);
    }
}

#[cfg(test)]
mod tool_tests {
    use super::*;

    #[test]
    fn cloak_tools_matches_upstream() {
        let mut body = json!({"tools":[{"name":"Read"},{"name":"web_search","type":"web_search_20250305"}],"tool_choice":{"type":"tool","name":"Read"},"messages":[{"content":[{"type":"tool_use","name":"Read"},{"type":"tool_result","name":"Read"}]}]});
        let map = cloak_tools(&mut body);
        assert_eq!(map.get("Read_ide").unwrap(), "Read");
        assert_eq!(body["tools"][0]["name"], "Read_ide");
        assert_eq!(body["tools"][1]["name"], "web_search");
        assert_eq!(body["tools"].as_array().unwrap().len(), 22);
        assert_eq!(body["tool_choice"]["name"], "Read_ide");
        assert_eq!(body["messages"][0]["content"][0]["name"], "Read_ide");
        assert_eq!(body["messages"][0]["content"][1]["name"], "Read_ide");
    }

    #[test]
    fn decloak_responses_and_fallback() {
        let map = ToolNameMap::from([("Read_ide".into(), "Read".into())]);
        let mut body = json!({"content":[{"type":"tool_use","name":"Read_ide"},{"type":"tool_use","name":"other_ide"},{"type":"tool_use","name":"Bash"}]});
        decloak_tool_names(&mut body, &map);
        assert_eq!(body["content"][0]["name"], "Read");
        assert_eq!(body["content"][1]["name"], "other");
        assert_eq!(body["content"][2]["name"], "Bash");
        let mut chunk = json!({"type":"content_block_start","content_block":{"type":"tool_use","name":"Read_ide"}});
        decloak_stream_chunk(&mut chunk, &map);
        assert_eq!(chunk["content_block"]["name"], "Read");
    }

    #[test]
    fn api_key_and_other_providers_are_untouched() {
        let original = json!({"tools":[{"name":"Read"}]});
        for (provider, token) in [("claude", "sk-ant-api03-SYNTHETIC"), ("other", "sk-ant-oat-SYNTHETIC")] {
            let mut body = original.clone();
            assert!(cloak_tools_on_oauth(&mut body, provider, Some(token)).is_none());
            assert_eq!(body, original);
        }
        let mut body = original;
        assert!(cloak_tools_on_oauth(&mut body, "claude", Some("sk-ant-oat-SYNTHETIC")).is_some());
        assert_eq!(body["tools"][0]["name"], "Read_ide");
    }
}

pub const CLAUDE_CLI_VERSION: &str = "2.1.322";
const CC_ENTRYPOINT: &str = "sdk-cli";

pub const CLAUDE_CLI_BETA: &str = "claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14,context-management-2025-06-27,prompt-caching-scope-2026-01-05,advanced-tool-use-2025-11-20,effort-2025-11-24,structured-outputs-2025-12-15,fast-mode-2026-02-01,redact-thinking-2026-02-12,token-efficient-tools-2026-03-28";

pub fn is_oauth_token(token: &str) -> bool {
    token.contains("sk-ant-oat")
}

fn sha256_hex(input: &str) -> String {
    digest(&SHA256, input.as_bytes()).as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

fn billing_header(payload: &Value) -> String {
    let hash = sha256_hex(&payload.to_string());
    let build = &hash[..3];
    let cch = &hash[..5];
    format!("x-anthropic-billing-header: cc_version={CLAUDE_CLI_VERSION}.{build}; cc_entrypoint={CC_ENTRYPOINT}; cch={cch};")
}

/// Stable UUID-v4 shaped string derived from a seed (stable per account), like upstream `deriveUuid`.
fn derive_uuid(seed: &str) -> String {
    let h = sha256_hex(seed);
    let variant = format!("{:x}", (u8::from_str_radix(&h[16..17], 16).unwrap_or(0) & 0x3) | 0x8);
    format!("{}-{}-4{}-{}{}-{}", &h[0..8], &h[8..12], &h[13..16], variant, &h[17..20], &h[20..32])
}

fn fake_user_id(session_id: &str, api_key: &str) -> String {
    let device_id = sha256_hex(&format!("device:{api_key}"));
    let account_uuid = derive_uuid(&format!("account:{api_key}"));
    let session = session_id.trim_start_matches("claude:").trim();
    format!(r#"{{"device_id":"{device_id}","account_uuid":"{account_uuid}","session_id":"{session}"}}"#)
}

/// Upstream `applyCloaking`: billing block as `system[0]` and fake `metadata.user_id`.
/// No-op for non-OAuth tokens. Returns the body unchanged for non-object input.
pub fn apply_cloaking(body: &mut Value, token: &str, session_id: &str) {
    if !is_oauth_token(token) || !body.is_object() {
        return;
    }
    let billing = json!({"type": "text", "text": billing_header(body)});
    let system = match body.get("system").cloned() {
        Some(Value::Array(blocks)) => {
            if blocks.first().and_then(|b| b["text"].as_str()).is_some_and(|t| t.starts_with("x-anthropic-billing-header:")) {
                Value::Array(blocks)
            } else {
                let mut out = vec![billing];
                out.extend(blocks);
                Value::Array(out)
            }
        }
        Some(Value::String(text)) => json!([billing, {"type": "text", "text": text}]),
        _ => json!([billing]),
    };
    body["system"] = system;
    if body["metadata"]["user_id"].is_null() {
        body["metadata"]["user_id"] = json!(fake_user_id(session_id, token));
    }
}

/// Anthropic beta flags sent with every OAuth claude request (CLI fingerprint).
pub fn cli_beta_header() -> &'static str {
    CLAUDE_CLI_BETA
}

pub fn selected_beta(model: &str, body: &Value) -> String {
    let mut flags: Vec<&str> = CLAUDE_CLI_BETA.split(',').filter(|flag| {
        !matches!(*flag, "advanced-tool-use-2025-11-20" | "effort-2025-11-24")
            && !(*flag == "redact-thinking-2026-02-12" && body["thinking"]["display"] == "summarized")
    }).collect();
    if model.starts_with("claude-opus") || model.starts_with("claude-sonnet") {
        flags.extend(["advanced-tool-use-2025-11-20", "effort-2025-11-24"]);
    }
    flags.join(",")
}

pub fn cli_user_agent() -> String {
    format!("claude-cli/{CLAUDE_CLI_VERSION} (external, sdk-cli)")
}

/// Stable session uuid for a conversation seed (first message), so one conversation keeps one id.
pub fn session_seed(seed: &str) -> String {
    derive_uuid(&format!("session:{seed}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Synthetic fixture shaped like a real OAuth token. Not a real credential.
    const FAKE_OAT: &str = "sk-ant-oat01-SYNTHETIC-TEST-TOKEN";

    #[test]
    fn oauth_request_gets_billing_block_and_user_id() {
        let mut body = json!({"model": "claude-haiku-5-5", "system": "be brief", "messages": []});
        apply_cloaking(&mut body, FAKE_OAT, "claude:sess-1");
        let sys = body["system"].as_array().unwrap();
        assert!(sys[0]["text"].as_str().unwrap().starts_with("x-anthropic-billing-header: cc_version=2.1.322."));
        assert_eq!(sys[1]["text"], "be brief");
        let uid: Value = serde_json::from_str(body["metadata"]["user_id"].as_str().unwrap()).unwrap();
        assert_eq!(uid["session_id"], "sess-1");
        assert_eq!(uid["device_id"].as_str().unwrap().len(), 64);
    }

    #[test]
    fn billing_block_is_not_injected_twice() {
        let mut body = json!({"model": "m", "messages": []});
        apply_cloaking(&mut body, FAKE_OAT, "s");
        apply_cloaking(&mut body, FAKE_OAT, "s");
        assert_eq!(body["system"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn non_oauth_token_is_untouched() {
        let mut body = json!({"model": "m", "messages": []});
        apply_cloaking(&mut body, "sk-ant-api03-SYNTHETIC", "s");
        assert!(body.get("system").is_none());
        assert!(body.get("metadata").is_none());
    }

    #[test]
    fn beta_header_matches_cli_fingerprint() {
        assert!(cli_beta_header().starts_with("claude-code-20250219,oauth-2025-04-20,"));
        assert_eq!(cli_user_agent(), "claude-cli/2.1.322 (external, sdk-cli)");
        assert_eq!(selected_beta("claude-haiku-5-5", &json!({})), "claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14,context-management-2025-06-27,prompt-caching-scope-2026-01-05,structured-outputs-2025-12-15,fast-mode-2026-02-01,redact-thinking-2026-02-12,token-efficient-tools-2026-03-28");
        assert!(selected_beta("claude-opus-5", &json!({})).ends_with("advanced-tool-use-2025-11-20,effort-2025-11-24"));
        assert!(!selected_beta("claude-sonnet-5", &json!({"thinking":{"display":"summarized"}})).contains("redact-thinking"));
    }
}
