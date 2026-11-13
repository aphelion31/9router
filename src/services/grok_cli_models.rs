use serde_json::{Map, Value};
use std::collections::HashSet;

pub fn parse_grok_cli_models(data: &Value) -> Vec<Value> {
    let entries = if data.is_array() {
        data
    } else {
        data.get("data")
            .or_else(|| data.get("models"))
            .or_else(|| data.get("results"))
            .unwrap_or(&Value::Null)
    };
    let pairs: Vec<(Option<&str>, &Value)> = match entries {
        Value::Array(items) => items.iter().map(|item| (None, item)).collect(),
        Value::Object(items) => items
            .iter()
            .map(|(key, item)| (Some(key.as_str()), item))
            .collect(),
        _ => Vec::new(),
    };
    let mut seen = HashSet::new();
    let mut models = Vec::new();
    for (key, raw) in pairs {
        let mut item = match raw {
            Value::String(id) => Map::from_iter([("id".into(), Value::String(id.clone()))]),
            Value::Object(item) => item.clone(),
            _ => continue,
        };
        let id = ["id", "model_id", "modelId", "model", "slug"]
            .iter()
            .find_map(|k| item.get(*k).filter(|v| !v.is_null()))
            .and_then(Value::as_str)
            .or(key)
            .or_else(|| item.get("name").and_then(Value::as_str))
            .unwrap_or("")
            .trim()
            .to_owned();
        if id.is_empty() || !seen.insert(id.clone()) {
            continue;
        }
        let name = ["display_name", "displayName", "name"]
            .iter()
            .find_map(|k| item.get(*k).filter(|v| !v.is_null()))
            .cloned()
            .unwrap_or_else(|| id.clone().into());
        for (target, keys) in [
            (
                "contextLength",
                [
                    "context_length",
                    "contextLength",
                    "context_window",
                    "contextWindow",
                ],
            ),
            (
                "maxOutputTokens",
                ["max_output_tokens", "maxOutputTokens", "", ""],
            ),
        ] {
            let number = keys
                .iter()
                .find_map(|key| item.get(*key).filter(|v| !v.is_null()))
                .and_then(|v| {
                    v.as_f64()
                        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                })
                .filter(|v: &f64| v.is_finite() && *v > 0.0);
            if let Some(number) = number {
                if let Some(n) = serde_json::Number::from_f64(number) {
                    item.insert(target.into(), n.into());
                }
            }
        }
        if id == "grok-build" {
            if !item
                .get("contextLength")
                .and_then(Value::as_f64)
                .is_some_and(|n| n > 0.0)
            {
                item.insert("contextLength".into(), 500000.into());
            }
            if !item
                .get("maxOutputTokens")
                .and_then(Value::as_f64)
                .is_some_and(|n| n > 0.0)
            {
                item.insert("maxOutputTokens".into(), 64000.into());
            }
        }
        item.insert("id".into(), id.into());
        item.insert("name".into(), name);
        models.push(item.into());
    }
    models
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn normalizes_official_metadata_and_deduplicates() {
        let models = parse_grok_cli_models(
            &json!({"models":[{"model_id":"grok-build","display_name":"Grok Build","context_window":500000,"max_output_tokens":64000,"supported_in_api":false},{"id":"grok-build"}]}),
        );
        assert_eq!(models.len(), 1);
        assert_eq!(models[0]["id"], "grok-build");
        assert_eq!(models[0]["name"], "Grok Build");
        assert_eq!(models[0]["contextLength"].as_f64(), Some(500000.0));
        assert_eq!(models[0]["maxOutputTokens"].as_f64(), Some(64000.0));
        assert_eq!(models[0]["supported_in_api"], false);
        assert_eq!(
            parse_grok_cli_models(&json!(["grok-build"]))[0]["contextLength"],
            500000
        );
    }
}
