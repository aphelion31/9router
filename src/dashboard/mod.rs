use axum::{
    http::{HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rusqlite::Connection;
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};

const LOGIN_HTML: &str = include_str!("login.html");
const OVERVIEW_HTML: &str = include_str!("overview.html");
const ENDPOINT_HTML: &str = include_str!("endpoint.html");
const PROVIDERS_HTML: &str = include_str!("providers.html");
const COMBOS_HTML: &str = include_str!("combos.html");
const STYLE_CSS: &str = include_str!("style.css");
const APP_JS: &str = include_str!("app.js");

pub async fn login_page() -> Html<&'static str> {
    Html(LOGIN_HTML)
}

pub async fn overview_page() -> Html<&'static str> {
    Html(OVERVIEW_HTML)
}

pub async fn endpoint_page() -> Html<&'static str> {
    Html(ENDPOINT_HTML)
}

pub async fn providers_page() -> Html<&'static str> {
    Html(PROVIDERS_HTML)
}

pub async fn combos_page() -> Html<&'static str> {
    Html(COMBOS_HTML)
}

pub async fn style_css() -> Response {
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/css; charset=utf-8"),
        )],
        STYLE_CSS,
    )
        .into_response()
}

pub async fn app_js() -> Response {
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/javascript; charset=utf-8"),
        )],
        APP_JS,
    )
        .into_response()
}

pub fn issue_auth_token(secret: &[u8]) -> String {
    let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"HS256","typ":"JWT"}"#);
    let exp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        + 86400;
    let payload = URL_SAFE_NO_PAD.encode(format!(r#"{{"exp":{exp}}}"#));
    let input = format!("{header}.{payload}");
    let sig = ring::hmac::sign(
        &ring::hmac::Key::new(ring::hmac::HMAC_SHA256, secret),
        input.as_bytes(),
    );
    format!("{input}.{}", URL_SAFE_NO_PAD.encode(sig.as_ref()))
}

pub fn verify_password(db: &Connection, candidate: &str) -> bool {
    let expected = std::env::var("DASHBOARD_PASSWORD")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            crate::db::kv_get(db, "settings", "password")
                .ok()
                .flatten()
                .filter(|s| !s.is_empty())
        })
        .or_else(|| {
            crate::db::kv_get(db, "settings", "data")
                .ok()
                .flatten()
                .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
                .and_then(|v| {
                    v.get("password")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .filter(|s| !s.is_empty())
        });
    let Some(expected) = expected else {
        return false;
    };
    let a = candidate.as_bytes();
    let b = expected.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b.iter()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub fn auth_cookie_header(token: &str) -> String {
    format!("auth_token={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age=86400")
}

pub fn clear_auth_cookie_header() -> &'static str {
    "auth_token=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0"
}
