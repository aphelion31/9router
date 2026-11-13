use serde_json::{Value, json};

pub(super) fn is_responses(provider: &str, model: &str) -> bool {
    crate::providers::lookup(provider)
        .and_then(|p| p.models.iter().find(|m| m.id == model))
        .and_then(|m| m.metadata.get("targetFormat"))
        .and_then(Value::as_str)
        == Some("openai-responses")
        || model.starts_with("muse-spark")
}

pub(super) fn prepare(body: &mut Value, model: &str) {
    if body.get("input").is_none() {
        *body = crate::translator::openai_responses::openai_to_openai_responses_request(
            model, body, true, None,
        );
    }
    if let Some(o) = body.as_object_mut() {
        if o.get("input")
            .and_then(Value::as_array)
            .is_none_or(|a| a.is_empty())
        {
            o.insert("input".into(), json!([{"type":"message", "role":"user", "content":[{"type":"input_text", "text":"..."}]}]));
        }
        if !o.contains_key("max_output_tokens") {
            if let Some(v) = o
                .get("max_completion_tokens")
                .or_else(|| o.get("max_tokens"))
                .cloned()
            {
                o.insert("max_output_tokens".into(), v);
            }
        }
        o.remove("max_tokens");
        o.remove("max_completion_tokens");
        if let Some(effort) = o.remove("reasoning_effort") {
            if !o.contains_key("reasoning") {
                o.insert(
                    "reasoning".into(),
                    json!({"effort":effort,"summary":"auto"}),
                );
            }
        }
        o.insert("stream".into(), true.into());
        o.insert("store".into(), false.into());
        if let Some(items) = o.get_mut("input").and_then(Value::as_array_mut) {
            items.retain(|i| i.get("type").and_then(Value::as_str) != Some("reasoning"));
            for item in items {
                if let Some(map) = item.as_object_mut() {
                    map.remove("encrypted_content");
                    map.remove("reasoning_encrypted_content");
                }
            }
        }
        if let Some(tools) = o.get_mut("tools").and_then(Value::as_array_mut) {
            for tool in tools {
                if let Some(t) = tool.as_object_mut()
                    && !t.contains_key("parameters")
                    && t.get("type").and_then(Value::as_str) == Some("function")
                {
                    t.insert(
                        "parameters".into(),
                        json!({"type":"object","properties":{}}),
                    );
                }
            }
        }
    }
}

pub(super) fn url(endpoint: &str) -> anyhow::Result<reqwest::Url> {
    let mut url = reqwest::Url::parse(endpoint)?;
    let path = url.path().replace("/chat/completions", "/responses");
    url.set_path(&path);
    Ok(url)
}
