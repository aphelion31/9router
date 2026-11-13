use axum::{
    Json, Router,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::json;

use crate::server::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/tunnel/status", get(status))
        .route("/api/tunnel/enable", post(not_available))
        .route("/api/tunnel/disable", post(not_available))
        .route("/api/tunnel/tailscale-check", get(tailscale_check))
        .route("/api/tunnel/tailscale-enable", post(not_available))
        .route("/api/tunnel/tailscale-disable", post(not_available))
        .route("/api/tunnel/tailscale-install", post(not_available))
}

async fn tailscale_check() -> Response {
    Json(json!({ "available": false, "running": false })).into_response()
}

// ponytail: tunnel enable/disable spawn cloudflared child process; not available in Rust
async fn not_available() -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({ "error": "not available" })),
    )
        .into_response()
}

async fn status() -> Response {
    Json(json!({
        "tunnel": { "enabled": false, "running": false, "url": null },
        "tailscale": { "available": false, "running": false },
        "download": { "downloading": false, "progress": 0 },
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
    use serde_json::Value;
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
    async fn tunnel_status_and_501() {
        let app = test_app();

        let r = app
            .clone()
            .oneshot(Request::get("/api/tunnel/status").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let body: Value =
            serde_json::from_slice(&to_bytes(r.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(body["tunnel"]["enabled"], false);

        for path in ["/api/tunnel/enable", "/api/tunnel/disable"] {
            let r = app
                .clone()
                .oneshot(Request::post(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(r.status(), 501);
        }
    }
}
