use serde_json::{Value, json};

pub const KIMCHI_API: &str = "https://llm.kimchi.dev";
pub const KIMCHI_USER_AGENT: &str = "kimchi/0.1.40";

pub fn build_kimchi_models_url(endpoint: Option<&str>) -> String {
    let endpoint = endpoint
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(KIMCHI_API);
    format!(
        "{}/v1/models/metadata?include_in_cli=true",
        endpoint.trim_end_matches('/')
    )
}

pub fn normalize_kimchi_model(item: &Value) -> Option<Value> {
    let mut model = item.as_object()?.clone();
    let id = ["slug", "id", "model", "name"]
        .iter()
        .find_map(|k| item[*k].as_str().filter(|s| !s.is_empty()))?
        .trim();
    if id.is_empty() {
        return None;
    }
    let name = ["display_name", "displayName", "name"]
        .iter()
        .find_map(|k| item[*k].as_str().filter(|s| !s.is_empty()))
        .unwrap_or(id)
        .trim();
    let modalities: Vec<Value> = item["input_modalities"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|v| matches!(v.as_str(), Some("text" | "image")))
        .cloned()
        .collect();
    let vision = modalities.iter().any(|v| v == "image");
    let reasoning = item["reasoning"] == true;
    let provider = item["provider"].as_str().unwrap_or("");
    let kind = if vision { "imageToText" } else { "llm" };
    let number = |values: &[&Value]| {
        values.iter().find_map(|v| {
            v.as_f64()
                .or_else(|| v.as_str().and_then(|s| s.parse::<f64>().ok()))
                .filter(|n| *n != 0.0 && n.is_finite())
        })
    };
    let context = number(&[
        &item["limits"]["context_window"],
        &item["contextLength"],
        &item["context_length"],
    ]);
    let max = number(&[
        &item["limits"]["max_output_tokens"],
        &item["maxOutputTokens"],
        &item["max_output_tokens"],
    ]);
    let mut capabilities = json!({"vision": vision, "reasoning": reasoning});
    if let Some(n) = context {
        capabilities["contextWindow"] = json!(n);
        model.insert("contextLength".into(), json!(n));
    }
    if let Some(n) = max {
        capabilities["maxOutput"] = json!(n);
        model.insert("maxOutputTokens".into(), json!(n));
    }
    if !provider.is_empty() {
        capabilities["upstreamProvider"] = json!(provider);
    }
    model.insert("id".into(), json!(id));
    model.insert("name".into(), json!(name));
    model.insert("provider".into(), json!(provider));
    model.insert("upstreamProvider".into(), json!(provider));
    model.insert("reasoning".into(), json!(reasoning));
    model.insert("inputModalities".into(), json!(modalities));
    model.insert("kind".into(), json!(kind));
    model.insert("type".into(), json!(kind));
    model.insert("capabilities".into(), capabilities);
    if provider == "anthropic" {
        model.insert(
            "compat".into(),
            json!({"supportsReasoningEffort":false,"cacheControlFormat":"anthropic"}),
        );
    }
    Some(Value::Object(model))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn normalizes_model_and_url() {
        assert_eq!(
            build_kimchi_models_url(Some(" https://example.com/// ")),
            "https://example.com/v1/models/metadata?include_in_cli=true"
        );
        let m = normalize_kimchi_model(&json!({"slug":" model ","provider":"anthropic","input_modalities":["text","image","audio"],"limits":{"context_window":100},"reasoning":true})).unwrap();
        assert_eq!(m["id"], "model");
        assert_eq!(m["kind"], "imageToText");
        assert_eq!(m["capabilities"]["vision"], true);
        assert_eq!(m["compat"]["cacheControlFormat"], "anthropic");
        assert_eq!(m["inputModalities"].as_array().unwrap().len(), 2);
        assert!(normalize_kimchi_model(&json!({"slug":" "})).is_none());
    }
}
