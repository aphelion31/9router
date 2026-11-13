use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};

use crate::handlers::Credentials;
use crate::server::AppState;

pub fn media_error(status: StatusCode, msg: impl Into<String>) -> Response {
    let msg = msg.into();
    (
        status,
        Json(json!({
            "error": {
                "message": msg,
                "type": if status.is_client_error() { "invalid_request_error" } else { "api_error" },
                "code": status.as_u16()
            }
        })),
    )
        .into_response()
}

pub fn split_provider_model(raw: &str, default_provider: &str) -> (String, String) {
    let trimmed = raw.trim();
    if let Some((p, m)) = trimmed.split_once('/') {
        let canonical = crate::providers::lookup(p).map(|pr| pr.id.as_str()).unwrap_or(p);
        if crate::providers::lookup(canonical).is_some()
            || matches!(
                canonical,
                "google-tts"
                    | "edge-tts"
                    | "local-device"
                    | "elevenlabs"
                    | "xiaomi-mimo"
                    | "selfhosted-tts"
                    | "selfhosted-embedding"
                    | "ollama-local"
                    | "deepgram"
                    | "assemblyai"
                    | "inworld"
                    | "cartesia"
                    | "playht"
                    | "fish-audio"
                    | "coqui"
                    | "tortoise"
                    | "fal-ai"
                    | "black-forest-labs"
                    | "stability-ai"
                    | "runwayml"
                    | "sdwebui"
                    | "comfyui"
                    | "nanobanana"
                    | "huggingface"
            )
        {
            return (canonical.to_owned(), m.to_owned());
        }
    }
    (default_provider.to_owned(), trimmed.to_owned())
}

pub fn resolve_credentials(state: &AppState, provider: &str) -> Option<Credentials> {
    let db = state.db.lock().ok()?;
    let mut st = db
        .prepare("SELECT id,name,data FROM providerConnections WHERE provider=?1 AND isActive=1 ORDER BY priority ASC LIMIT 1")
        .ok()?;
    let mut rows = st
        .query_map([provider], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .ok()?;
    let (id, name, data_str) = rows.next()?.ok()?;
    let data: Value = crate::crypto::parse_data(&data_str).unwrap_or(Value::Null);
    let access_token = data["accessToken"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let api_key = data["apiKey"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    Some(Credentials {
        connection_id: id,
        connection_name: name,
        access_token,
        refresh_token: data["refreshToken"].as_str().map(str::to_owned),
        api_key,
        expires_at: data["expiresAt"].as_str().map(str::to_owned),
        provider_specific_data: data.get("providerSpecificData").cloned().unwrap_or(Value::Null),
        ..Default::default()
    })
}

pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .unwrap_or_default()
}

pub trait RequestJsonExt {
    fn json_body(self, val: &Value) -> Self;
}

impl RequestJsonExt for reqwest::RequestBuilder {
    fn json_body(self, val: &Value) -> Self {
        self.header("Content-Type", "application/json")
            .body(serde_json::to_vec(val).unwrap_or_default())
    }
}

pub async fn read_json(res: reqwest::Response) -> Result<Value, String> {
    let bytes = res.bytes().await.map_err(|e| e.to_string())?;
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}

pub fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
