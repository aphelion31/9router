use serde_json::Value;
use std::collections::HashSet;

pub fn detect_required_capabilities(body: &Value) -> Vec<String> {
    let mut required = HashSet::new();
    fn mime(s: &str, out: &mut HashSet<&'static str>) {
        if s.starts_with("image/") { out.insert("vision"); }
        else if s == "application/pdf" { out.insert("pdf"); }
        else if s.starts_with("audio/") { out.insert("audioInput"); }
        else if s.starts_with("video/") { out.insert("videoInput"); }
    }
    fn scan(value: &Value, out: &mut HashSet<&'static str>) {
        if let Some(items) = value.as_array() { for item in items { scan(item, out); } }
        if let Some(obj) = value.as_object() {
            match value["type"].as_str().unwrap_or("") {
                "image_url" | "image" | "input_image" => { out.insert("vision"); },
                "input_audio" | "audio_url" | "audio" => { out.insert("audioInput"); },
                "input_video" | "video_url" | "video" => { out.insert("videoInput"); },
                "file" | "document" | "input_file" => {
                    let data = value["file"]["file_data"].as_str().or_else(|| value["source"]["data"].as_str()).unwrap_or("");
                    let kind = value["source"]["media_type"].as_str().or_else(|| data.strip_prefix("data:").and_then(|s| s.split([';', ',']).next()));
                    if let Some(kind) = kind { mime(kind, out); } else { out.insert("pdf"); }
                }, _ => ()
            }
            if value["images"].as_array().is_some_and(|a| !a.is_empty()) || !value["image_url"].is_null() || !value["image"].is_null() { out.insert("vision"); }
            if !value["audio_url"].is_null() || !value["audio"].is_null() { out.insert("audioInput"); }
            for key in ["mimeType", "contentType", "mediaType"] { if let Some(s) = value[key].as_str() { mime(s, out); } }
            for (key, v) in obj { if matches!(key.as_str(), "content" | "parts" | "inlineData" | "fileData" | "attachments" | "experimental_attachments") { scan(v, out); } }
        }
        if let Some(s) = value.as_str() {
            if s.contains("data:image/") { out.insert("vision"); }
            else if s.contains("data:audio/") { out.insert("audioInput"); }
            else if s.contains("data:application/pdf") { out.insert("pdf"); }
        }
    }
    for items in [&body["messages"], &body["input"], body.get("contents").unwrap_or(&body["request"]["contents"])] {
        if let Some(items) = items.as_array() {
            let start = items.iter().rposition(|m| matches!(m["role"].as_str(), Some("assistant" | "model"))).map_or(0, |i| i + 1);
            for item in &items[start..] { scan(item, &mut required); }
        }
    }
    if body["tools"].as_array().is_some_and(|tools| tools.iter().any(|t| [t["type"].as_str(), t["function"]["name"].as_str().or_else(|| t["name"].as_str())].into_iter().flatten().any(|s| s.contains("web_search") || s.contains("web_fetch")))) { required.insert("search"); }
    ["vision", "pdf", "audioInput", "videoInput", "search"].into_iter().filter(|cap| required.contains(cap)).map(str::to_owned).collect()
}

// combo.js reorderByCapabilities: preserve fallback members and stable tier order.
pub fn reorder_by_capabilities(models: &mut [String], required: &[String], satisfies: impl Fn(&str, &[&str]) -> bool) {
    let hard: Vec<_> = required.iter().map(String::as_str).filter(|c| CAPS.contains(c)).collect();
    let all: Vec<_> = required.iter().map(String::as_str).collect();
    models.sort_by_key(|m| if satisfies(m, &all) { 0 } else if satisfies(m, &hard) { 1 } else { 2 });
}

fn model_metadata(provider: &str, model: &str) -> Option<&'static crate::providers::Model> {
    crate::providers::lookup(provider)?.models.iter().find(|m| m.id == model)
}
pub fn model_context_window(provider: &str, model: &str) -> Option<u64> {
    model_metadata(provider, model)?.metadata.get("contextLength")?.as_u64()
}
pub fn model_satisfies(model: &str, required: &[&str]) -> bool {
    let Some((provider, model)) = model.split_once('/') else { return false; };
    let Some(model) = model_metadata(provider, model) else { return false; };
    let caps = model.metadata.get("capabilities");
    required.iter().all(|cap| caps.is_some_and(|c| c[*cap] == true || c.as_array().is_some_and(|a| a.iter().any(|v| v.as_str() == Some(cap)))))
}

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

const CHARS_PER_TOKEN: f64 = 4.0;
const HEAD_KEEP: usize = 6;
static NULL: Value = Value::Null;

fn content_of(m: &Value) -> &Value {
    match (m.get("content"), m.get("parts")) {
        (Some(c), _) if !c.is_null() => c,
        (_, Some(p)) => p,
        _ => &NULL,
    }
}
fn block_len(content: &Value) -> usize {
    match content {
        Value::String(s) => s.encode_utf16().count(),
        Value::Array(items) => items
            .iter()
            .map(|b| {
                b.get("text")
                    .and_then(Value::as_str)
                    .map_or(50, |t| t.encode_utf16().count())
            })
            .sum(),
        _ => 0,
    }
}

// Trim history to fit a smaller context window by dropping the middle turns.
// Keeps system/developer messages, the first HEAD_KEEP older turns, and the
// trailing user run after the last assistant turn (carries the media).
pub fn strip_history_for_context(body: &Value, context_window: Option<u64>) -> Value {
    let Some(key) = ["messages", "input", "contents"]
        .into_iter()
        .find(|k| body[*k].is_array())
    else {
        return body.clone();
    };
    let arr = body[key].as_array().map(Vec::as_slice).unwrap_or_default();
    if arr.is_empty() {
        return body.clone();
    }
    let is_system = |m: &Value| matches!(m["role"].as_str(), Some("system" | "developer"));
    let is_assistant = |m: &Value| matches!(m["role"].as_str(), Some("assistant" | "model"));
    let system: Vec<&Value> = arr.iter().filter(|m| is_system(m)).collect();
    let rest: Vec<&Value> = arr.iter().filter(|m| !is_system(m)).collect();
    if rest.is_empty() {
        return body.clone();
    }
    let Some(last_assistant) = rest.iter().rposition(|m| is_assistant(m)) else {
        return body.clone();
    };
    let older = &rest[..=last_assistant];
    let tail = &rest[last_assistant + 1..];
    let budget = context_window.filter(|w| *w > 0).unwrap_or(200_000) as f64
        * 0.8
        * CHARS_PER_TOKEN;

    let mut head = older.len().min(HEAD_KEEP);
    let mut total: f64 = system
        .iter()
        .chain(&older[..head])
        .chain(tail)
        .map(|m| block_len(content_of(m)) as f64)
        .sum();
    while total > budget && head > 0 {
        head -= 1;
        total -= block_len(content_of(older[head])) as f64;
    }
    if head == older.len() {
        return body.clone();
    }
    let mut out = body.clone();
    out[key] = Value::Array(
        system
            .into_iter()
            .chain(older[..head].iter().copied())
            .chain(tail.iter().copied())
            .cloned()
            .collect(),
    );
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn capability_tiers_keep_all_fallbacks_stable() {
        // combo.js reorderByCapabilities and detectRequiredCapabilities
        let required = super::detect_required_capabilities(&serde_json::json!({"messages":[{"role":"user","content":[{"type":"image_url"}]}],"tools":[{"type":"web_search"}]}));
        assert_eq!(required, ["vision", "search"]);
        let mut models = vec!["none".into(), "hard".into(), "both".into(), "hard2".into()];
        super::reorder_by_capabilities(&mut models, &required, |m, caps| caps.iter().all(|c| match *c { "vision" => m != "none", "search" => m == "both", _ => false }));
        assert_eq!(models, ["both", "hard", "hard2", "none"]);
    }
    use super::*;
    use serde_json::json;

    #[test]
    fn strip_history_drops_middle_turns_over_budget() {
        let small = json!({"messages":[{"role":"user","content":"hi"}]});
        assert_eq!(strip_history_for_context(&small, Some(1000)), small);

        let mut messages = vec![json!({"role":"system","content":"sys"})];
        for i in 0..8 {
            let role = if i % 2 == 0 { "user" } else { "assistant" };
            messages.push(json!({"role": role, "content": "x".repeat(1000)}));
        }
        messages.push(json!({"role":"user","content":"now"}));
        let body = json!({"messages": messages});
        // budget = 1000 * 0.8 * 4 = 3200 chars; sys + head + tail = 6006 → trims to 3 head turns.
        let out = strip_history_for_context(&body, Some(1000));
        let out_msgs = out["messages"].as_array().unwrap();
        assert_eq!(out_msgs.len(), 5);
        assert_eq!(out_msgs[0]["role"], "system");
        assert_eq!(out_msgs[4]["content"], "now");
    }

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
