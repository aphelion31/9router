use axum::{
    Json, Router,
    body::Body,
    extract::{Path, Request, State},
    http::{HeaderMap, HeaderName, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, get, post},
};
use serde_json::json;

use crate::server::AppState;

const DEFAULT_HEADROOM_URL: &str = "http://127.0.0.1:8787";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/headroom/status", get(status))
        .route("/api/headroom/start", post(not_available))
        .route("/api/headroom/stop", post(not_available))
        .route("/api/headroom/restart", post(not_available))
        .route(
            "/api/headroom/extras",
            get(not_available).post(not_available).delete(not_available),
        )
        .route("/api/headroom/proxy/{*path}", any(proxy))
        .route("/api/headroom/proxy", any(proxy_root))
}

fn headroom_url(state: &AppState) -> String {
    let db = state.db.lock().unwrap();
    crate::db::settings_get(&db)
        .ok()
        .and_then(|v| v["headroomUrl"].as_str().map(str::to_owned))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_HEADROOM_URL.to_owned())
}

// ponytail: headroom start/stop/restart/extras spawn Python/pip child processes; not available in Rust
async fn not_available() -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({ "error": "not available" })),
    )
        .into_response()
}

async fn status(State(s): State<AppState>) -> Response {
    let url = headroom_url(&s);
    let health_url = format!("{}/health", url.trim_end_matches('/'));
    let healthy = match s
        .executor
        .0
        .get(&health_url)
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
    {
        Ok(r) => r.status().is_success(),
        Err(_) => false,
    };
    Json(json!({
        "running": healthy,
        "healthy": healthy,
        "url": url,
        "managedPid": null,
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
        Some("localhost" | "127.0.0.1" | "::1")
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
    async fn headroom_process_routes_return_501() {
        let app = test_app();
        for (method, path) in [
            ("POST", "/api/headroom/start"),
            ("POST", "/api/headroom/stop"),
            ("POST", "/api/headroom/restart"),
            ("GET", "/api/headroom/extras"),
            ("POST", "/api/headroom/extras"),
            ("DELETE", "/api/headroom/extras"),
        ] {
            let r = app
                .clone()
                .oneshot(Request::builder().method(method).uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(r.status(), 501, "expected 501 for {method} {path}");
            let body: Value =
                serde_json::from_slice(&to_bytes(r.into_body(), 4096).await.unwrap()).unwrap();
            assert_eq!(body["error"], "not available");
        }
    }

    #[tokio::test]
    async fn headroom_status_reports_down_when_unreachable() {
        let db = Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        crate::db::settings_put(&db, &json!({ "headroomUrl": "http://127.0.0.1:1" })).unwrap();
        let state = AppState {
            db: Arc::new(Mutex::new(db)),
            executor: Arc::new(HttpExecutor(reqwest::Client::new())),
            jwt_secret: None,
        };
        let r = router()
            .with_state(state)
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
