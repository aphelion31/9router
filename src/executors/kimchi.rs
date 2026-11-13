use serde_json::{Value, json};

pub(super) fn prepare(model: &str, body: &mut Value) {
    let Some(obj) = body.as_object_mut() else {
        return;
    };
    let system = obj
        .remove("system")
        .map(|v| match v {
            Value::String(s) => s,
            Value::Array(parts) => parts
                .iter()
                .filter_map(|p| p.as_str().or_else(|| p.get("text").and_then(Value::as_str)))
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        })
        .unwrap_or_default();
    if let Some(messages) = obj.get_mut("messages").and_then(Value::as_array_mut) {
        if !system.trim().is_empty() {
            if let Some(existing) = messages
                .iter_mut()
                .find(|m| m.get("role").and_then(Value::as_str) == Some("system"))
            {
                if let Some(content) = existing.get_mut("content") {
                    if let Some(text) = content.as_str() {
                        *content = json!(format!("{}\n\n{}", system.trim(), text));
                    } else if let Some(parts) = content.as_array_mut() {
                        parts.insert(0, json!({"type":"text", "text": system.trim()}));
                    }
                }
            } else {
                messages.insert(0, json!({"role":"system", "content":system.trim()}));
            }
        }
        for message in messages {
            if let Some(m) = message.as_object_mut() {
                m.remove("cache_control");
                if m.get("role").and_then(Value::as_str) == Some("assistant")
                    && m.get("reasoning_content")
                        .and_then(Value::as_str)
                        .is_some_and(|s| s.len() > 8)
                {
                    m.remove("reasoning_content");
                }
                if let Some(parts) = m.get_mut("content").and_then(Value::as_array_mut) {
                    for part in parts {
                        if let Some(p) = part.as_object_mut() {
                            p.remove("cache_control");
                            p.remove("signature");
                        }
                    }
                }
            }
        }
    }
    if let Some(tools) = obj.get_mut("tools").and_then(Value::as_array_mut) {
        for tool in tools {
            if let Some(t) = tool.as_object_mut() {
                t.remove("cache_control");
            }
        }
    }
    for key in [
        "anthropic_version",
        "anthropic_beta",
        "client_metadata",
        "mcp_servers",
        "stop_sequences",
        "thinking",
        "top_k",
    ] {
        obj.remove(key);
    }
    if model
        .split(['-', '_', '/'])
        .any(|s| s.eq_ignore_ascii_case("claude") || s.eq_ignore_ascii_case("anthropic"))
    {
        obj.remove("reasoning_effort");
        obj.remove("reasoning");
    } else if obj.get("reasoning_effort").and_then(Value::as_str) == Some("auto") {
        obj.insert("reasoning_effort".into(), json!("medium"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kimchi_prepare_hoists_system_and_normalizes_reasoning() {
        let mut body = json!({
            "system": "Be terse",
            "messages": [{"role": "user", "content": "hi"}],
            "reasoning_effort": "auto",
            "thinking": {"type": "enabled"}
        });
        prepare("deepseek-r1", &mut body);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], "Be terse");
        assert_eq!(body["reasoning_effort"], "medium");
        assert!(body.get("thinking").is_none());
    }
}
