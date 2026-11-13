use serde_json::{Value, json};
use std::collections::HashSet;

pub fn is_qoder_pat(token: &str) -> bool {
    token.starts_with("pt-")
}

pub fn routable_qoder_models(models: &[Value], raw_configs: &[(String, Value)]) -> Vec<Value> {
    let mut seen = HashSet::new();
    let keys: HashSet<&str> = models
        .iter()
        .filter_map(|m| m["id"].as_str())
        .chain(raw_configs.iter().map(|(k, _)| k.as_str()))
        .collect();
    let mut out = Vec::new();
    for (id, name) in [("smodel", "Sonus"), ("cmodel", "Cantus")] {
        if !keys.contains(id) {
            seen.insert(id.to_owned());
            out.push(json!({"id":id,"name":name,"hidden":false}));
        }
    }
    for model in models {
        if let Some(id) = model["id"].as_str().filter(|s| !s.is_empty()) {
            if seen.insert(id.to_owned()) {
                out.push(json!({"id":id,"name":model["name"].as_str().filter(|s| !s.is_empty()).unwrap_or(id),"hidden":false}));
            }
        }
    }
    for (id, cfg) in raw_configs {
        if !id.is_empty() && seen.insert(id.clone()) {
            out.push(json!({"id":id,"name":cfg["display_name"].as_str().filter(|s| !s.is_empty()).unwrap_or(id),"hidden":true}));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_static_and_hidden_models() {
        let out = routable_qoder_models(
            &[json!({"id":"smodel","name":"Live"})],
            &[("hidden".into(), json!({"display_name":"Hidden"}))],
        );
        assert_eq!(
            out,
            vec![
                json!({"id":"cmodel","name":"Cantus","hidden":false}),
                json!({"id":"smodel","name":"Live","hidden":false}),
                json!({"id":"hidden","name":"Hidden","hidden":true})
            ]
        );
        assert!(is_qoder_pat("pt-token"));
        assert!(!is_qoder_pat("jt-token"));
    }
}
