use anyhow::Context;
use serde_json::{Value, json};

pub(super) fn prepare(model: &str, body: &mut Value) -> anyhow::Result<()> {
    let object = body
        .as_object_mut()
        .context("GitHub request body must be an object")?;
    let model = model.to_ascii_lowercase();
    if model.contains("gpt-5") || ["o1-", "o3-", "o4-"].iter().any(|s| model.contains(s)) {
        if let Some(value) = object.remove("max_tokens") {
            object.insert("max_completion_tokens".into(), value);
        }
    }
    if object.get("reasoning_effort").and_then(Value::as_str) == Some("none") {
        object.remove("reasoning_effort");
    }
    if let Some(messages) = object.get_mut("messages").and_then(Value::as_array_mut) {
        for message in messages {
            if let Some(parts) = message.get_mut("content").and_then(Value::as_array_mut) {
                for part in parts.iter_mut() {
                    if !matches!(
                        part.get("type").and_then(Value::as_str),
                        Some("text" | "image_url")
                    ) {
                        let text = part
                            .get("text")
                            .or_else(|| part.get("content"))
                            .cloned()
                            .unwrap_or_else(|| part.clone());
                        let text = text
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| text.to_string());
                        *part = json!({"type":"text", "text":text});
                    }
                }
                parts.retain(|part| part.get("text").and_then(Value::as_str) != Some(""));
                if parts.is_empty() {
                    message["content"] = Value::Null;
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn supports_responses_endpoint(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    !(m.contains("gemini") || m.contains("claude"))
}

pub(crate) fn responses_url(endpoint: &str) -> anyhow::Result<reqwest::Url> {
    let trimmed = endpoint.trim_end_matches('/');
    let base = trimmed
        .strip_suffix("/chat/completions")
        .or_else(|| trimmed.strip_suffix("/v1/messages"))
        .unwrap_or(trimmed);
    Ok(reqwest::Url::parse(&format!("{base}/responses"))?)
}

pub(crate) fn prepare_responses_fallback(model: &str, body: &Value) -> Value {
    let mut out = json!({
        "model": model,
        "stream": body.get("stream").and_then(Value::as_bool).unwrap_or(true),
        "input": body.get("messages").cloned().unwrap_or_else(|| json!([])),
    });
    if let Some(obj) = out.as_object_mut() {
        if let Some(tools) = body.get("tools") {
            obj.insert("tools".into(), tools.clone());
        }
        if let Some(tc) = body.get("tool_choice") {
            obj.insert("tool_choice".into(), tc.clone());
        }
        if let Some(max_tok) = body
            .get("max_completion_tokens")
            .or_else(|| body.get("max_tokens"))
        {
            obj.insert("max_output_tokens".into(), max_tok.clone());
        }
        if let Some(effort) = body.get("reasoning_effort").and_then(Value::as_str)
            && effort != "none"
        {
            obj.insert("reasoning".into(), json!({ "effort": effort }));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_responses_helpers_work() {
        assert!(supports_responses_endpoint("gpt-5-codex"));
        assert!(!supports_responses_endpoint("claude-sonnet-4"));
        assert!(!supports_responses_endpoint("gemini-2.5-pro"));
        assert_eq!(
            responses_url("https://api.githubcopilot.com/chat/completions")
                .unwrap()
                .as_str(),
            "https://api.githubcopilot.com/responses"
        );
        let fallback = prepare_responses_fallback(
            "gpt-5-codex",
            &json!({"messages":[{"role":"user","content":"hi"}],"max_tokens":100,"reasoning_effort":"high"}),
        );
        assert_eq!(fallback["max_output_tokens"], 100);
        assert_eq!(fallback["reasoning"]["effort"], "high");
    }
}
