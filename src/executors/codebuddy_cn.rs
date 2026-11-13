use anyhow::Context;
use serde_json::{Value, json};

const NEUTRAL_PROMPT: &str =
    "You are a helpful AI assistant that helps with software engineering tasks.";

fn flatten_content(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn has_ordered(haystack: &str, parts: &[&str]) -> bool {
    let mut rest = haystack;
    for (idx, part) in parts.iter().enumerate() {
        let Some(pos) = rest.find(part) else {
            return false;
        };
        if idx > 0 && pos == 0 {
            return false;
        }
        rest = &rest[pos + part.len()..];
    }
    true
}

fn matches_agent_pattern(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let direct = [
        "you are claude code",
        "you are cursor",
        "you are windsurf",
        "you are cline",
        "you are aider",
        "you are continue",
        "you are copilot",
        "you are cody",
        "you are a coding agent",
        "you are an ai agent",
        "you are an ai coding agent",
        "you are an ai code agent",
        "you are a code agent",
        "you are a agent",
        "you are an agent",
        "orchestration capabilities",
        "ohmyopencode",
        "<agent-identity>",
        "<role>",
        "<behavior_instructions>",
    ];
    if direct.iter().any(|p| lower.contains(p)) {
        return true;
    }
    if has_ordered(&lower, &["anthropic", "official", "cli"])
        || has_ordered(&lower, &["anxthxropic", "official", "cli"])
        || has_ordered(&lower, &["claude", "code", "official", "cli"])
        || has_ordered(&lower, &["claude", "code", "issues"])
        || has_ordered(&lower, &["give feedback", "claude", "code"])
    {
        return true;
    }
    if let Some(pos) = lower.find("cc_entrypoint") {
        let after = lower[pos + "cc_entrypoint".len()..].trim_start();
        if let Some(after_eq) = after.strip_prefix('=') {
            let val = after_eq.trim_start();
            if ["cli", "vscode", "jetbrains", "gui"]
                .iter()
                .any(|k| val.starts_with(k))
            {
                return true;
            }
        }
    }
    let mut search = lower.as_str();
    while let Some(pos) = search.find("you are ") {
        let after = &search[pos + "you are ".len()..];
        let window: String = after.chars().take(48).collect();
        if let Some(idx) = window.find("ai agent") {
            if idx <= 30 || window[..idx].ends_with("powerful ") {
                return true;
            }
        }
        search = &search[pos + 8..];
    }
    false
}

pub(super) fn prepare(body: &mut Value) -> anyhow::Result<()> {
    let object = body
        .as_object_mut()
        .context("CodeBuddy CN request body must be an object")?;
    object.insert("stream".into(), Value::Bool(true));

    if let Some(messages) = object.get_mut("messages").and_then(Value::as_array_mut) {
        for message in messages.iter_mut() {
            let Some(msg_obj) = message.as_object_mut() else {
                continue;
            };
            if msg_obj.get("role").and_then(Value::as_str) != Some("system") {
                continue;
            }
            let Some(content) = msg_obj.get("content") else {
                continue;
            };
            let text = flatten_content(content);
            if text.is_empty() {
                continue;
            }
            if text.len() > 2000 || matches_agent_pattern(&text) {
                let replacement = if content.is_string() {
                    json!(NEUTRAL_PROMPT)
                } else {
                    json!([{"type": "text", "text": NEUTRAL_PROMPT}])
                };
                msg_obj.insert("content".into(), replacement);
            }
        }
    }

    if let Some(eff) = object.get("reasoning_effort").and_then(Value::as_str) {
        if matches!(eff, "none" | "off") {
            object.remove("reasoning_effort");
        } else if !eff.is_empty() {
            object.insert("reasoning_summary".into(), json!("auto"));
        }
    }
    Ok(())
}

pub(crate) fn parse_code_6004_reset_ms(body: &Value) -> Option<u64> {
    let code = body
        .pointer("/error/code")
        .or_else(|| body.get("code"))
        .and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()));
    if code != Some(6004) {
        return None;
    }
    if let Some(ms) = body
        .pointer("/error/retryAfterMs")
        .or_else(|| body.get("retryAfterMs"))
        .and_then(Value::as_u64)
    {
        return Some(ms);
    }
    if let Some(sec) = body
        .pointer("/error/retryAfter")
        .or_else(|| body.get("retryAfter"))
        .and_then(Value::as_u64)
    {
        return Some(sec * 1000);
    }
    Some(60_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codebuddy_cn_6004_reset_parsing_works() {
        assert_eq!(
            parse_code_6004_reset_ms(&json!({"error": {"code": 6004, "retryAfter": 45}})),
            Some(45_000)
        );
        assert_eq!(parse_code_6004_reset_ms(&json!({"error": {"code": 500}})), None);
    }
}
