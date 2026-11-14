use std::collections::HashSet;
use std::future::Future;

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};

use crate::handlers::{Credentials, chat::ChatHooks};
use crate::server::{AppState, Hooks};
use crate::services::member_failover::resolve_combo_member_attempt_cap;

pub enum AttemptOutcome {
    Success(Response),
    Failure {
        status: StatusCode,
        message: String,
        response: Response,
    },
}

pub async fn for_each_account<F, Fut>(
    state: &AppState,
    provider: &str,
    model: &str,
    allow_noauth: bool,
    no_creds_response: impl FnOnce() -> Response,
    mut attempt: F,
) -> Response
where
    F: FnMut(Option<Credentials>) -> Fut,
    Fut: Future<Output = AttemptOutcome>,
{
    let hooks = Hooks {
        state: state.clone(),
        api_key_id: None,
    };
    let settings = hooks.settings().await;
    let cap = resolve_combo_member_attempt_cap(&settings, provider, model);
    let mut excluded = HashSet::new();
    let mut failures = 0usize;
    let mut last_response: Option<Response> = None;

    loop {
        let Some(creds) = hooks.get_credentials(provider, model, &excluded).await else {
            if excluded.is_empty() {
                if allow_noauth {
                    return match attempt(None).await {
                        AttemptOutcome::Success(r) | AttemptOutcome::Failure { response: r, .. } => r,
                    };
                }
                return no_creds_response();
            }
            return last_response.unwrap_or_else(|| {
                media_error(StatusCode::SERVICE_UNAVAILABLE, "All accounts unavailable")
            });
        };

        let conn_id = creds.connection_id.clone();
        match attempt(Some(creds)).await {
            AttemptOutcome::Success(resp) => {
                if conn_id != "noauth" {
                    hooks.on_request_success(&conn_id, model).await;
                }
                return resp;
            }
            AttemptOutcome::Failure {
                status,
                message,
                response,
            } => {
                if conn_id == "noauth"
                    || !hooks
                        .on_account_error(&conn_id, model, status.as_u16(), &message)
                        .await
                {
                    return response;
                }
                excluded.insert(conn_id);
                failures += 1;
                last_response = Some(response);
                if cap.is_some_and(|c| failures >= c) {
                    break;
                }
            }
        }
    }

    last_response.unwrap_or_else(|| {
        media_error(StatusCode::SERVICE_UNAVAILABLE, "All accounts unavailable")
    })
}

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
