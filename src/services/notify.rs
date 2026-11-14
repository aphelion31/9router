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

/// Telegram sendMessage URL. The token is part of the path, so callers must never log it.
pub fn telegram_send_url(bot_token: &str) -> String {
    format!("https://api.telegram.org/bot{bot_token}/sendMessage")
}

/// Webhook headers. The signature is only sent when a secret is configured.
pub fn webhook_headers(secret: &str, body: &str) -> Vec<(&'static str, String)> {
    let mut headers = vec![("Content-Type", "application/json".to_owned())];
    if !secret.is_empty() {
        headers.push(("X-9Router-Signature", sign_webhook(secret, body)));
    }
    headers
}

pub fn read_config(db: &rusqlite::Connection) -> Value {
    let raw = crate::db::kv_get(db, "notify", "config").ok().flatten()
        .and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_else(default_notify_config);
    normalize_notify_config(&raw)
}

fn redact(value: &Value, mask: bool, depth: usize) -> Value {
    if depth > 4 { return json!("[truncated]"); }
    match value {
        Value::Array(items) => Value::Array(items.iter().take(50).map(|v| redact(v, mask, depth + 1)).collect()),
        Value::Object(items) => Value::Object(items.iter().filter_map(|(k, v)| {
            if ["apiKey", "apikey", "api_key", "accessToken", "access_token", "refreshToken", "refresh_token", "idToken", "id_token", "secret", "token", "password", "botToken", "clientSecret", "sessionToken", "cookie", "authorization"].contains(&k.as_str()) {
                mask.then(|| (k.clone(), json!("[masked]")))
            } else { Some((k.clone(), redact(v, mask, depth + 1))) }
        }).collect()),
        Value::String(s) => json!(s.chars().take(2000).collect::<String>()),
        _ => value.clone(),
    }
}

async fn post(client: &reqwest::Client, url: &str, body: String, secret: &str) -> bool {
    let mut request = client.post(url).timeout(std::time::Duration::from_millis(NOTIFY_TIMEOUT_MS));
    for (name, value) in webhook_headers(secret, &body) { request = request.header(name, value); }
    request.body(body).send().await.is_ok_and(|r| r.status().is_success())
}

fn payload(event: &str, data: Value) -> String {
    let millis = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as i64;
    json!({"event": event, "timestamp": crate::services::usage_shared::unix_millis_iso(millis), "data": data}).to_string()
}

pub async fn send_notification(client: &reqwest::Client, cfg: &Value, event: &str, data: Value) -> Value {
    let mut results = json!({"telegram":"skipped", "webhook":"skipped"});
    let telegram = &cfg["telegram"];
    let token = telegram["botToken"].as_str().unwrap_or("");
    let chat = telegram["chatId"].as_str().unwrap_or("");
    if telegram["enabled"] == true && !token.is_empty() && !chat.is_empty() {
        let body = json!({"chat_id":chat,"text":build_notify_text(event, &data)}).to_string();
        results["telegram"] = json!(if post(client, &telegram_send_url(token), body, "").await { "sent" } else { "failed" });
    }
    let webhook = &cfg["webhook"];
    let url = webhook["url"].as_str().unwrap_or("");
    if webhook["enabled"] == true && !url.is_empty() {
        results["webhook"] = json!(if post(client, url, payload(event, data), webhook["secret"].as_str().unwrap_or("")).await { "sent" } else { "failed" });
    }
    results
}

pub fn emit(db: std::sync::Arc<std::sync::Mutex<rusqlite::Connection>>, event: &'static str, data: Value) {
    tokio::spawn(async move {
        let cfg = match db.lock() { Ok(db) => read_config(&db), Err(_) => return };
        let wh = &cfg["webhook"];
        let url = wh["url"].as_str().unwrap_or("");
        if wh["enabled"] != true || url.is_empty() || cfg["webhookEvents"][event] != true { return; }
        let data = redact(&data, cfg["webhookEvents"]["maskCredentials"] != false, 0);
        let _ = post(&reqwest::Client::new(), url, payload(event, data), wh["secret"].as_str().unwrap_or("")).await;
    });
}

#[cfg(test)]
mod delivery_tests {
    use super::*;
    #[tokio::test]
    async fn signed_event_is_delivered_and_secrets_redacted() {
        use axum::{Router, routing::post, extract::State, http::HeaderMap, body::Bytes};
        use std::sync::{Arc, Mutex};
        let received = Arc::new(Mutex::new(None));
        let app = Router::new().route("/", post(|State(received): State<Arc<Mutex<Option<(HeaderMap, Bytes)>>>>, headers: HeaderMap, body: Bytes| async move {
            *received.lock().unwrap() = Some((headers, body));
        })).with_state(received.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE kv(scope TEXT, key TEXT, value TEXT, PRIMARY KEY(scope,key));").unwrap();
        let cfg = json!({"webhook":{"enabled":true,"url":format!("http://{addr}/"),"secret":"key"},"webhookEvents":{"usage":true}});
        crate::db::kv_set(&db, "notify", "config", &cfg.to_string()).unwrap();
        emit(Arc::new(Mutex::new(db)), "usage", json!({"apiKey":"private","model":"x"}));
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop { if received.lock().unwrap().is_some() { break; } tokio::time::sleep(std::time::Duration::from_millis(10)).await; }
        }).await.unwrap();
        let guard = received.lock().unwrap();
        let (headers, body) = guard.as_ref().unwrap();
        let text = std::str::from_utf8(body).unwrap();
        assert_eq!(headers["x-9router-signature"], sign_webhook("key", text));
        let value: Value = serde_json::from_str(text).unwrap();
        assert_eq!(value["data"]["apiKey"], "[masked]");
        assert_eq!(value["event"], "usage");
        server.abort();
    }
    #[test]
    fn telegram_url_and_optional_signature() {
        assert_eq!(
            telegram_send_url("T"),
            "https://api.telegram.org/botT/sendMessage"
        );
        assert_eq!(webhook_headers("", "{}").len(), 1);
        let signed = webhook_headers("key", "{}");
        assert_eq!(signed[1].0, "X-9Router-Signature");
        assert_eq!(signed[1].1, sign_webhook("key", "{}"));
    }
}
