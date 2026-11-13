use crate::server::AppState;
use axum::{
    Json, Router,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};

fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({"error":message}))).into_response()
}
// ponytail: external auth, maintenance and lifecycle services are not ported; implement before enabling these endpoints.
async fn unavailable() -> Response {
    error(StatusCode::NOT_IMPLEMENTED, "Service not implemented")
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/init", get(|| async { "Initialized" }))
        .route("/api/locale", post(locale))
        .route("/api/version", get(version))
        .route("/api/version/update", post(unavailable))
        .route("/api/version/shutdown", post(version_shutdown))
        .route("/api/shutdown", post(shutdown))
}
async fn shutdown(headers: axum::http::HeaderMap) -> Response {
    if std::env::var("NODE_ENV").as_deref() == Ok("production") {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"success":false,"message":"Not allowed in production"})),
        )
            .into_response();
    }
    let secret = std::env::var("SHUTDOWN_SECRET")
        .ok()
        .filter(|s| !s.is_empty());
    if !secret.is_some_and(|s| {
        headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            == Some(format!("Bearer {s}").as_str())
    }) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"success":false,"message":"Unauthorized"})),
        )
            .into_response();
    }
    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        crate::server::shutdown_notify().notify_one();
    });
    Json(json!({"success":true,"message":"Shutting down..."})).into_response()
}
async fn locale(Json(body): Json<Value>) -> Response {
    let locales = [
        "en", "vi", "zh-CN", "zh-TW", "ja", "pt-BR", "pt-PT", "ko", "es", "de", "fr", "he", "ar",
        "ru", "pl", "cs", "nl", "tr", "uk", "tl", "id", "km", "th", "hi", "bn", "ur", "ro", "sv",
        "it", "el", "hu", "fi", "da", "no", "fa",
    ];
    let Some(locale) = body["locale"].as_str().filter(|v| locales.contains(v)) else {
        return error(StatusCode::BAD_REQUEST, "Invalid locale");
    };
    (
        [(
            header::SET_COOKIE,
            format!("locale={locale}; Path=/; Max-Age=31536000"),
        )],
        Json(json!({"success":true,"locale":locale})),
    )
        .into_response()
}
fn parse_version(v: &str) -> Option<([u64; 3], u64)> {
    let v = v.trim().trim_start_matches('v');
    let (core, pro) = match v.split_once("-pro.") {
        Some((c, p)) => (
            c,
            p.chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse::<u64>()
                .unwrap_or(0),
        ),
        None => (v.split(['-', '+']).next().unwrap_or(v), 0),
    };
    let mut parts = core.split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts.next()?.parse::<u64>().ok()?;
    let patch_str: String = parts
        .next()?
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    Some(([major, minor, patch_str.parse::<u64>().ok()?], pro))
}
fn compare_versions(a: &str, b: &str) -> i8 {
    let (Some(pa), Some(pb)) = (parse_version(a), parse_version(b)) else {
        return 0;
    };
    for i in 0..3 {
        if pa.0[i] > pb.0[i] {
            return 1;
        }
        if pa.0[i] < pb.0[i] {
            return -1;
        }
    }
    if pa.1 > pb.1 {
        1
    } else if pa.1 < pb.1 {
        -1
    } else {
        0
    }
}
static VERSION_CACHE: std::sync::OnceLock<
    tokio::sync::Mutex<(Option<String>, Option<std::time::Instant>)>,
> = std::sync::OnceLock::new();
async fn get_latest_version_cached() -> Option<String> {
    let cache = VERSION_CACHE.get_or_init(|| tokio::sync::Mutex::new((None, None)));
    {
        let guard = cache.lock().await;
        if let (Some(val), Some(at)) = (&guard.0, guard.1) {
            if at.elapsed() < std::time::Duration::from_secs(3600) {
                return Some(val.clone());
            }
        }
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(4))
        .build()
        .ok()?;
    let resp = client
        .get("https://api.github.com/repos/giienew/9router-pro/releases/latest")
        .header("User-Agent", "9router-pro")
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .ok()?;
    let bytes = resp.bytes().await.ok()?;
    let json: Value = serde_json::from_slice(&bytes).ok()?;
    let latest = json["tag_name"]
        .as_str()
        .map(|s| s.trim_start_matches('v').to_string())
        .filter(|s| !s.is_empty());
    if let Some(ref v) = latest {
        let mut guard = cache.lock().await;
        *guard = (Some(v.clone()), Some(std::time::Instant::now()));
    }
    latest
}
async fn version() -> Response {
    let current = env!("CARGO_PKG_VERSION");
    let latest = get_latest_version_cached().await;
    let has_update = latest
        .as_deref()
        .is_some_and(|l| compare_versions(l, current) > 0);
    Json(json!({"currentVersion": current, "latestVersion": latest, "hasUpdate": has_update}))
        .into_response()
}
async fn version_shutdown() -> Response {
    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        crate::server::shutdown_notify().notify_one();
    });
    Json(json!({"success": true, "message": "Shutting down for manual update..."})).into_response()
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::routes::test_support::call;
    use axum::body::Body;
    use axum::http::{Method, Request};
    use rusqlite::Connection;
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;
    fn sys_app() -> (Router, Arc<Mutex<Connection>>) {
        let db = Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        let db = Arc::new(Mutex::new(db));
        let state = AppState {
            db: Arc::clone(&db),
            executor: Arc::new(crate::api::executor::HttpExecutor(reqwest::Client::new())),
            jwt_secret: None,
        };
        (routes().with_state(state), db)
    }
    #[tokio::test]
    async fn init_locale_and_shutdown_work() {
        let (app, _) = sys_app();
        let res = app
            .clone()
            .oneshot(Request::get("/api/init").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], b"Initialized");
        let (st, body) = call(
            app.clone(),
            Request::builder()
                .method(Method::POST)
                .uri("/api/locale")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"locale":"vi"}"#))
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(body["success"], true);
        assert_eq!(body["locale"], "vi");
        let (st, _) = call(
            app.clone(),
            Request::builder()
                .method(Method::POST)
                .uri("/api/locale")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"locale":"xx-invalid"}"#))
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        let (st, _) = call(
            app.clone(),
            Request::builder()
                .method(Method::POST)
                .uri("/api/shutdown")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);
        assert_eq!(compare_versions("0.5.104-pro.2", "0.5.104-pro.1"), 1);
        assert_eq!(compare_versions("0.5.104", "0.5.104-pro.1"), -1);
        let (st, body) = call(
            app.clone(),
            Request::get("/api/version").body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(body["currentVersion"], env!("CARGO_PKG_VERSION"));
        let (st, body) = call(
            app,
            Request::builder()
                .method(Method::POST)
                .uri("/api/version/shutdown")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(body["success"], true);
    }
}
