use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};
use std::collections::HashMap;

use crate::server::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/pxpipe/status", get(status))
        .route("/api/pxpipe/health", get(health).post(health))
        .route("/api/pxpipe/stats", get(stats))
        .route("/api/pxpipe/logs", get(logs))
        .route("/api/pxpipe/install", post(not_available))
        .route("/api/pxpipe/start", post(not_available))
        .route("/api/pxpipe/stop", post(not_available))
        .route("/api/pxpipe/restart", post(not_available))
}

// ponytail: pxpipe install/start/stop/restart load or npm-install a Node.js package; not available in Rust
async fn not_available() -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({ "error": "not available" })),
    )
        .into_response()
}

async fn status(State(s): State<AppState>) -> Response {
    let db = s.db.lock().unwrap();
    let settings = crate::db::settings_get(&db).unwrap_or(Value::Null);
    Json(json!({
        "installed": false,
        "loaded": false,
        "running": false,
        "enabled": settings["pxpipeEnabled"].as_bool().unwrap_or(false),
        "autoInstall": settings["pxpipeAutoInstall"].as_bool().unwrap_or(false),
        "minChars": settings["pxpipeMinChars"],
        "timeoutMs": settings["pxpipeTimeoutMs"],
    }))
    .into_response()
}

async fn health() -> Response {
    Json(json!({
        "healthy": false,
        "checks": [],
    }))
    .into_response()
}

async fn stats(
    State(s): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let limit = params
        .get("limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(100)
        .min(500);
    let db = s.db.lock().unwrap();
    let mut stored = crate::db::kv_get(&db, "pxpipe", "stats")
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .unwrap_or_else(|| {
            json!({
                "totalEvents": 0,
                "totalSavedTokens": 0,
                "totalSavedChars": 0,
                "recent": [],
            })
        });
    if let Some(arr) = stored.get_mut("recent").and_then(|v| v.as_array_mut()) {
        arr.truncate(limit);
    }
    Json(stored).into_response()
}

async fn logs(
    State(s): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let limit = params
        .get("limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(100)
        .min(500);
    let db = s.db.lock().unwrap();
    let events: Vec<Value> = crate::db::kv_get(&db, "pxpipe", "events")
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .rev()
        .take(limit)
        .collect();
    Json(json!({
        "installLog": "",
        "events": events,
    }))
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
    async fn pxpipe_lifecycle_routes_return_501() {
        let app = test_app();
        for path in [
            "/api/pxpipe/install",
            "/api/pxpipe/start",
            "/api/pxpipe/stop",
            "/api/pxpipe/restart",
        ] {
            let r = app
                .clone()
                .oneshot(Request::post(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(r.status(), 501, "expected 501 for {path}");
            let body: Value =
                serde_json::from_slice(&to_bytes(r.into_body(), 4096).await.unwrap()).unwrap();
            assert_eq!(body["error"], "not available");
        }
    }

    #[tokio::test]
    async fn pxpipe_read_routes_return_defaults() {
        let app = test_app();
        let r = app
            .clone()
            .oneshot(Request::get("/api/pxpipe/status").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let body: Value =
            serde_json::from_slice(&to_bytes(r.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(body["installed"], false);
        assert_eq!(body["enabled"], false);

        for (method, path) in [
            ("GET", "/api/pxpipe/health"),
            ("POST", "/api/pxpipe/health"),
            ("GET", "/api/pxpipe/stats"),
            ("GET", "/api/pxpipe/logs"),
        ] {
            let r = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(path)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(r.status(), 200, "expected 200 for {method} {path}");
        }
    }
}
