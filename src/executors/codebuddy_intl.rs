use anyhow::Context;
use serde_json::{Value, json};

pub(super) fn prepare(body: &mut Value) -> anyhow::Result<()> {
    let object = body
        .as_object_mut()
        .context("CodeBuddy Intl request body must be an object")?;
    object.insert("stream".into(), Value::Bool(true));

    if let Some(eff) = object.get("reasoning_effort").and_then(Value::as_str) {
        if matches!(eff, "none" | "off") {
            object.remove("reasoning_effort");
        } else if !eff.is_empty() {
            object.insert("reasoning_summary".into(), json!("auto"));
        }
    }

    let source = object
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut messages = vec![json!({"role": "system", "content": "You are CodeBuddy Code."})];
    for mut message in source {
        let Some(msg_obj) = message.as_object_mut() else {
            continue;
        };
        let role = msg_obj
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if matches!(role, "system" | "developer") {
            continue;
        }
        if role == "user" {
            if let Some(text) = msg_obj.get("content").and_then(Value::as_str) {
                let text = text.to_string();
                msg_obj.insert("content".into(), json!([{"type": "text", "text": text}]));
            }
        }
        messages.push(message);
    }
    object.insert("messages".into(), Value::Array(messages));
    Ok(())
}
