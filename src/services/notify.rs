use ring::hmac;
use serde_json::{Value, json};

pub const NOTIFY_TIMEOUT_MS: u64 = 10000;

pub fn default_notify_config() -> Value {
    json!({"telegram":{"enabled":false,"botToken":"","chatId":""},"webhook":{"enabled":false,"url":"","secret":""},"webhookEvents":{"usage":false,"quota":false,"error":false,"maskCredentials":true}})
}

pub fn normalize_webhook_events(raw: &Value) -> Value {
    json!({"usage":raw["usage"] == true,"quota":raw["quota"] == true,"error":raw["error"] == true,"maskCredentials":raw["maskCredentials"] != false})
}

pub fn normalize_notify_config(raw: &Value) -> Value {
    fn string(v: &Value) -> &str {
        v.as_str().unwrap_or("")
    }
    json!({"telegram":{"enabled":raw["telegram"]["enabled"] == true,"botToken":string(&raw["telegram"]["botToken"]).trim(),"chatId":string(&raw["telegram"]["chatId"]).trim()},"webhook":{"enabled":raw["webhook"]["enabled"] == true,"url":string(&raw["webhook"]["url"]).trim(),"secret":string(&raw["webhook"]["secret"])},"webhookEvents":normalize_webhook_events(&raw["webhookEvents"])})
}

pub fn sign_webhook(secret: &str, body: &str) -> String {
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes());
    hmac::sign(&key, body.as_bytes())
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn build_notify_text(event: &str, data: &Value) -> String {
    let mut lines = vec![format!("[9Router Pro] {event}")];
    for (key, prefix) in [
        ("title", ""),
        ("detail", ""),
        ("model", "model: "),
        ("keyName", "key: "),
        ("error", "error: "),
    ] {
        if let Some(s) = data[key].as_str().filter(|s| !s.is_empty()) {
            let s = if key == "error" {
                s.chars().take(300).collect::<String>()
            } else {
                s.to_owned()
            };
            lines.push(format!("{prefix}{s}"));
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn notifications_default_safe_and_sign_correctly() {
        assert_eq!(
            normalize_notify_config(&Value::Null),
            default_notify_config()
        );
        assert_eq!(
            sign_webhook("key", "The quick brown fox jumps over the lazy dog"),
            "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
        );
        assert_eq!(
            build_notify_text("quota", &json!({"model":"x"})),
            "[9Router Pro] quota\nmodel: x"
        );
    }
}
