use serde_json::Value;
use std::collections::HashMap;

pub fn detect_client_tool(headers: &HashMap<String, String>, body: &Value) -> Option<&'static str> {
    let get = |key: &str| {
        headers
            .get(key)
            .map(|s| s.to_lowercase())
            .unwrap_or_default()
    };
    let ua = get("user-agent");
    let x_app = get("x-app");
    let intent = get("openai-intent");
    let initiator = headers
        .get("x-initiator")
        .or_else(|| headers.get("X-Initiator"))
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    let originator = get("originator");
    if body["userAgent"] == "antigravity" {
        return Some("antigravity");
    }
    if ua.contains("githubcopilotchat") || intent == "conversation-panel" || initiator == "user" {
        return Some("github-copilot");
    }
    if ua.contains("claude-cli") || ua.contains("claude-code") || x_app == "cli" {
        return Some("claude");
    }
    if ua.contains("gemini-cli") {
        return Some("gemini-cli");
    }
    if ["codex-tui", "codex-cli", "codex_cli_rs", "codex desktop"]
        .iter()
        .any(|s| ua.contains(s))
        || originator.starts_with("codex_")
    {
        return Some("codex");
    }
    if ua.contains("deepseek-tui") {
        return Some("deepseek-tui");
    }
    None
}

pub fn is_native_passthrough(client: &str, provider: &str) -> bool {
    let provider = if provider.starts_with("anthropic-compatible") {
        "anthropic"
    } else {
        provider
    };
    matches!(
        (client, provider),
        ("claude", "claude" | "anthropic")
            | ("gemini-cli", "gemini-cli")
            | ("antigravity", "antigravity")
            | ("codex", "codex")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn detects_clients_in_priority_order() {
        let headers = HashMap::from([
            ("user-agent".into(), "Claude-CLI".into()),
            ("x-initiator".into(), "user".into()),
        ]);
        assert_eq!(
            detect_client_tool(&headers, &json!({})),
            Some("github-copilot")
        );
        assert_eq!(
            detect_client_tool(&headers, &json!({"userAgent":"antigravity"})),
            Some("antigravity")
        );
        assert!(is_native_passthrough(
            "claude",
            "anthropic-compatible-custom"
        ));
        assert!(!is_native_passthrough("github-copilot", "github"));
    }
}
