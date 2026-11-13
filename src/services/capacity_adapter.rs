use serde_json::Value;
use std::collections::HashSet;

pub const DEFAULT_FALLBACK_MODEL: &str = "oc/mimo-v2.6-flash-free";
const CAPS: [&str; 4] = ["vision", "pdf", "audioInput", "videoInput"];
#[derive(Debug, PartialEq, Eq)]
pub struct CapacityConfig {
    pub enabled: bool,
    pub round_robin: bool,
    pub models: Vec<String>,
}

pub fn get_capacity_adapter_config(cap: &str, settings: &Value) -> CapacityConfig {
    let entry = &settings["capacityAdapter"][cap];
    let enabled = entry.is_array() || (entry.is_object() && entry["enabled"] != false);
    let list = entry.as_array().or_else(|| entry["models"].as_array());
    let mut models: Vec<String> = list
        .into_iter()
        .flatten()
        .filter_map(|item| {
            item["model"]
                .as_str()
                .filter(|s| !s.is_empty())
                .or_else(|| item.as_str())
                .filter(|s| !s.is_empty())
        })
        .map(|s| {
            if s == "oc/mimo-v2.5-free" {
                DEFAULT_FALLBACK_MODEL
            } else {
                s
            }
            .to_owned()
        })
        .collect();
    if enabled && models.is_empty() {
        models.push(DEFAULT_FALLBACK_MODEL.into());
    }
    CapacityConfig {
        enabled,
        round_robin: entry.is_object() && entry["roundRobin"] == true,
        models,
    }
}
pub fn get_capacity_adapter_models(settings: &Value) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for cap in CAPS {
        let config = get_capacity_adapter_config(cap, settings);
        if config.enabled {
            for model in config.models {
                if seen.insert(model.clone()) {
                    out.push(model);
                }
            }
        }
    }
    out
}
pub fn get_capacity_adapter_strategy(cap: &str, settings: &Value) -> &'static str {
    let config = get_capacity_adapter_config(cap, settings);
    if config.enabled && config.round_robin {
        "round-robin"
    } else {
        "fallback"
    }
}
pub fn get_active_adapter_strategy(required: &[String], settings: &Value) -> &'static str {
    for cap in required {
        if CAPS.contains(&cap.as_str()) && get_capacity_adapter_config(cap, settings).enabled {
            return get_capacity_adapter_strategy(cap, settings);
        }
    }
    "fallback"
}
pub fn augment_models_with_capacity_adapter(
    models: &[String],
    required: &[String],
    settings: &Value,
    satisfies: impl Fn(&str, &[&str]) -> bool,
) -> Vec<String> {
    let hard: Vec<_> = required
        .iter()
        .map(String::as_str)
        .filter(|s| CAPS.contains(s))
        .collect();
    if hard.is_empty() || models.is_empty() || models.iter().any(|m| satisfies(m, &hard)) {
        return models.to_vec();
    }
    let mut result: Vec<_> = get_capacity_adapter_models(settings)
        .into_iter()
        .filter(|m| !models.contains(m) && satisfies(m, &hard))
        .collect();
    result.extend_from_slice(models);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn adapter_prioritizes_capable_fallback_without_overriding_capable_original() {
        let settings = json!({"capacityAdapter":{"vision":{"enabled":true,"roundRobin":true,"models":["vendor/vision"]},"pdf":[]}});
        assert_eq!(
            get_capacity_adapter_strategy("vision", &settings),
            "round-robin"
        );
        assert_eq!(
            get_capacity_adapter_config("pdf", &settings).models,
            [DEFAULT_FALLBACK_MODEL]
        );
        let original = vec!["vendor/text".into()];
        assert_eq!(
            augment_models_with_capacity_adapter(
                &original,
                &["vision".into()],
                &settings,
                |model, _| model.contains("vision")
            ),
            ["vendor/vision", "vendor/text"]
        );
    }
}
