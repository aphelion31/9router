use anyhow::{Context, bail};
use serde_json::{Value, json};

pub(super) fn prepare(model: &str, body: &mut Value) -> anyhow::Result<()> {
    if model.to_ascii_lowercase().contains("claude") {
        bail!(
            "GitHub Copilot Claude requires messages endpoint translation, which is not implemented"
        );
    }
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
