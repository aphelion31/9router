use serde_json::{Value, json};
use std::collections::HashSet;

pub fn expand_copilot_catalog(raw: &[Value]) -> Vec<Value> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for model in raw {
        if model["capabilities"]["type"] != "chat"
            || (model.get("policy").is_some_and(|p| !p.is_null())
                && model["policy"]["state"] != "enabled")
        {
            continue;
        }
        let Some(id) = model["id"].as_str().filter(|s| !s.is_empty()) else {
            continue;
        };
        if seen.insert(id) {
            out.push(json!({"id":id,"name":model["name"].as_str().filter(|s| !s.is_empty()).unwrap_or(id)}));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selects_enabled_chat_models_and_deduplicates() {
        let models = expand_copilot_catalog(&[
            json!({"id":"ok","capabilities":{"type":"chat"}}),
            json!({"id":"ok","capabilities":{"type":"chat"}}),
            json!({"id":"disabled","capabilities":{"type":"chat"},"policy":{"state":"disabled"}}),
            json!({"id":"embed","capabilities":{"type":"embeddings"}}),
        ]);
        assert_eq!(models, vec![json!({"id":"ok","name":"ok"})]);
    }
}
