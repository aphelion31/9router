use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use rusqlite::OptionalExtension;
use serde_json::{Map, Value, json};
use std::{collections::HashMap, convert::Infallible};

use crate::server::AppState;

const ALLOWED_FILES: &[&str] = &[
    "1_req_client.json",
    "2_req_source.json",
    "3_req_openai.json",
    "4_req_target.json",
    "5_res_provider.txt",
    "6_res_openai.txt",
    "7_res_client.txt",
    "7_res_client.json",
];

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/translator/console-logs",
            get(get_console_logs).delete(clear_console_logs),
        )
        .route("/api/translator/console-logs/stream", get(stream_logs))
        .route("/api/translator/load", get(load_file))
        .route("/api/translator/save", post(save_file))
        .route("/api/translator/translate", post(translate))
        .route("/api/translator/send", post(send_request))
}

async fn get_console_logs(State(s): State<AppState>) -> Response {
    let db = s.db.lock().unwrap();
    let logs: Vec<Value> = crate::db::kv_get(&db, "translator", "console_logs")
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    Json(json!({ "success": true, "logs": logs })).into_response()
}

async fn clear_console_logs(State(s): State<AppState>) -> Response {
    let db = s.db.lock().unwrap();
    let _ = crate::db::kv_set(&db, "translator", "console_logs", "[]");
    Json(json!({ "success": true })).into_response()
}

struct SseLogsStream(Option<Event>);

impl futures_core::Stream for SseLogsStream {
    type Item = Result<Event, Infallible>;
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        match self.0.take() {
            Some(evt) => std::task::Poll::Ready(Some(Ok(evt))),
            None => std::task::Poll::Pending,
        }
    }
}

async fn stream_logs(State(s): State<AppState>) -> Sse<impl futures_core::Stream<Item = Result<Event, Infallible>>> {
    let logs: Vec<Value> = {
        let db = s.db.lock().unwrap();
        crate::db::kv_get(&db, "translator", "console_logs")
            .ok()
            .flatten()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    };
    let init_evt = if logs.is_empty() {
        Event::default().comment("ping")
    } else {
        Event::default().data(json!({ "type": "init", "logs": logs }).to_string())
    };
    Sse::new(SseLogsStream(Some(init_evt))).keep_alive(KeepAlive::default())
}

async fn load_file(
    State(s): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let Some(file) = params.get("file").filter(|f| !f.is_empty()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "error": "File parameter required" })),
        )
            .into_response();
    };
    if !ALLOWED_FILES.contains(&file.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "error": "Invalid file name" })),
        )
            .into_response();
    }
    let db = s.db.lock().unwrap();
    match crate::db::kv_get(&db, "translator_file", file)
        .ok()
        .flatten()
    {
        Some(content) => Json(json!({ "success": true, "content": content })).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({ "success": false, "error": "File not found" })),
        )
            .into_response(),
    }
}

fn is_auth_header(k: &str) -> bool {
    matches!(
        k.to_ascii_lowercase().as_str(),
        "authorization" | "x-api-key" | "api-key" | "x-goog-api-key"
    )
}

fn scrub_node(v: &mut Value) {
    match v {
        Value::Array(arr) => arr.iter_mut().for_each(scrub_node),
        Value::Object(map) => {
            for (k, val) in map.iter_mut() {
                if is_auth_header(k) {
                    *val = Value::String(if k.eq_ignore_ascii_case("authorization") {
                        "Bearer ***".into()
                    } else {
                        "***".into()
                    });
                } else {
                    scrub_node(val);
                }
            }
        }
        _ => {}
    }
}

fn scrub_auth_material(content: &str) -> String {
    let Ok(mut parsed) = serde_json::from_str::<Value>(content) else {
        return content.to_owned();
    };
    scrub_node(&mut parsed);
    serde_json::to_string_pretty(&parsed).unwrap_or_else(|_| content.to_owned())
}

async fn save_file(State(s): State<AppState>, Json(body): Json<Value>) -> Response {
    let Some(file) = body.get("file").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "error": "File and content required" })),
        )
            .into_response();
    };
    let Some(content_val) = body.get("content") else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "error": "File and content required" })),
        )
            .into_response();
    };
    if !ALLOWED_FILES.contains(&file) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "error": "Invalid file name" })),
        )
            .into_response();
    }
    let raw = match content_val.as_str() {
        Some(s) => scrub_auth_material(s),
        None => scrub_auth_material(&content_val.to_string()),
    };
    let db = s.db.lock().unwrap();
    let _ = crate::db::kv_set(&db, "translator_file", file, &raw);
    Json(json!({ "success": true })).into_response()
}

fn parse_provider_model(model_str: &str) -> (&str, &str) {
    match model_str.split_once('/') {
        Some((p, m)) if !p.is_empty() && !m.is_empty() => (p, m),
        _ => ("openai", model_str),
    }
}

fn detect_source_format(body: &Value) -> &'static str {
    if body.get("contents").is_some() {
        "gemini"
    } else if body.get("anthropic_version").is_some()
        || (body.get("system").is_some() && body.get("messages").is_some())
    {
        "claude"
    } else {
        "openai"
    }
}

fn target_format_for(provider: &str) -> &'static str {
    match provider {
        "claude" | "anthropic" => "claude",
        "gemini" | "gemini-cli" | "antigravity" => "gemini",
        _ => "openai",
    }
}

fn has_active_connection(state: &AppState, provider: &str) -> Option<Value> {
    let db = state.db.lock().unwrap();
    let data_str: Option<String> = db
        .query_row(
            "SELECT data FROM providerConnections WHERE provider=?1 AND isActive=1 ORDER BY priority ASC LIMIT 1",
            [provider],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten();
    data_str.and_then(|s| serde_json::from_str::<Value>(&s).ok())
}

async fn translate(State(s): State<AppState>, Json(req): Json<Value>) -> Response {
    let Some(step) = req.get("step").and_then(|v| v.as_i64()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "error": "Step and body required" })),
        )
            .into_response();
    };
    let Some(body) = req.get("body") else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "error": "Step and body required" })),
        )
            .into_response();
    };

    match step {
        1 => {
            let client_body = body.get("body").unwrap_or(body);
            let raw_model = client_body["model"].as_str().unwrap_or("");
            let (provider, model) = parse_provider_model(raw_model);
            let source_format = detect_source_format(client_body);
            let target_format = target_format_for(provider);
            Json(json!({
                "success": true,
                "result": {
                    "provider": provider,
                    "model": model,
                    "sourceFormat": source_format,
                    "targetFormat": target_format,
                }
            }))
            .into_response()
        }
        2 => {
            // ponytail: step 2 normalizes client body to OpenAI intermediate shape
            let client_body = body.get("body").unwrap_or(body);
            let mut out = client_body.clone();
            if let Some(obj) = out.as_object_mut() {
                if let Some(m) = obj.get("model").and_then(|v| v.as_str()).map(str::to_owned) {
                    let (_, model) = parse_provider_model(&m);
                    obj.insert("model".into(), Value::String(model.to_owned()));
                }
                obj.remove("_toolNameMap");
            }
            Json(json!({ "success": true, "result": { "body": out } })).into_response()
        }
        3 => {
            let openai_body = body.get("body").unwrap_or(body);
            let provider = body["provider"].as_str().unwrap_or("");
            let model = body["model"].as_str().unwrap_or("");
            if provider.is_empty() || model.is_empty() {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "success": false, "error": "provider and model required" })),
                )
                    .into_response();
            }
            if has_active_connection(&s, provider).is_none() {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "success": false,
                        "error": format!("No active connection for provider: {provider}")
                    })),
                )
                    .into_response();
            }
            let url = match target_format_for(provider) {
                "claude" => "https://api.anthropic.com/v1/messages",
                "gemini" => "https://generativelanguage.googleapis.com/v1beta/models",
                _ => "https://api.openai.com/v1/chat/completions",
            };
            let mut headers = Map::new();
            headers.insert("Content-Type".into(), Value::String("application/json".into()));
            headers.insert("Authorization".into(), Value::String("Bearer ***".into()));
            Json(json!({
                "success": true,
                "result": {
                    "url": url,
                    "headers": headers,
                    "body": openai_body,
                }
            }))
            .into_response()
        }
        _ => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "error": "Invalid step (1-3)" })),
        )
            .into_response(),
    }
}

async fn send_request(State(s): State<AppState>, Json(req): Json<Value>) -> Response {
    let provider = req["provider"].as_str().unwrap_or("");
    let model = req["model"].as_str().unwrap_or("");
    let Some(body) = req.get("body") else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "error": "provider, model, and body required" })),
        )
            .into_response();
    };
    if provider.is_empty() || model.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "error": "provider, model, and body required" })),
        )
            .into_response();
    }
    let Some(conn) = has_active_connection(&s, provider) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "success": false,
                "error": format!("No active connection for provider: {provider}")
            })),
        )
            .into_response();
    };

    let token = conn["accessToken"]
        .as_str()
        .or_else(|| conn["apiKey"].as_str())
        .unwrap_or("");
    let url = match target_format_for(provider) {
        "claude" => "https://api.anthropic.com/v1/messages",
        "gemini" => "https://generativelanguage.googleapis.com/v1beta/models",
        _ => "https://api.openai.com/v1/chat/completions",
    };

    let mut payload = body.clone();
    if let Some(obj) = payload.as_object_mut() {
        obj.entry("model").or_insert_with(|| Value::String(model.to_owned()));
    }

    let res = match s
        .executor
        .0
        .post(url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .body(payload.to_string())
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "success": false, "error": e.to_string() })),
            )
                .into_response();
        }
    };

    if !res.status().is_success() {
        let status = res.status();
        let details = res.text().await.unwrap_or_default();
        return (
            StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
            Json(json!({
                "success": false,
                "error": format!("Provider error: {}", status.as_u16()),
                "details": details,
            })),
        )
            .into_response();
    }

    let text = res.text().await.unwrap_or_default();
    (
        StatusCode::OK,
        [("Content-Type", "text/event-stream")],
        text,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::executor::HttpExecutor;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use rusqlite::Connection;
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;

    fn test_app() -> Router {
        let db = Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        let state = AppState {
            db: Arc::new(Mutex::new(db)),
            executor: Arc::new(HttpExecutor(reqwest::Client::new())),
            jwt_secret: None,
        };
        router().with_state(state)
    }

    #[tokio::test]
    async fn save_and_load_scrubs_auth_and_rejects_invalid_file() {
        let app = test_app();

        let r = app
            .clone()
            .oneshot(
                Request::get("/api/translator/load?file=../../etc/passwd")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 400);

        let r = app
            .clone()
            .oneshot(
                Request::post("/api/translator/save")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "file": "1_req_client.json",
                            "content": "{\"headers\":{\"Authorization\":\"Bearer secret-123\",\"x-api-key\":\"k-secret\"}}"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 200);

        let r = app
            .oneshot(
                Request::get("/api/translator/load?file=1_req_client.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let body: Value =
            serde_json::from_slice(&to_bytes(r.into_body(), 4096).await.unwrap()).unwrap();
        let loaded = body["content"].as_str().unwrap();
        assert!(!loaded.contains("secret-123"));
        assert!(!loaded.contains("k-secret"));
        assert!(loaded.contains("Bearer ***"));
    }

    #[tokio::test]
    async fn translate_step1_and_console_logs_work() {
        let app = test_app();

        let r = app
            .clone()
            .oneshot(
                Request::post("/api/translator/translate")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "step": 1,
                            "body": { "model": "claude/claude-3-5-sonnet", "system": "hi", "messages": [] }
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let body: Value =
            serde_json::from_slice(&to_bytes(r.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(body["result"]["provider"], "claude");
        assert_eq!(body["result"]["sourceFormat"], "claude");

        let r = app
            .oneshot(
                Request::get("/api/translator/console-logs")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
    }
}
