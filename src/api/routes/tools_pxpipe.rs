use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use crate::server::AppState;
use crate::services::process_supervisor::{self, FixedCommand, ProcessSupervisor, SupervisorError};

const PXPIPE_PROCESS_ID: &str = "pxpipe";
const PXPIPE_PACKAGE: &str = "pxpipe-proxy@latest";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/pxpipe/status", get(status))
        .route("/api/pxpipe/health", get(health).post(health))
        .route("/api/pxpipe/stats", get(stats))
        .route("/api/pxpipe/logs", get(logs))
        .route("/api/pxpipe/install", post(install))
        .route("/api/pxpipe/start", post(start))
        .route("/api/pxpipe/stop", post(stop))
        .route("/api/pxpipe/restart", post(restart))
}

fn pxpipe_dir() -> PathBuf {
    process_supervisor::data_dir().join("pxpipe")
}

fn library_entry() -> PathBuf {
    pxpipe_dir()
        .join("node_modules")
        .join("pxpipe-proxy")
        .join("dist")
        .join("core")
        .join("library.js")
}

fn is_installed() -> bool {
    pxpipe_dir()
        .join("node_modules")
        .join("pxpipe-proxy")
        .join("package.json")
        .exists()
        && library_entry().exists()
}

async fn build_status_json(s: &AppState) -> Value {
    let rec = process_supervisor::global()
        .status(&s.db, PXPIPE_PROCESS_ID)
        .await;
    let db = s.db.lock().unwrap();
    let settings = crate::db::settings_get(&db).unwrap_or(Value::Null);
    let installed = is_installed();
    let running = rec.status == "running";
    json!({
        "installed": installed,
        "loaded": running,
        "running": running,
        "pid": rec.pid,
        "enabled": settings["pxpipeEnabled"].as_bool().unwrap_or(false),
        "autoInstall": settings["pxpipeAutoInstall"].as_bool().unwrap_or(false),
        "minChars": settings["pxpipeMinChars"],
        "timeoutMs": settings["pxpipeTimeoutMs"],
    })
}

async fn run_npm_install() -> Result<Value, Response> {
    let dir = pxpipe_dir();
    let _ = std::fs::create_dir_all(&dir);
    let pkg_json = dir.join("package.json");
    if !pkg_json.exists() {
        let _ = std::fs::write(
            &pkg_json,
            r#"{"name":"9router-pxpipe-host","private":true}"#,
        );
    }
    let cmd = FixedCommand::new(
        "npm",
        vec![
            "install".to_owned(),
            PXPIPE_PACKAGE.to_owned(),
            "--no-audit".to_owned(),
            "--no-fund".to_owned(),
            "--omit=dev".to_owned(),
        ],
    );
    let log_path = dir.join("install.log");
    match ProcessSupervisor::run_once(cmd, Some(&dir), Some(&log_path), Duration::from_secs(300))
        .await
    {
        Ok(out) if out.status.success() => Ok(json!({
            "installed": is_installed(),
            "path": dir.join("node_modules").join("pxpipe-proxy"),
        })),
        Ok(out) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "error": format!("npm install exited with code={:?}", out.status.code()),
                "code": "INSTALL_FAILED",
            })),
        )
            .into_response()),
        Err(SupervisorError::NotInstalled(msg)) => Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": msg, "code": "NPM_NOT_FOUND" })),
        )
            .into_response()),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string(), "code": "pxpipe_failed" })),
        )
            .into_response()),
    }
}

async fn install(State(s): State<AppState>) -> Response {
    match run_npm_install().await {
        Ok(info) => {
            let _ = process_supervisor::global()
                .stop(&s.db, PXPIPE_PROCESS_ID, Duration::from_secs(1))
                .await;
            Json(json!({
                "installed": info["installed"],
                "path": info["path"],
                "health": { "healthy": is_installed(), "checks": [] },
            }))
            .into_response()
        }
        Err(resp) => resp,
    }
}

async fn start(State(s): State<AppState>) -> Response {
    if !is_installed() {
        let auto_install = {
            let db = s.db.lock().unwrap();
            crate::db::settings_get(&db)
                .ok()
                .and_then(|v| v["pxpipeAutoInstall"].as_bool())
                .unwrap_or(false)
        };
        if !auto_install {
            return (
                StatusCode::CONFLICT,
                Json(json!({ "error": "PXPIPE is not installed", "code": "NOT_INSTALLED" })),
            )
                .into_response();
        }
        if let Err(resp) = run_npm_install().await {
            return resp;
        }
    }

    let entry = library_entry().to_string_lossy().into_owned();
    let cmd = FixedCommand::new("node", vec![entry]);
    let log_path = pxpipe_dir().join("pxpipe.log");
    match process_supervisor::global()
        .spawn(&s.db, PXPIPE_PROCESS_ID, cmd, Some(&log_path))
        .await
    {
        Ok(_) => Json(build_status_json(&s).await).into_response(),
        Err(SupervisorError::NotInstalled(msg)) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": msg, "code": "NODE_NOT_FOUND" })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string(), "code": "pxpipe_failed" })),
        )
            .into_response(),
    }
}

async fn stop(State(s): State<AppState>) -> Response {
    let was_running = process_supervisor::global()
        .stop(&s.db, PXPIPE_PROCESS_ID, Duration::from_secs(2))
        .await
        .ok()
        .flatten()
        .is_some();
    let mut st = build_status_json(&s).await;
    if let Some(obj) = st.as_object_mut() {
        obj.insert("stopped".into(), Value::Bool(was_running));
    }
    Json(st).into_response()
}

async fn restart(State(s): State<AppState>) -> Response {
    let _ = process_supervisor::global()
        .stop(&s.db, PXPIPE_PROCESS_ID, Duration::from_secs(2))
        .await;
    start(State(s)).await
}

async fn status(State(s): State<AppState>) -> Response {
    Json(build_status_json(&s).await).into_response()
}

async fn health() -> Response {
    Json(json!({
        "healthy": is_installed(),
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
    let install_log = std::fs::read_to_string(pxpipe_dir().join("install.log")).unwrap_or_default();
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
        "installLog": install_log,
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
    async fn pxpipe_lifecycle_routes_work_without_501() {
        let app = test_app();

        // start without install and autoInstall=false returns 409 NOT_INSTALLED
        let r = app
            .clone()
            .oneshot(Request::post("/api/pxpipe/start").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_ne!(r.status(), 501);
        if !is_installed() {
            assert_eq!(r.status(), 409);
            let body: Value =
                serde_json::from_slice(&to_bytes(r.into_body(), 4096).await.unwrap()).unwrap();
            assert_eq!(body["code"], "NOT_INSTALLED");
        }

        // stop when not running returns 200 stopped: false
        let r = app
            .clone()
            .oneshot(Request::post("/api/pxpipe/stop").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let body: Value =
            serde_json::from_slice(&to_bytes(r.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(body["stopped"], false);

        // restart when not installed returns 409 (not 501)
        let r = app
            .oneshot(Request::post("/api/pxpipe/restart").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_ne!(r.status(), 501);
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
