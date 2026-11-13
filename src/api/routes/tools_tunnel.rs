use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};
use std::time::Duration;

use crate::server::AppState;
use crate::services::process_supervisor::{self, FixedCommand, ProcessSupervisor, SupervisorError};

const TUNNEL_PROCESS_ID: &str = "cloudflared";
const KV_SCOPE: &str = "tunnel";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/tunnel/status", get(status))
        .route("/api/tunnel/enable", post(enable))
        .route("/api/tunnel/disable", post(disable))
        .route("/api/tunnel/tailscale-check", get(tailscale_check))
        .route("/api/tunnel/tailscale-enable", post(tailscale_enable))
        .route("/api/tunnel/tailscale-disable", post(tailscale_disable))
        .route("/api/tunnel/tailscale-install", post(tailscale_install))
}

fn load_kv_json(s: &AppState, key: &str) -> Value {
    let db = s.db.lock().unwrap();
    crate::db::kv_get(&db, KV_SCOPE, key)
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .unwrap_or_else(|| json!({}))
}

fn save_kv_json(s: &AppState, key: &str, val: &Value) {
    if let Ok(db) = s.db.lock() {
        let _ = crate::db::kv_set(&db, KV_SCOPE, key, &val.to_string());
    }
}

async fn enable(State(s): State<AppState>) -> Response {
    let cmd = FixedCommand::new(
        "cloudflared",
        vec![
            "tunnel".to_owned(),
            "--url".to_owned(),
            "http://127.0.0.1:20128".to_owned(),
        ],
    );
    let log_path = process_supervisor::data_dir()
        .join("tunnel")
        .join("cloudflared.log");
    match process_supervisor::global()
        .spawn(&s.db, TUNNEL_PROCESS_ID, cmd, Some(&log_path))
        .await
    {
        Ok(rec) => {
            save_kv_json(&s, "cloudflare", &json!({ "enabled": true, "pid": rec.pid }));
            Json(json!({
                "success": true,
                "enabled": true,
                "running": true,
                "pid": rec.pid,
            }))
            .into_response()
        }
        Err(SupervisorError::NotInstalled(msg)) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": msg, "code": "NOT_INSTALLED" })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn disable(State(s): State<AppState>) -> Response {
    let stopped = process_supervisor::global()
        .stop(&s.db, TUNNEL_PROCESS_ID, Duration::from_secs(2))
        .await
        .ok()
        .flatten()
        .is_some();
    save_kv_json(&s, "cloudflare", &json!({ "enabled": false }));
    Json(json!({ "success": true, "enabled": false, "stopped": stopped })).into_response()
}

async fn tailscale_check() -> Response {
    let out = ProcessSupervisor::run_once(
        FixedCommand::new("tailscale", vec!["status".to_owned(), "--json".to_owned()]),
        None,
        None,
        Duration::from_secs(2),
    )
    .await;
    match out {
        Ok(o) if o.status.success() => {
            Json(json!({ "available": true, "running": true })).into_response()
        }
        Ok(_) => Json(json!({ "available": true, "running": false })).into_response(),
        Err(SupervisorError::NotInstalled(_)) => {
            Json(json!({ "available": false, "running": false })).into_response()
        }
        Err(_) => Json(json!({ "available": false, "running": false })).into_response(),
    }
}

async fn tailscale_enable(State(s): State<AppState>) -> Response {
    let cmd = FixedCommand::new(
        "tailscale",
        vec!["funnel".to_owned(), "--bg".to_owned(), "20128".to_owned()],
    );
    match ProcessSupervisor::run_once(cmd, None, None, Duration::from_secs(15)).await {
        Ok(o) if o.status.success() => {
            save_kv_json(&s, "tailscale", &json!({ "enabled": true, "running": true }));
            Json(json!({ "success": true, "running": true })).into_response()
        }
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr).trim().to_owned();
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": if stderr.is_empty() { "tailscale funnel failed".to_owned() } else { stderr } })),
            )
                .into_response()
        }
        Err(SupervisorError::NotInstalled(msg)) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": msg, "code": "NOT_INSTALLED" })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn tailscale_disable(State(s): State<AppState>) -> Response {
    let cmd = FixedCommand::new(
        "tailscale",
        vec!["funnel".to_owned(), "--bg".to_owned(), "off".to_owned()],
    );
    match ProcessSupervisor::run_once(cmd, None, None, Duration::from_secs(15)).await {
        Ok(o) if o.status.success() => {
            save_kv_json(&s, "tailscale", &json!({ "enabled": false, "running": false }));
            Json(json!({ "success": true, "running": false })).into_response()
        }
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr).trim().to_owned();
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": if stderr.is_empty() { "tailscale funnel off failed".to_owned() } else { stderr } })),
            )
                .into_response()
        }
        Err(SupervisorError::NotInstalled(msg)) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": msg, "code": "NOT_INSTALLED" })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn tailscale_install() -> Response {
    let has_brew = ProcessSupervisor::run_once(
        FixedCommand::new("brew", vec!["--version".to_owned()]),
        None,
        None,
        Duration::from_secs(3),
    )
    .await
    .is_ok_and(|o| o.status.success());

    let sse = if !has_brew {
        format!(
            "event: error\ndata: {}\n\n",
            json!({ "error": "Homebrew (brew) is required for automated Tailscale installation" })
        )
    } else {
        let mut events = format!(
            "event: progress\ndata: {}\n\n",
            json!({ "message": "Installing Tailscale via Homebrew..." })
        );
        match ProcessSupervisor::run_once(
            FixedCommand::new(
                "brew",
                vec!["install".to_owned(), "--cask".to_owned(), "tailscale".to_owned()],
            ),
            None,
            None,
            Duration::from_secs(300),
        )
        .await
        {
            Ok(o) if o.status.success() => {
                events.push_str(&format!(
                    "event: done\ndata: {}\n\n",
                    json!({ "success": true, "authUrl": null })
                ));
            }
            Ok(o) => {
                let msg = String::from_utf8_lossy(&o.stderr).trim().to_owned();
                events.push_str(&format!(
                    "event: error\ndata: {}\n\n",
                    json!({ "error": if msg.is_empty() { "brew install failed".to_owned() } else { msg } })
                ));
            }
            Err(e) => {
                events.push_str(&format!(
                    "event: error\ndata: {}\n\n",
                    json!({ "error": e.to_string() })
                ));
            }
        }
        events
    };

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/event-stream"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        Body::from(sse),
    )
        .into_response()
}

async fn status(State(s): State<AppState>) -> Response {
    let rec = process_supervisor::global()
        .status(&s.db, TUNNEL_PROCESS_ID)
        .await;
    let cf = load_kv_json(&s, "cloudflare");
    let ts = load_kv_json(&s, "tailscale");
    let running = rec.status == "running";
    let enabled = cf["enabled"].as_bool().unwrap_or(false) || running;
    Json(json!({
        "tunnel": { "enabled": enabled, "running": running, "pid": rec.pid, "url": null },
        "tailscale": {
            "available": ts["running"].as_bool().unwrap_or(false),
            "running": ts["running"].as_bool().unwrap_or(false),
        },
        "download": { "downloading": false, "progress": 0 },
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::executor::HttpExecutor;
    use axum::body::to_bytes;
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
    async fn tunnel_routes_work_without_501() {
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

        let r = app
            .clone()
            .oneshot(Request::post("/api/tunnel/disable").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let body: Value =
            serde_json::from_slice(&to_bytes(r.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(body["success"], true);
        assert_eq!(body["enabled"], false);

        for path in [
            "/api/tunnel/enable",
            "/api/tunnel/tailscale-enable",
            "/api/tunnel/tailscale-disable",
        ] {
            let r = app
                .clone()
                .oneshot(Request::post(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_ne!(r.status(), 501, "expected non-501 for {path}");
        }
    }
}
