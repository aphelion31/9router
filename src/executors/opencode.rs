use anyhow::Context;
use serde_json::Value;
use std::sync::OnceLock;

pub(super) fn session() -> &'static str {
    static SESSION: OnceLock<String> = OnceLock::new();
    SESSION.get_or_init(|| {
        use ring::rand::{SecureRandom, SystemRandom};
        let now = crate::routing::classify::now_ms();
        let mut bytes = [0u8; 14];
        SystemRandom::new()
            .fill(&mut bytes)
            .expect("system random unavailable");
        let chars = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
        let suffix: String = bytes
            .iter()
            .map(|v| chars[*v as usize % chars.len()] as char)
            .collect();
        format!(
            "ses_{:012x}{suffix}",
            !(now.wrapping_mul(4096).wrapping_add(1)) & ((1u64 << 48) - 1)
        )
    })
}

pub(super) fn url(endpoint: &str, model: &str) -> anyhow::Result<reqwest::Url> {
    let mut url = reqwest::Url::parse(endpoint).context("invalid OpenCode endpoint")?;
    let path = if model.starts_with("muse-spark") {
        "/inference/v1/responses"
    } else if model == "union-alpha" {
        "/inference/anthropic/v1/messages"
    } else {
        "/inference/openai/v1/chat/completions"
    };
    url.set_path(path);
    Ok(url)
}

pub(super) fn prepare(body: &mut Value, model: &str) {
    if let Some(obj) = body.as_object_mut() {
        obj.entry("model").or_insert_with(|| model.into());
        obj.insert("stream".into(), true.into());
        if obj.get("reasoning_effort").and_then(Value::as_str) == Some("auto") {
            obj.remove("reasoning_effort");
        }
    }
}
