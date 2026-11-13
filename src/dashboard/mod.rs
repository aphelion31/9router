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
const PROVIDERS_NEW_HTML: &str = include_str!("providers_new.html");
const PROVIDER_DETAIL_HTML: &str = include_str!("provider_detail.html");
const COMBOS_HTML: &str = include_str!("combos.html");
const USAGE_HTML: &str = include_str!("usage.html");
const QUOTA_HTML: &str = include_str!("quota.html");
const TOKEN_SAVER_HTML: &str = include_str!("token_saver.html");
const PROMPT_INJECTION_HTML: &str = include_str!("prompt_injection.html");
const PROFILE_HTML: &str = include_str!("profile.html");
const SKILLS_HTML: &str = include_str!("skills.html");
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

pub async fn providers_new_page() -> Html<&'static str> {
    Html(PROVIDERS_NEW_HTML)
}

pub async fn provider_detail_page() -> Html<&'static str> {
    Html(PROVIDER_DETAIL_HTML)
}

pub async fn combos_page() -> Html<&'static str> {
    Html(COMBOS_HTML)
}

pub async fn usage_page() -> Html<&'static str> {
    Html(USAGE_HTML)
}

pub async fn quota_page() -> Html<&'static str> {
    Html(QUOTA_HTML)
}

pub async fn token_saver_page() -> Html<&'static str> {
    Html(TOKEN_SAVER_HTML)
}

pub async fn prompt_injection_page() -> Html<&'static str> {
    Html(PROMPT_INJECTION_HTML)
}

pub async fn profile_page() -> Html<&'static str> {
    Html(PROFILE_HTML)
}

pub async fn skills_page() -> Html<&'static str> {
    Html(SKILLS_HTML)
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
    // Upstream 9router-pro stores bcrypt hashes; env/plain values compare in constant time.
    if expected.starts_with("$2") {
        return bcrypt::verify(candidate, &expected).unwrap_or(false);
    }
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

#[cfg(test)]
mod bcrypt_tests {
    #[test]
    fn verifies_upstream_bcrypt_hash() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        let hash = bcrypt::hash("s3cret", 4).unwrap();
        crate::db::kv_set(&db, "settings", "password", &hash).unwrap();
        assert!(super::verify_password(&db, "s3cret"));
        assert!(!super::verify_password(&db, "wrong"));
    }
}
