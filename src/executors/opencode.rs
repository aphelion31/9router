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

pub(super) fn conversation_session(body: &Value) -> String {
    if let Some(id) = body
        .get("session_id")
        .or_else(|| body.get("thread_id"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return id.to_string();
    }
    let first_user = body
        .get("messages")
        .and_then(Value::as_array)
        .or_else(|| body.get("input").and_then(Value::as_array))
        .and_then(|arr| {
            arr.iter()
                .find(|m| m.get("role").and_then(Value::as_str) == Some("user"))
        });
    if let Some(msg) = first_user {
        let digest = ring::digest::digest(
            &ring::digest::SHA256,
            msg.to_string().as_bytes(),
        );
        let hex: String = digest
            .as_ref()
            .iter()
            .take(12)
            .map(|b| format!("{b:02x}"))
            .collect();
        return format!("ses_{hex}");
    }
    session().to_string()
}

pub(super) fn prepare(body: &mut Value, model: &str) {
    if let Some(obj) = body.as_object_mut() {
        obj.entry("model").or_insert_with(|| model.into());
        obj.insert("stream".into(), true.into());
        if obj.get("reasoning_effort").and_then(Value::as_str) == Some("auto") {
            obj.remove("reasoning_effort");
        }
    }
    let responses = model.starts_with("muse-spark");
    super::opencode_zen::prepare(body, responses);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn conversation_session_is_deterministic_per_first_user_message() {
        let b1 = json!({"messages":[{"role":"user","content":"hello"}]});
        let b2 = json!({"messages":[{"role":"user","content":"hello"},{"role":"assistant","content":"hi"}]});
        assert_eq!(conversation_session(&b1), conversation_session(&b2));
        assert!(conversation_session(&b1).starts_with("ses_"));
    }
}
