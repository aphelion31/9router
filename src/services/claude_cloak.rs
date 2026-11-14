//! Claude Code cloaking for OAuth (`sk-ant-oat`) tokens. Port of upstream
//! `open-sse/utils/claudeCloaking.js` (`applyCloaking`) and the claude CLI
//! fingerprint from `open-sse/providers/shared.js` (`CLAUDE_CLI_SPOOF_HEADERS`).
//! Without it Anthropic answers OAuth claude calls with a 429 `{"message":"Error"}`.
use ring::digest::{SHA256, digest};
use serde_json::{Value, json};

pub const CLAUDE_CLI_VERSION: &str = "2.1.280";
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
        assert!(sys[0]["text"].as_str().unwrap().starts_with("x-anthropic-billing-header: cc_version=2.1.280."));
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
        assert_eq!(cli_user_agent(), "claude-cli/2.1.280 (external, sdk-cli)");
    }
}
