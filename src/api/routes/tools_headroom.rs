use axum::{
    Json, Router,
    body::Body,
    extract::{Path, Query, Request, State},
    http::{HeaderMap, HeaderName, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, get, post},
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use crate::server::AppState;
use crate::services::process_supervisor::{self, FixedCommand, SupervisorError};

const DEFAULT_HEADROOM_URL: &str = "http://127.0.0.1:8787";
const HEADROOM_PROCESS_ID: &str = "headroom";
const HEADROOM_EXTRAS: &[&str] = &["code", "ml"];

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/headroom/status", get(status))
        .route("/api/headroom/start", post(start))
        .route("/api/headroom/stop", post(stop))
        .route("/api/headroom/restart", post(restart))
        .route(
            "/api/headroom/extras",
            get(extras_get).post(extras_post).delete(extras_delete),
        )
        .route("/api/headroom/proxy/{*path}", any(proxy))
        .route("/api/headroom/proxy", any(proxy_root))
}

fn headroom_dir() -> PathBuf {
    process_supervisor::data_dir().join("headroom")
}

fn headroom_settings(state: &AppState) -> (String, bool, bool) {
    let db = state.db.lock().unwrap();
    let settings = crate::db::settings_get(&db).unwrap_or(Value::Null);
    let url = settings["headroomUrl"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_HEADROOM_URL)
        .to_owned();
    let code_aware = settings["headroomCodeAware"].as_bool().unwrap_or(false);
    let kompress = settings["headroomKompress"].as_bool().unwrap_or(true);
    (url, code_aware, kompress)
}

fn headroom_url(state: &AppState) -> String {
    headroom_settings(state).0
}

fn parse_port(url: &str) -> u16 {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.port())
        .filter(|&p| p > 0)
        .unwrap_or(8787)
}

fn build_headroom_cmd(port: u16, code_aware: bool, kompress: bool) -> FixedCommand {
    let mut args = vec!["proxy".to_owned(), "--port".to_owned(), port.to_string()];
    if code_aware {
        args.push("--code-aware".to_owned());
    }
    if !kompress {
        args.push("--disable-kompress".to_owned());
    }
    FixedCommand::new("headroom", args)
}

fn supervisor_error_response(err: SupervisorError, default_code: &str) -> Response {
    match err {
        SupervisorError::NotInstalled(msg) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": msg, "code": "NOT_INSTALLED" })),
        )
            .into_response(),
        other => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": other.to_string(), "code": default_code })),
        )
            .into_response(),
    }
}

async fn start(State(s): State<AppState>) -> Response {
    let (url, code_aware, kompress) = headroom_settings(&s);
    if !is_loopback_url(&url) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "External Headroom proxies must be started outside 9Router",
                "code": "EXTERNAL_PROXY",
            })),
        )
            .into_response();
    }
    let port = parse_port(&url);
    let cmd = build_headroom_cmd(port, code_aware, kompress);
    let log_path = headroom_dir().join("proxy.log");
    match process_supervisor::global()
        .spawn(&s.db, HEADROOM_PROCESS_ID, cmd, Some(&log_path))
        .await
    {
        Ok(rec) => Json(json!({
            "success": true,
            "pid": rec.pid,
            "alreadyRunning": rec.already_running.unwrap_or(false),
        }))
        .into_response(),
        Err(e) => supervisor_error_response(e, "headroom_failed"),
    }
}

async fn stop(State(s): State<AppState>) -> Response {
    match process_supervisor::global()
        .stop(&s.db, HEADROOM_PROCESS_ID, Duration::from_secs(2))
        .await
    {
        Ok(Some(pid)) => Json(json!({ "stopped": true, "pid": pid })).into_response(),
        Ok(None) => (
            StatusCode::CONFLICT,
            Json(json!({ "stopped": false, "reason": "not_running" })),
        )
            .into_response(),
        Err(e) => supervisor_error_response(e, "STOP_FAILED"),
    }
}

async fn restart(State(s): State<AppState>) -> Response {
    let (url, code_aware, kompress) = headroom_settings(&s);
    if !is_loopback_url(&url) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "External Headroom proxies must be started outside 9Router",
                "code": "EXTERNAL_PROXY",
            })),
        )
            .into_response();
    }
    let port = parse_port(&url);
    let cmd = build_headroom_cmd(port, code_aware, kompress);
    let log_path = headroom_dir().join("proxy.log");
    match process_supervisor::global()
        .restart(
            &s.db,
            HEADROOM_PROCESS_ID,
            cmd,
            Some(&log_path),
            Duration::from_secs(3),
        )
        .await
    {
        Ok(rec) => Json(json!({
            "success": true,
            "pid": rec.pid,
            "alreadyRunning": false,
        }))
        .into_response(),
        Err(e) => supervisor_error_response(e, "headroom_failed"),
    }
}

fn read_install_log_tail(max_lines: usize) -> String {
    let path = headroom_dir().join("install.log");
    let Ok(content) = std::fs::read_to_string(path) else {
        return String::new();
    };
    let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
    let start = lines.len().saturating_sub(max_lines);
    lines[start..].join("\n")
}

fn parse_valid_extras(body: &Value) -> Result<Vec<&'static str>, Response> {
    let Some(arr) = body["extras"].as_array() else {
        return Ok(vec![]);
    };
    let mut out = Vec::new();
    for item in arr {
        let Some(s) = item.as_str() else {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Invalid extras", "code": "INVALID_EXTRAS" })),
            )
                .into_response());
        };
        let Some(&known) = HEADROOM_EXTRAS.iter().find(|&&k| k == s) else {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("Unsupported extra: {s}"), "code": "INVALID_EXTRAS" })),
            )
                .into_response());
        };
        if !out.contains(&known) {
            out.push(known);
        }
    }
    Ok(out)
}

async fn extras_get(Query(params): Query<HashMap<String, String>>) -> Response {
    if params.get("log").is_some_and(|v| v == "1") {
        return Json(json!({ "log": read_install_log_tail(15) })).into_response();
    }
    let python_ok = process_supervisor::ProcessSupervisor::run_once(
        FixedCommand::new("python3", vec!["--version".to_owned()]),
        None,
        None,
        Duration::from_secs(2),
    )
    .await
    .is_ok_and(|o| o.status.success());

    Json(json!({
        "available": HEADROOM_EXTRAS,
        "installed": [],
        "python": if python_ok { Value::String("python3".to_owned()) } else { Value::Null },
    }))
    .into_response()
}

async fn extras_post(body: Option<Json<Value>>) -> Response {
    let body = body.map(|b| b.0).unwrap_or_else(|| json!({}));
    let requested = match parse_valid_extras(&body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let mut spec_parts = vec!["proxy"];
    spec_parts.extend(requested.iter().copied());
    let spec = format!("headroom-ai[{}]", spec_parts.join(","));
    let cmd = FixedCommand::new(
        "python3",
        vec![
            "-m".to_owned(),
            "pip".to_owned(),
            "install".to_owned(),
            "--upgrade".to_owned(),
            spec.clone(),
        ],
    );
    let log_path = headroom_dir().join("install.log");
    match process_supervisor::ProcessSupervisor::run_once(
        cmd,
        None,
        Some(&log_path),
        Duration::from_secs(300),
    )
    .await
    {
        Ok(out) if out.status.success() => Json(json!({
            "success": true,
            "code": out.status.code().unwrap_or(0),
            "spec": spec,
            "extras": requested,
        }))
        .into_response(),
        Ok(out) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "error": format!("pip install exited with code={:?}", out.status.code()),
                "code": "INSTALL_FAILED",
            })),
        )
            .into_response(),
        Err(SupervisorError::NotInstalled(msg)) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": msg, "code": "NO_PYTHON" })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string(), "code": "headroom_extras_failed" })),
        )
            .into_response(),
    }
}

async fn extras_delete(body: Option<Json<Value>>) -> Response {
    let body = body.map(|b| b.0).unwrap_or_else(|| json!({}));
    let requested = match parse_valid_extras(&body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let mut pkgs: Vec<String> = Vec::new();
    for extra in &requested {
        match *extra {
            "code" => {
                pkgs.push("tree-sitter".to_owned());
                pkgs.push("tree-sitter-language-pack".to_owned());
            }
            "ml" => {
                pkgs.push("torch".to_owned());
                pkgs.push("huggingface-hub".to_owned());
            }
            _ => {}
        }
    }
    if pkgs.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "No valid extras to remove", "code": "INVALID_EXTRAS" })),
        )
            .into_response();
    }
    let mut args = vec![
        "-m".to_owned(),
        "pip".to_owned(),
        "uninstall".to_owned(),
        "-y".to_owned(),
    ];
    args.extend(pkgs.clone());
    let cmd = FixedCommand::new("python3", args);
    let log_path = headroom_dir().join("install.log");
    match process_supervisor::ProcessSupervisor::run_once(
        cmd,
        None,
        Some(&log_path),
        Duration::from_secs(120),
    )
    .await
    {
        Ok(out) if out.status.success() => Json(json!({
            "success": true,
            "code": out.status.code().unwrap_or(0),
            "removed": pkgs,
            "extras": requested,
        }))
        .into_response(),
        Ok(out) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "error": format!("pip uninstall exited with code={:?}", out.status.code()),
                "code": "UNINSTALL_FAILED",
            })),
        )
            .into_response(),
        Err(SupervisorError::NotInstalled(msg)) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": msg, "code": "NO_PYTHON" })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string(), "code": "headroom_extras_failed" })),
        )
            .into_response(),
    }
}

async fn status(State(s): State<AppState>) -> Response {
    let url = headroom_url(&s);
    let health_url = format!("{}/health", url.trim_end_matches('/'));
    let healthy = match s
        .executor
        .0
        .get(&health_url)
        .timeout(Duration::from_secs(2))
        .send()
        .await
    {
        Ok(r) => r.status().is_success(),
        Err(_) => false,
    };
    let rec = process_supervisor::global()
        .status(&s.db, HEADROOM_PROCESS_ID)
        .await;
    Json(json!({
        "running": healthy || rec.status == "running",
        "healthy": healthy,
        "url": url,
        "managedPid": rec.pid,
    }))
    .into_response()
}

async fn proxy_root(State(s): State<AppState>, req: Request) -> Response {
    proxy_inner(s, String::new(), req).await
}

async fn proxy(
    State(s): State<AppState>,
    Path(path): Path<String>,
    req: Request,
) -> Response {
    proxy_inner(s, path, req).await
}

fn is_hop_by_hop(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "host"
    )
}

fn is_loopback_url(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    matches!(
        parsed.host_str().map(|h| h.trim_matches(|c| c == '[' || c == ']')),
        Some("localhost" | "127.0.0.1" | "::1" | "0.0.0.0")
    )
}

async fn proxy_inner(s: AppState, path: String, req: Request) -> Response {
    let base = headroom_url(&s);
    let Ok(mut target) = reqwest::Url::parse(&base) else {
        return (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": "Invalid headroomUrl" })),
        )
            .into_response();
    };
    if !matches!(target.scheme(), "http" | "https") {
        return (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": "Headroom URL must use http or https" })),
        )
            .into_response();
    }
    target.set_path(&format!("/{}", path.trim_start_matches('/')));
    target.set_query(req.uri().query());

    let method = req.method().clone();
    let loopback = is_loopback_url(target.as_str());
    let mut fwd_headers = HeaderMap::new();
    for (k, v) in req.headers() {
        let lower = k.as_str().to_ascii_lowercase();
        if is_hop_by_hop(&lower) {
            continue;
        }
        if !loopback && (lower == "cookie" || lower == "authorization") {
            continue;
        }
        fwd_headers.insert(k.clone(), v.clone());
    }

    let body_bytes = match axum::body::to_bytes(req.into_body(), 10 * 1024 * 1024).await {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": e.to_string() })),
            )
                .into_response();
        }
    };

    let mut builder = s.executor.0.request(method.clone(), target).headers(fwd_headers);
    if method != axum::http::Method::GET && method != axum::http::Method::HEAD {
        builder = builder.body(body_bytes);
    }

    let res = match builder.send().await {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "error": format!("Headroom proxy request failed: {e}") })),
            )
                .into_response();
        }
    };

    let status = res.status();
    let mut out_headers = HeaderMap::new();
    let mut is_html = false;
    for (k, v) in res.headers() {
        let lower = k.as_str().to_ascii_lowercase();
        if is_hop_by_hop(&lower) {
            continue;
        }
        if lower == "content-type" && v.to_str().unwrap_or("").contains("text/html") {
            is_html = true;
        }
        if let Ok(name) = HeaderName::from_bytes(k.as_str().as_bytes()) {
            out_headers.insert(name, v.clone());
        }
    }

    let bytes = res.bytes().await.unwrap_or_default();
    if path == "dashboard" && is_html {
        out_headers.remove("content-length");
        let html = String::from_utf8_lossy(&bytes)
            .replace("fetch('/stats", "fetch('/api/headroom/proxy/stats")
            .replace("fetch('/health", "fetch('/api/headroom/proxy/health")
            .replace(
                "fetch('/transformations/feed",
                "fetch('/api/headroom/proxy/transformations/feed",
            );
        return (status, out_headers, Body::from(html)).into_response();
    }

    (status, out_headers, Body::from(bytes)).into_response()
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

    fn test_app_with_db(headroom_url: Option<&str>) -> Router {
        let db = Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        if let Some(u) = headroom_url {
            crate::db::settings_put(&db, &json!({ "headroomUrl": u })).unwrap();
        }
        let state = AppState {
            db: Arc::new(Mutex::new(db)),
            executor: Arc::new(HttpExecutor(reqwest::Client::new())),
            jwt_secret: None,
        };
        router().with_state(state)
    }

    #[tokio::test]
    async fn headroom_process_routes_validate_and_manage_lifecycle() {
        let ext_app = test_app_with_db(Some("https://headroom.example.com"));
        for path in ["/api/headroom/start", "/api/headroom/restart"] {
            let r = ext_app
                .clone()
                .oneshot(Request::post(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(r.status(), 400);
            let body: Value =
                serde_json::from_slice(&to_bytes(r.into_body(), 4096).await.unwrap()).unwrap();
            assert_eq!(body["code"], "EXTERNAL_PROXY");
        }

        let app = test_app_with_db(None);
        let r = app
            .clone()
            .oneshot(Request::post("/api/headroom/stop").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(r.status(), 409);
        let body: Value =
            serde_json::from_slice(&to_bytes(r.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(body["stopped"], false);
        assert_eq!(body["reason"], "not_running");

        let r = app
            .clone()
            .oneshot(Request::get("/api/headroom/extras").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let body: Value =
            serde_json::from_slice(&to_bytes(r.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(body["available"], json!(["code", "ml"]));

        let r = app
            .clone()
            .oneshot(
                Request::post("/api/headroom/extras")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"extras":["rm -rf /"]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 400);
        let body: Value =
            serde_json::from_slice(&to_bytes(r.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(body["code"], "INVALID_EXTRAS");

        let r = app
            .oneshot(
                Request::delete("/api/headroom/extras")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"extras":[]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 400);
    }

    #[tokio::test]
    async fn headroom_status_reports_down_when_unreachable() {
        let app = test_app_with_db(Some("http://127.0.0.1:1"));
        let r = app
            .oneshot(Request::get("/api/headroom/status").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let body: Value =
            serde_json::from_slice(&to_bytes(r.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(body["running"], false);
        assert_eq!(body["url"], "http://127.0.0.1:1");
    }
}
