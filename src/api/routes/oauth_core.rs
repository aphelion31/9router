use crate::server::AppState;
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::{
    digest::{SHA256, digest},
    rand::{SecureRandom, SystemRandom},
};
use rusqlite::{Connection, params};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Clone)]
struct OAuthSession {
    provider: String,
    code_verifier: String,
    redirect_uri: String,
    created_at: Instant,
}

static OAUTH_SESSIONS: OnceLock<Mutex<HashMap<String, OAuthSession>>> = OnceLock::new();

fn sessions() -> &'static Mutex<HashMap<String, OAuthSession>> {
    OAUTH_SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn router() -> Router<AppState> {
    Router::new().route("/api/oauth/{provider}/{action}", get(oauth_get).post(oauth_post))
}

pub fn public_router() -> Router<AppState> {
    Router::new().route("/api/oauth/{provider}/callback", get(oauth_callback))
}

pub(crate) fn random_b64_pub(bytes: usize) -> String {
    random_b64(bytes)
}

pub(crate) fn pkce_challenge_pub(verifier: &str) -> String {
    pkce_challenge(verifier)
}

fn random_b64(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    SystemRandom::new().fill(&mut buf).ok();
    URL_SAFE_NO_PAD.encode(buf)
}

fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(digest(&SHA256, verifier.as_bytes()).as_ref())
}

fn now_iso(db: &Connection) -> String {
    db.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |r| r.get(0))
        .unwrap_or_else(|_| "1970-01-01T00:00:00.000Z".to_string())
}

fn store_session(state: String, session: OAuthSession) {
    if let Ok(mut map) = sessions().lock() {
        map.retain(|_, s| s.created_at.elapsed() < Duration::from_secs(900));
        map.insert(state, session);
    }
}

fn take_verified_session(state: &str, provider: &str) -> Option<OAuthSession> {
    let mut map = sessions().lock().ok()?;
    let sess = map.get(state)?;
    if sess.provider != provider || sess.created_at.elapsed() >= Duration::from_secs(900) {
        return None;
    }
    map.remove(state)
}

pub(crate) fn save_oauth_connection(
    db: &Connection,
    provider: &str,
    auth_type: &str,
    access_token: Option<&str>,
    refresh_token: Option<&str>,
    expires_in: Option<i64>,
    email: Option<&str>,
    display_name: Option<&str>,
    provider_specific: Value,
) -> rusqlite::Result<Value> {
    let id = format!("conn-{}", random_b64(9));
    let now = now_iso(db);
    let expires_at = expires_in.map(|secs| {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64
            + secs;
        ts.to_string()
    });
    let mut data = json!({
        "id": id,
        "provider": provider,
        "authType": auth_type,
        "testStatus": "active",
        "isActive": true,
        "providerSpecificData": provider_specific,
    });
    if let Some(at) = access_token {
        data["accessToken"] = json!(at);
    }
    if let Some(rt) = refresh_token {
        data["refreshToken"] = json!(rt);
    }
    if let Some(ea) = &expires_at {
        data["expiresAt"] = json!(ea);
    }
    if let Some(em) = email {
        data["email"] = json!(em);
    }
    if let Some(dn) = display_name {
        data["displayName"] = json!(dn);
    }

    db.execute(
        "INSERT INTO providerConnections(id, provider, authType, name, email, isActive, createdAt, updatedAt, data)
         VALUES(?1, ?2, ?3, ?4, ?5, 1, ?6, ?6, ?7)",
        params![
            id,
            provider,
            auth_type,
            display_name.unwrap_or(provider),
            email,
            now,
            data.to_string()
        ],
    )?;

    Ok(json!({
        "id": id,
        "provider": provider,
        "email": email,
        "displayName": display_name,
    }))
}

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

async fn oauth_callback(
    State(state_app): State<AppState>,
    Path(provider): Path<String>,
    Query(q): Query<CallbackQuery>,
) -> Response {
    if let Some(err) = q.error {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": err,
                "errorDescription": q.error_description
            })),
        )
            .into_response();
    }
    let Some(state_val) = q.state.as_deref().filter(|s| !s.is_empty()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Missing OAuth state" })),
        )
            .into_response();
    };
    let Some(code) = q.code.as_deref().filter(|s| !s.is_empty()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Missing authorization code" })),
        )
            .into_response();
    };
    let Some(session) = take_verified_session(state_val, &provider) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Invalid or expired OAuth state" })),
        )
            .into_response();
    };
    let Some(cfg) = crate::oauth::provider(&provider) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Unknown or unconfigured OAuth provider" })),
        )
            .into_response();
    };
    let verifier = cfg.pkce.then_some(session.code_verifier.as_str());
    match crate::oauth::exchange_code(&cfg, code, &session.redirect_uri, verifier, state_val).await {
        Ok(tokens) => {
            let db = state_app.db.lock().unwrap();
            match save_oauth_connection(
                &db,
                &provider,
                "oauth",
                Some(&tokens.access_token),
                tokens.refresh_token.as_deref(),
                tokens.expires_in,
                None,
                None,
                json!({}),
            ) {
                Ok(conn) => Json(json!({ "success": true, "connection": conn })).into_response(),
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": e.to_string() })),
                )
                    .into_response(),
            }
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn oauth_get(
    State(_state_app): State<AppState>,
    Path((provider, action)): Path<(String, String)>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    match action.as_str() {
        "authorize" => {
            let Some(cfg) = crate::oauth::provider(&provider) else {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": format!("Unknown or unconfigured OAuth provider: {provider}") })),
                )
                    .into_response();
            };
            let redirect_uri = params
                .get("redirect_uri")
                .cloned()
                .unwrap_or_else(|| "http://localhost:8080/callback".to_string());
            let state = random_b64(24);
            let verifier = random_b64(32);
            let challenge = pkce_challenge(&verifier);
            match crate::oauth::authorization_url(&cfg, &redirect_uri, &state, Some(&challenge)) {
                Ok(authorize_url) => {
                    store_session(
                        state.clone(),
                        OAuthSession {
                            provider,
                            code_verifier: verifier,
                            redirect_uri: redirect_uri.clone(),
                            created_at: Instant::now(),
                        },
                    );
                    Json(json!({
                        "authorizeUrl": authorize_url,
                        "authUrl": authorize_url,
                        "state": state,
                        "codeChallenge": challenge,
                        "redirectUri": redirect_uri,
                    }))
                    .into_response()
                }
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": e.to_string() })),
                )
                    .into_response(),
            }
        }
        "device-code" => {
            let Some(cfg) = crate::oauth::provider(&provider) else {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": format!("Unknown or unconfigured OAuth provider: {provider}") })),
                )
                    .into_response();
            };
            if cfg.device_url.is_none() {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "Provider does not support device code flow" })),
                )
                    .into_response();
            }
            match crate::oauth::device_code(&cfg).await {
                Ok(v) => Json(v).into_response(),
                Err(e) => (
                    StatusCode::BAD_GATEWAY,
                    Json(json!({ "error": e.to_string() })),
                )
                    .into_response(),
            }
        }
        "poll-status" => {
            let Some(st) = params.get("state").filter(|s| !s.is_empty()) else {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "Missing state" })),
                )
                    .into_response();
            };
            let known = sessions()
                .lock()
                .ok()
                .and_then(|m| m.get(st).map(|s| s.provider == provider))
                .unwrap_or(false);
            Json(json!({ "status": if known { "pending" } else { "unknown" } })).into_response()
        }
        "start-proxy" | "stop-proxy" => {
            if !matches!(
                provider.as_str(),
                "codex" | "xai" | "trae" | "windsurf" | "zed" | "xiaomi-mimo"
            ) {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "Proxy only supported for codex/xai/trae/windsurf/zed/xiaomi-mimo" })),
                )
                    .into_response();
            }
            Json(json!({ "success": true })).into_response()
        }
        "ide-status" => {
            if provider != "trae" && provider != "windsurf" {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "ide-status only supported for trae/windsurf" })),
                )
                    .into_response();
            }
            Json(json!({ "installed": false })).into_response()
        }
        "lease-status" => {
            if provider != "minimax-code" && provider != "minimax-code-global" {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "lease-status only supported for minimax-code/minimax-code-global" })),
                )
                    .into_response();
            }
            Json(json!({ "found": false, "error": "UNAVAILABLE" })).into_response()
        }
        _ => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Unknown action" })),
        )
            .into_response(),
    }
}

fn decode_jwt_payload(jwt: &str) -> Option<Value> {
    let part = jwt.split('.').nth(1)?;
    let decoded = URL_SAFE_NO_PAD.decode(part).ok()?;
    serde_json::from_slice(&decoded).ok()
}

async fn oauth_post(
    State(state_app): State<AppState>,
    Path((provider, action)): Path<(String, String)>,
    body: axum::body::Bytes,
) -> Response {
    let Ok(payload) = serde_json::from_slice::<Value>(&body) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Invalid or empty request body" })),
        )
            .into_response();
    };

    match action.as_str() {
        "exchange" => {
            let code = payload.get("code").and_then(Value::as_str).unwrap_or("");
            if code.starts_with("eyJ") && code.contains('.') {
                let claims = decode_jwt_payload(code).unwrap_or(json!({}));
                let email = claims.get("email").and_then(Value::as_str);
                let mut psd = json!({ "authMethod": "access_token" });
                if let Some(acc) = claims.get("account_id").and_then(Value::as_str) {
                    psd["chatgptAccountId"] = json!(acc);
                }
                if let Some(plan) = claims.get("plan_type").and_then(Value::as_str) {
                    psd["chatgptPlanType"] = json!(plan);
                }
                let db = state_app.db.lock().unwrap();
                return match save_oauth_connection(
                    &db,
                    &provider,
                    "access_token",
                    Some(code),
                    None,
                    None,
                    email,
                    None,
                    psd,
                ) {
                    Ok(conn) => Json(json!({ "success": true, "connection": conn })).into_response(),
                    Err(e) => (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({ "error": e.to_string() })),
                    )
                        .into_response(),
                };
            }

            let state_val = payload.get("state").and_then(Value::as_str);
            let session = match state_val {
                Some(st) if !st.is_empty() => match take_verified_session(st, &provider) {
                    Some(s) => Some(s),
                    None => {
                        return (
                            StatusCode::BAD_REQUEST,
                            Json(json!({ "error": "Invalid or expired OAuth state" })),
                        )
                            .into_response();
                    }
                },
                _ => None,
            };

            let redirect_uri = payload
                .get("redirectUri")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| session.as_ref().map(|s| s.redirect_uri.clone()))
                .unwrap_or_default();
            let verifier = session
                .as_ref()
                .map(|s| s.code_verifier.clone())
                .or_else(|| {
                    payload
                        .get("codeVerifier")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                });
            if code.is_empty() || redirect_uri.is_empty() {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "Missing required fields" })),
                )
                    .into_response();
            }
            let Some(cfg) = crate::oauth::provider(&provider) else {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": format!("Unknown or unconfigured OAuth provider: {provider}") })),
                )
                    .into_response();
            };
            if cfg.pkce && verifier.as_deref().unwrap_or("").is_empty() {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "Missing required fields" })),
                )
                    .into_response();
            }
            let state_str = state_val.unwrap_or("");
            match crate::oauth::exchange_code(&cfg, code, &redirect_uri, verifier.as_deref(), state_str).await
            {
                Ok(tokens) => {
                    let db = state_app.db.lock().unwrap();
                    match save_oauth_connection(
                        &db,
                        &provider,
                        "oauth",
                        Some(&tokens.access_token),
                        tokens.refresh_token.as_deref(),
                        tokens.expires_in,
                        None,
                        None,
                        json!({}),
                    ) {
                        Ok(conn) => {
                            Json(json!({ "success": true, "connection": conn })).into_response()
                        }
                        Err(e) => (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            Json(json!({ "error": e.to_string() })),
                        )
                            .into_response(),
                    }
                }
                Err(e) => (
                    StatusCode::BAD_GATEWAY,
                    Json(json!({ "error": e.to_string() })),
                )
                    .into_response(),
            }
        }
        "poll" => {
            let device_code = payload.get("deviceCode").and_then(Value::as_str).unwrap_or("");
            if device_code.is_empty() {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "Missing device code" })),
                )
                    .into_response();
            }
            let Some(cfg) = crate::oauth::provider(&provider) else {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": format!("Unknown or unconfigured OAuth provider: {provider}") })),
                )
                    .into_response();
            };
            match crate::oauth::poll_device(&cfg, device_code).await {
                Ok(tokens) => {
                    let pid = if provider == "kimi-coding" {
                        "kimi"
                    } else {
                        &provider
                    };
                    let db = state_app.db.lock().unwrap();
                    match save_oauth_connection(
                        &db,
                        pid,
                        "oauth",
                        Some(&tokens.access_token),
                        tokens.refresh_token.as_deref(),
                        tokens.expires_in,
                        None,
                        None,
                        json!({}),
                    ) {
                        Ok(conn) => {
                            Json(json!({ "success": true, "connection": conn })).into_response()
                        }
                        Err(e) => (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            Json(json!({ "error": e.to_string() })),
                        )
                            .into_response(),
                    }
                }
                Err(e) => {
                    let msg = e.to_string();
                    let pending = msg.contains("authorization_pending") || msg.contains("slow_down");
                    Json(json!({
                        "success": false,
                        "error": msg,
                        "pending": pending
                    }))
                    .into_response()
                }
            }
        }
        "register-session" => {
            if !matches!(provider.as_str(), "trae" | "windsurf" | "zed") {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "register-session only supported for trae/windsurf/zed" })),
                )
                    .into_response();
            }
            let st = payload.get("state").and_then(Value::as_str).unwrap_or("");
            if st.is_empty() {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "Missing state" })),
                )
                    .into_response();
            }
            Json(json!({ "success": true })).into_response()
        }
        "lease-connect" => {
            if provider != "minimax-code" && provider != "minimax-code-global" {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "lease-connect only supported for minimax-code/minimax-code-global" })),
                )
                    .into_response();
            }
            (
                StatusCode::CONFLICT,
                Json(json!({ "error": "MiniMax desktop lease broker unavailable", "code": "UNAVAILABLE" })),
            )
                .into_response()
        }
        "manual-code" => {
            if provider != "xai" {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "Manual code only supported for xai" })),
                )
                    .into_response();
            }
            (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "xAI OAuth session not found; restart the login flow and paste the code again" })),
            )
                .into_response()
        }
        _ => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Unknown action" })),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::executor::HttpExecutor;
    use axum::{body::Body, body::to_bytes, http::Request};
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;

    fn test_app() -> (Router, Arc<Mutex<Connection>>) {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        let db = Arc::new(Mutex::new(conn));
        let state = AppState {
            db: Arc::clone(&db),
            executor: Arc::new(HttpExecutor(reqwest::Client::new())),
            jwt_secret: None,
        };
        let app = Router::new()
            .merge(public_router())
            .merge(router())
            .with_state(state);
        (app, db)
    }

    #[tokio::test]
    async fn authorize_generates_state_and_callback_rejects_mismatch() {
        let (app, _db) = test_app();
        let res = app
            .clone()
            .oneshot(
                Request::get("/api/oauth/codex/authorize?redirect_uri=http://localhost:8080/cb")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v: Value =
            serde_json::from_slice(&to_bytes(res.into_body(), 65536).await.unwrap()).unwrap();
        assert!(v["authorizeUrl"].as_str().unwrap().contains("code_challenge="));
        assert!(!v["state"].as_str().unwrap().is_empty());

        let bad_cb = app
            .clone()
            .oneshot(
                Request::get("/api/oauth/codex/callback?code=abc&state=wrong-state")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bad_cb.status(), StatusCode::BAD_REQUEST);

        let bad_ex = app
            .oneshot(
                Request::post("/api/oauth/codex/exchange")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"code":"abc","state":"wrong-state"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bad_ex.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn exchange_jwt_creates_access_token_connection() {
        let (app, db) = test_app();
        let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#);
        let payload = URL_SAFE_NO_PAD.encode(r#"{"email":"u@example.com","account_id":"acc-1"}"#);
        let jwt = format!("{header}.{payload}.sig");
        let body = json!({ "code": jwt }).to_string();

        let res = app
            .oneshot(
                Request::post("/api/oauth/codex/exchange")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v: Value =
            serde_json::from_slice(&to_bytes(res.into_body(), 65536).await.unwrap()).unwrap();
        assert_eq!(v["success"], true);
        assert_eq!(v["connection"]["email"], "u@example.com");

        let count: i64 = db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM providerConnections", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }
}
