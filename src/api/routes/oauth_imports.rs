use crate::{
    api::routes::oauth_core::{pkce_challenge_pub, random_b64_pub, save_oauth_connection},
    server::AppState,
};
use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::path::PathBuf;

const MAX_BULK_IMPORT_ITEMS: usize = 50;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/oauth/codex/bulk-import", post(codex_bulk_import))
        .route("/api/oauth/codex/import-token", post(codex_import_token))
        .route("/api/oauth/cursor/auto-import", get(cursor_auto_import))
        .route(
            "/api/oauth/cursor/import",
            get(cursor_import_get).post(cursor_import_post),
        )
        .route("/api/oauth/gitlab/pat", post(gitlab_pat))
        .route("/api/oauth/grok-cli/bulk-import", post(grok_cli_bulk_import))
        .route("/api/oauth/iflow/cookie", post(iflow_cookie))
        .route("/api/oauth/kiro/api-key", post(kiro_api_key))
        .route("/api/oauth/kiro/auto-import", get(kiro_auto_import))
        .route("/api/oauth/kiro/import", post(kiro_import))
        .route(
            "/api/oauth/kiro/import-cli-proxy",
            post(kiro_import_cli_proxy),
        )
        .route("/api/oauth/kiro/social-authorize", get(kiro_social_authorize))
        .route("/api/oauth/kiro/social-exchange", post(kiro_social_exchange))
        .route("/api/oauth/xiaomi-mimo/api-key", post(xiaomi_mimo_api_key))
        .route(
            "/api/oauth/xiaomi-mimo/auto-import",
            get(xiaomi_mimo_auto_import),
        )
        .route(
            "/api/oauth/xiaomi-mimo/login/start",
            post(xiaomi_mimo_login_start),
        )
        .route(
            "/api/oauth/xiaomi-mimo/login/status",
            get(xiaomi_mimo_login_status),
        )
        .route("/api/oauth/zed/auto-import", get(zed_auto_import))
        .route("/api/oauth/zed/import", post(zed_import))
}

fn decode_jwt_claims(token: &str) -> Value {
    let Some(part) = token.split('.').nth(1) else {
        return json!({});
    };
    URL_SAFE_NO_PAD
        .decode(part)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| json!({}))
}

fn normalize_bulk_accounts(body: Value) -> Option<Vec<Value>> {
    match body {
        Value::Array(arr) => Some(arr),
        Value::Object(ref map) => {
            if let Some(Value::Array(arr)) = map.get("accounts") {
                Some(arr.clone())
            } else {
                Some(vec![body])
            }
        }
        _ => None,
    }
}

async fn codex_bulk_import(
    State(state): State<AppState>,
    body: axum::body::Bytes,
) -> Response {
    let Ok(parsed) = serde_json::from_slice::<Value>(&body) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Invalid JSON body" })),
        )
            .into_response();
    };
    let Some(accounts) = normalize_bulk_accounts(parsed).filter(|a| !a.is_empty()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "No accounts provided" })),
        )
            .into_response();
    };
    if accounts.len() > MAX_BULK_IMPORT_ITEMS {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("Bulk import is limited to {MAX_BULK_IMPORT_ITEMS} accounts per request") })),
        )
            .into_response();
    }

    let db = state.db.lock().unwrap();
    let mut results = Vec::with_capacity(accounts.len());
    let mut success = 0usize;
    let mut failed = 0usize;

    for (i, item) in accounts.iter().enumerate() {
        let Some(obj) = item.as_object() else {
            failed += 1;
            results.push(json!({ "index": i, "ok": false, "error": "Item is not an object" }));
            continue;
        };
        let Some(access_token) = obj
            .get("accessToken")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            failed += 1;
            results.push(json!({ "index": i, "ok": false, "error": "Missing accessToken" }));
            continue;
        };
        let id_or_access = obj
            .get("idToken")
            .and_then(Value::as_str)
            .unwrap_or(access_token);
        let claims = decode_jwt_claims(id_or_access);
        let email = obj
            .get("email")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                claims
                    .pointer("/https:~1~1api.openai.com~1profile/email")
                    .or_else(|| claims.get("email"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            });
        let mut psd = obj
            .get("providerSpecificData")
            .cloned()
            .unwrap_or_else(|| json!({}));
        if psd.get("chatgptAccountId").is_none()
            && let Some(acc) = claims
                .pointer("/https:~1~1api.openai.com~1auth/chatgpt_account_id")
                .or_else(|| claims.get("account_id"))
                .and_then(Value::as_str)
        {
            psd["chatgptAccountId"] = json!(acc);
        }
        let refresh = obj.get("refreshToken").and_then(Value::as_str);
        let expires_in = obj.get("expiresIn").and_then(Value::as_i64);
        match save_oauth_connection(
            &db,
            "codex",
            "oauth",
            Some(access_token),
            refresh,
            expires_in,
            email.as_deref(),
            obj.get("displayName").and_then(Value::as_str),
            psd,
        ) {
            Ok(conn) => {
                success += 1;
                results.push(json!({ "index": i, "ok": true, "id": conn["id"], "email": conn["email"] }));
            }
            Err(e) => {
                failed += 1;
                results.push(json!({ "index": i, "ok": false, "error": e.to_string() }));
            }
        }
    }

    Json(json!({
        "total": accounts.len(),
        "success": success,
        "failed": failed,
        "results": results,
    }))
    .into_response()
}

async fn codex_import_token(
    State(state): State<AppState>,
    body: axum::body::Bytes,
) -> Response {
    let Ok(payload) = serde_json::from_slice::<Value>(&body) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Invalid JSON body" })),
        )
            .into_response();
    };
    let Some(token) = payload
        .get("accessToken")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Access token is required" })),
        )
            .into_response();
    };
    let claims = decode_jwt_claims(token);
    let email = claims
        .pointer("/https:~1~1api.openai.com~1profile/email")
        .or_else(|| claims.get("email"))
        .or_else(|| claims.get("preferred_username"))
        .and_then(Value::as_str);
    let account_id = claims
        .pointer("/https:~1~1api.openai.com~1auth/chatgpt_account_id")
        .or_else(|| claims.get("account_id"))
        .and_then(Value::as_str);
    let plan = claims
        .pointer("/https:~1~1api.openai.com~1auth/chatgpt_plan_type")
        .or_else(|| claims.get("plan_type"))
        .and_then(Value::as_str);

    let mut psd = json!({ "authMethod": "access_token" });
    if let Some(a) = account_id {
        psd["chatgptAccountId"] = json!(a);
    }
    if let Some(p) = plan {
        psd["chatgptPlanType"] = json!(p);
    }
    let name = payload
        .get("name")
        .and_then(Value::as_str)
        .or(email)
        .unwrap_or("ChatGPT Access Token");

    let db = state.db.lock().unwrap();
    match save_oauth_connection(
        &db,
        "codex",
        "access_token",
        Some(token),
        None,
        None,
        email,
        Some(name),
        psd,
    ) {
        Ok(conn) => Json(json!({
            "success": true,
            "connection": {
                "id": conn["id"],
                "provider": "codex",
                "email": email,
                "name": name,
                "workspace": account_id,
                "plan": plan,
            }
        }))
        .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn cursor_auto_import() -> impl IntoResponse {
    let home = std::env::var("HOME").unwrap_or_default();
    let candidates = [
        PathBuf::from(&home).join("Library/Application Support/Cursor/User/globalStorage/state.vscdb"),
        PathBuf::from(&home).join(".config/Cursor/User/globalStorage/state.vscdb"),
    ];
    for p in candidates {
        if p.exists()
            && let Ok(db) = Connection::open_with_flags(&p, OpenFlags::SQLITE_OPEN_READ_ONLY)
        {
            let access_token: Option<String> = db
                .query_row(
                    "SELECT value FROM itemTable WHERE key IN ('cursorAuth/accessToken','cursorAuth/token') LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .ok();
            let machine_id: Option<String> = db
                .query_row(
                    "SELECT value FROM itemTable WHERE key IN ('storage.serviceMachineId','storage.machineId','telemetry.machineId') LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .ok();
            if let (Some(at), Some(mid)) = (access_token, machine_id) {
                return Json(json!({
                    "found": true,
                    "accessToken": at.trim_matches('"'),
                    "machineId": mid.trim_matches('"'),
                }));
            }
        }
    }
    Json(json!({
        "found": false,
        "error": "Cursor IDE database not found or not signed in."
    }))
}

async fn cursor_import_get() -> impl IntoResponse {
    Json(json!({
        "provider": "cursor",
        "method": "import_token",
        "instructions": "Extract cursorAuth/accessToken and storage.serviceMachineId from Cursor state.vscdb",
        "requiredFields": [
            { "name": "accessToken", "label": "Access Token", "description": "From cursorAuth/accessToken in state.vscdb", "type": "textarea" },
            { "name": "machineId", "label": "Machine ID", "description": "From storage.serviceMachineId in state.vscdb", "type": "text" },
        ]
    }))
}

async fn cursor_import_post(
    State(state): State<AppState>,
    body: axum::body::Bytes,
) -> Response {
    let Ok(payload) = serde_json::from_slice::<Value>(&body) else {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "Invalid JSON body" }))).into_response();
    };
    let Some(access_token) = payload.get("accessToken").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) else {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "Access token is required" }))).into_response();
    };
    let Some(machine_id) = payload.get("machineId").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) else {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "Machine ID is required" }))).into_response();
    };
    let claims = decode_jwt_claims(access_token);
    let email = claims.get("email").and_then(Value::as_str);
    let db = state.db.lock().unwrap();
    match save_oauth_connection(
        &db,
        "cursor",
        "oauth",
        Some(access_token),
        None,
        Some(365 * 24 * 3600),
        email,
        None,
        json!({ "machineId": machine_id, "authMethod": "imported", "provider": "Imported" }),
    ) {
        Ok(conn) => Json(json!({ "success": true, "connection": { "id": conn["id"], "provider": "cursor", "email": email } })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

async fn gitlab_pat(
    State(state): State<AppState>,
    body: axum::body::Bytes,
) -> Response {
    let Ok(payload) = serde_json::from_slice::<Value>(&body) else {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "Invalid request body" }))).into_response();
    };
    let Some(token) = payload.get("token").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) else {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "Personal Access Token is required" }))).into_response();
    };
    let base = payload
        .get("baseUrl")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("https://gitlab.com")
        .trim_end_matches('/');

    let Ok(resp) = state
        .executor
        .0
        .get(format!("{base}/api/v4/user"))
        .header("Private-Token", token)
        .header("Accept", "application/json")
        .send()
        .await
    else {
        return (StatusCode::BAD_GATEWAY, Json(json!({ "error": "GitLab request failed" }))).into_response();
    };
    if !resp.status().is_success() {
        return (StatusCode::UNAUTHORIZED, Json(json!({ "error": "GitLab token verification failed" }))).into_response();
    }
    let user: Value = resp
        .bytes()
        .await
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| json!({}));
    let email = user
        .get("email")
        .or_else(|| user.get("public_email"))
        .and_then(Value::as_str);
    let display_name = user
        .get("name")
        .or_else(|| user.get("username"))
        .and_then(Value::as_str);

    let db = state.db.lock().unwrap();
    match save_oauth_connection(
        &db,
        "gitlab",
        "oauth",
        Some(token),
        None,
        None,
        email,
        display_name,
        json!({ "baseUrl": base, "authKind": "personal_access_token" }),
    ) {
        Ok(_) => Json(json!({ "success": true })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

async fn grok_cli_bulk_import(
    State(state): State<AppState>,
    body: axum::body::Bytes,
) -> Response {
    let Ok(parsed) = serde_json::from_slice::<Value>(&body) else {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "Invalid JSON body" }))).into_response();
    };
    let Some(accounts) = normalize_bulk_accounts(parsed).filter(|a| !a.is_empty()) else {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "No accounts provided" }))).into_response();
    };
    if accounts.len() > MAX_BULK_IMPORT_ITEMS {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("Bulk import is limited to {MAX_BULK_IMPORT_ITEMS} accounts per request") })),
        )
            .into_response();
    }
    let db = state.db.lock().unwrap();
    let mut results = Vec::with_capacity(accounts.len());
    let mut success = 0usize;
    let mut failed = 0usize;
    for (i, item) in accounts.iter().enumerate() {
        let Some(obj) = item.as_object() else {
            failed += 1;
            results.push(json!({ "index": i, "ok": false, "error": "Item is not an object" }));
            continue;
        };
        let Some(at) = obj
            .get("access_token")
            .or_else(|| obj.get("accessToken"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            failed += 1;
            results.push(json!({ "index": i, "ok": false, "error": "Missing access_token / accessToken" }));
            continue;
        };
        let rt = obj
            .get("refresh_token")
            .or_else(|| obj.get("refreshToken"))
            .and_then(Value::as_str);
        let email = obj.get("email").and_then(Value::as_str);
        match save_oauth_connection(
            &db,
            "grok-cli",
            "oauth",
            Some(at),
            rt,
            obj.get("expires_in").or_else(|| obj.get("expiresIn")).and_then(Value::as_i64),
            email,
            obj.get("displayName").or_else(|| obj.get("name")).and_then(Value::as_str),
            json!({ "authMethod": "device_code" }),
        ) {
            Ok(conn) => {
                success += 1;
                results.push(json!({ "index": i, "ok": true, "id": conn["id"], "email": conn["email"] }));
            }
            Err(e) => {
                failed += 1;
                results.push(json!({ "index": i, "ok": false, "error": e.to_string() }));
            }
        }
    }
    Json(json!({ "total": accounts.len(), "success": success, "failed": failed, "results": results })).into_response()
}

async fn iflow_cookie(
    State(state): State<AppState>,
    body: axum::body::Bytes,
) -> Response {
    let Ok(payload) = serde_json::from_slice::<Value>(&body) else {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "Invalid JSON body" }))).into_response();
    };
    let Some(cookie) = payload.get("cookie").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) else {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "Cookie is required" }))).into_response();
    };
    if !cookie.contains("BXAuth=") {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "Cookie must contain BXAuth field" }))).into_response();
    }
    let normalized = if cookie.ends_with(';') {
        cookie.to_string()
    } else {
        format!("{cookie};")
    };
    let Ok(resp) = state
        .executor
        .0
        .get("https://platform.iflow.cn/api/openapi/apikey")
        .header("Cookie", &normalized)
        .send()
        .await
    else {
        return (StatusCode::BAD_GATEWAY, Json(json!({ "error": "Failed to fetch API key info" }))).into_response();
    };
    let status = resp.status();
    if !status.is_success() {
        return (
            StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
            Json(json!({ "error": "Failed to fetch API key info" })),
        )
            .into_response();
    }
    let data: Value = resp
        .bytes()
        .await
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| json!({}));
    if !data.get("success").and_then(Value::as_bool).unwrap_or(false) {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "API key fetch failed" }))).into_response();
    }
    Json(json!({ "success": true, "data": data.get("data") })).into_response()
}

async fn kiro_api_key(
    State(state): State<AppState>,
    body: axum::body::Bytes,
) -> Response {
    let Ok(payload) = serde_json::from_slice::<Value>(&body) else {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "Invalid JSON body" }))).into_response();
    };
    let Some(api_key) = payload.get("apiKey").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) else {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "API key is required" }))).into_response();
    };
    let region = payload.get("region").and_then(Value::as_str).unwrap_or("us-east-1");
    let claims = decode_jwt_claims(api_key);
    let email = claims.get("email").and_then(Value::as_str);
    let db = state.db.lock().unwrap();
    match save_oauth_connection(
        &db,
        "kiro",
        "api_key",
        Some(api_key),
        None,
        Some(365 * 24 * 3600),
        email,
        None,
        json!({ "region": region, "authMethod": "api_key", "provider": "API Key" }),
    ) {
        Ok(conn) => Json(json!({ "success": true, "connection": { "id": conn["id"], "provider": "kiro", "email": email } })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

async fn kiro_auto_import() -> impl IntoResponse {
    let home = std::env::var("HOME").unwrap_or_default();
    let cache = PathBuf::from(&home).join(".aws/sso/cache");
    let Ok(entries) = std::fs::read_dir(&cache) else {
        return Json(json!({ "found": false, "error": "AWS SSO cache not found. Please login to Kiro IDE first." }));
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) == Some("json")
            && let Ok(text) = std::fs::read_to_string(&p)
            && let Ok(v) = serde_json::from_str::<Value>(&text)
            && let Some(rt) = v.get("refreshToken").and_then(Value::as_str)
            && rt.starts_with("aorAAAAAG")
        {
            return Json(json!({
                "found": true,
                "refreshToken": rt,
                "authMethod": v.get("authMethod"),
                "region": v.get("region"),
            }));
        }
    }
    Json(json!({ "found": false, "error": "Kiro token not found in AWS SSO cache. Please login to Kiro IDE first." }))
}

async fn kiro_import(
    State(state): State<AppState>,
    body: axum::body::Bytes,
) -> Response {
    let Ok(payload) = serde_json::from_slice::<Value>(&body) else {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "Invalid JSON body" }))).into_response();
    };
    let Some(rt) = payload.get("refreshToken").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) else {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "Refresh token is required" }))).into_response();
    };
    let region = payload.get("region").and_then(Value::as_str).unwrap_or("us-east-1");
    let db = state.db.lock().unwrap();
    match save_oauth_connection(
        &db,
        "kiro",
        "oauth",
        None,
        Some(rt),
        Some(3600),
        None,
        None,
        json!({ "region": region, "authMethod": "imported", "provider": "Imported" }),
    ) {
        Ok(conn) => Json(json!({ "success": true, "connection": { "id": conn["id"], "provider": "kiro", "email": Value::Null } })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

async fn kiro_import_cli_proxy(
    State(state): State<AppState>,
    body: axum::body::Bytes,
) -> Response {
    let Ok(payload) = serde_json::from_slice::<Value>(&body) else {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "CLIProxyAPI import failed" }))).into_response();
    };
    let raw = payload
        .get("cliProxyAuth")
        .or_else(|| payload.get("auth"))
        .or_else(|| payload.get("json"))
        .unwrap_or(&payload);
    let Some(at) = raw
        .get("access_token")
        .or_else(|| raw.get("accessToken"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    else {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "CLIProxyAPI import failed: missing accessToken" }))).into_response();
    };
    let rt = raw.get("refresh_token").or_else(|| raw.get("refreshToken")).and_then(Value::as_str);
    let email = raw.get("email").and_then(Value::as_str);
    let db = state.db.lock().unwrap();
    match save_oauth_connection(
        &db,
        "kiro",
        "oauth",
        Some(at),
        rt,
        Some(3600),
        email,
        None,
        json!({ "authMethod": "external_idp" }),
    ) {
        Ok(conn) => Json(json!({ "success": true, "connection": { "id": conn["id"], "provider": "kiro", "email": email } })).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

async fn kiro_social_authorize(Query(q): Query<std::collections::HashMap<String, String>>) -> Response {
    let provider = q.get("provider").map(String::as_str).unwrap_or("");
    if provider != "google" && provider != "github" {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "Invalid provider. Use 'google' or 'github'" }))).into_response();
    }
    let verifier = random_b64_pub(32);
    let challenge = pkce_challenge_pub(&verifier);
    let state = random_b64_pub(24);
    let idp = if provider == "google" { "Google" } else { "Github" };
    let auth_url = format!(
        "https://prod.us-east-1.auth.desktop.kiro.dev/login?idp={idp}&redirect_uri=kiro%3A%2F%2Fkiro.kiroAgent%2Fauthenticate-success&code_challenge={challenge}&code_challenge_method=S256&state={state}"
    );
    Json(json!({
        "authUrl": auth_url,
        "state": state,
        "codeVerifier": verifier,
        "codeChallenge": challenge,
        "provider": provider,
    }))
    .into_response()
}

async fn kiro_social_exchange(
    State(state): State<AppState>,
    body: axum::body::Bytes,
) -> Response {
    let Ok(payload) = serde_json::from_slice::<Value>(&body) else {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "Invalid JSON body" }))).into_response();
    };
    let code = payload.get("code").and_then(Value::as_str).unwrap_or("");
    let verifier = payload.get("codeVerifier").and_then(Value::as_str).unwrap_or("");
    let provider = payload.get("provider").and_then(Value::as_str).unwrap_or("");
    if code.is_empty() || verifier.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "Missing required fields" }))).into_response();
    }
    if provider != "google" && provider != "github" {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "Invalid provider" }))).into_response();
    }
    let Ok(resp) = state
        .executor
        .0
        .post("https://prod.us-east-1.auth.desktop.kiro.dev/oauth/token")
        .header("Content-Type", "application/json")
        .body(
            json!({
                "code": code,
                "code_verifier": verifier,
                "redirect_uri": "kiro://kiro.kiroAgent/authenticate-success"
            })
            .to_string(),
        )
        .send()
        .await
    else {
        return (StatusCode::BAD_GATEWAY, Json(json!({ "error": "Kiro social exchange failed" }))).into_response();
    };
    if !resp.status().is_success() {
        return (StatusCode::BAD_GATEWAY, Json(json!({ "error": "Kiro social exchange rejected" }))).into_response();
    }
    let tok: Value = resp
        .bytes()
        .await
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| json!({}));
    let at = tok.get("accessToken").and_then(Value::as_str);
    let rt = tok.get("refreshToken").and_then(Value::as_str);
    let db = state.db.lock().unwrap();
    match save_oauth_connection(
        &db,
        "kiro",
        "oauth",
        at,
        rt,
        tok.get("expiresIn").and_then(Value::as_i64),
        None,
        None,
        json!({ "authMethod": provider }),
    ) {
        Ok(conn) => Json(json!({ "success": true, "connection": conn })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

async fn xiaomi_mimo_api_key(
    State(state): State<AppState>,
    body: axum::body::Bytes,
) -> Response {
    let Ok(payload) = serde_json::from_slice::<Value>(&body) else {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "Invalid JSON body" }))).into_response();
    };
    let key = payload.get("apiKey").and_then(Value::as_str).map(str::trim).unwrap_or("");
    let pass = payload.get("mimoPassToken").and_then(Value::as_str);
    if key.is_empty() && pass.is_none() {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "API key is required" }))).into_response();
    }
    if !key.is_empty() && !key.starts_with("sk-") {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "Invalid key format — expected sk- prefix" }))).into_response();
    }
    let uid = payload.get("uid").and_then(Value::as_str);
    let email = uid.map(|u| format!("{u}@xiaomi"));
    let base_url = payload
        .get("baseUrl")
        .and_then(Value::as_str)
        .unwrap_or("https://api.xiaomimimo.com/v1");
    let db = state.db.lock().unwrap();
    match save_oauth_connection(
        &db,
        "xiaomi-mimo",
        if key.is_empty() { "session" } else { "api_key" },
        (!key.is_empty()).then_some(key),
        None,
        None,
        email.as_deref(),
        None,
        json!({ "uid": uid, "baseUrl": base_url, "mimoPassToken": pass }),
    ) {
        Ok(conn) => Json(json!({ "success": true, "connection": conn })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

async fn xiaomi_mimo_auto_import() -> impl IntoResponse {
    let home = std::env::var("HOME").unwrap_or_default();
    let candidates = [
        PathBuf::from(&home).join(".local/share/mimocode/auth.json"),
        PathBuf::from(&home).join("Library/Application Support/mimocode/auth.json"),
    ];
    for p in candidates {
        if let Ok(text) = std::fs::read_to_string(&p)
            && let Ok(v) = serde_json::from_str::<Value>(&text)
            && let Some(key) = v.pointer("/xiaomi/key").and_then(Value::as_str)
        {
            return Json(json!({
                "found": true,
                "apiKey": key,
                "uid": v.pointer("/xiaomi/metadata/uid"),
                "baseUrl": v.pointer("/xiaomi/metadata/base_url"),
            }));
        }
    }
    Json(json!({ "found": false, "error": "Xiaomi MiMo Desktop auth file not found." }))
}

async fn xiaomi_mimo_login_start() -> impl IntoResponse {
    let state = random_b64_pub(16);
    Json(json!({ "state": state, "status": "pending", "region": "cn" }))
}

async fn xiaomi_mimo_login_status() -> impl IntoResponse {
    (StatusCode::NOT_FOUND, Json(json!({ "status": "expired" })))
}

async fn zed_auto_import() -> impl IntoResponse {
    Json(json!({ "found": false, "error": "Zed IDE credentials not found" }))
}

async fn zed_import(
    State(state): State<AppState>,
    body: axum::body::Bytes,
) -> Response {
    let Ok(payload) = serde_json::from_slice::<Value>(&body) else {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "Invalid JSON body" }))).into_response();
    };
    let access_token = payload.get("accessToken").and_then(Value::as_str).map(str::trim).unwrap_or("");
    let user_id = payload.get("userId").and_then(Value::as_str).map(str::trim).unwrap_or("");
    if access_token.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "Access token is required" }))).into_response();
    }
    if user_id.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "User id is required" }))).into_response();
    }
    let Ok(resp) = state
        .executor
        .0
        .get("https://cloud.zed.dev/client/users/me")
        .header("Authorization", format!("{user_id} {access_token}"))
        .send()
        .await
    else {
        return (StatusCode::UNAUTHORIZED, Json(json!({ "error": "Zed token validation failed" }))).into_response();
    };
    if !resp.status().is_success() {
        return (StatusCode::UNAUTHORIZED, Json(json!({ "error": "Zed token validation failed" }))).into_response();
    }
    let user: Value = resp
        .bytes()
        .await
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| json!({}));
    let email = user.get("email").and_then(Value::as_str);
    let display_name = user.get("name").and_then(Value::as_str);
    let db = state.db.lock().unwrap();
    match save_oauth_connection(
        &db,
        "zed",
        "oauth",
        Some(access_token),
        None,
        None,
        email,
        display_name,
        json!({ "authMethod": "imported", "userId": user_id }),
    ) {
        Ok(conn) => Json(json!({ "success": true, "connection": conn })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
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
        (router().with_state(state), db)
    }

    #[tokio::test]
    async fn bulk_imports_and_token_imports_persist_connections() {
        let (app, db) = test_app();
        let res = app
            .clone()
            .oneshot(
                Request::post("/api/oauth/codex/bulk-import")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"accounts":[{"accessToken":"tok-1"},{"bad":1}]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v: Value =
            serde_json::from_slice(&to_bytes(res.into_body(), 65536).await.unwrap()).unwrap();
        assert_eq!(v["success"], 1);
        assert_eq!(v["failed"], 1);

        let res = app
            .clone()
            .oneshot(
                Request::post("/api/oauth/grok-cli/bulk-import")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"[{"access_token":"xai-1"}]"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let res = app
            .clone()
            .oneshot(
                Request::post("/api/oauth/kiro/api-key")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"apiKey":"kiro-secret-1"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let res = app
            .oneshot(
                Request::post("/api/oauth/xiaomi-mimo/api-key")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"apiKey":"bad-prefix"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);

        let count: i64 = db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM providerConnections", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 3);
    }

    #[tokio::test]
    async fn kiro_social_authorize_and_cursor_import_validate() {
        let (app, _db) = test_app();
        let res = app
            .clone()
            .oneshot(
                Request::get("/api/oauth/kiro/social-authorize?provider=google")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v: Value =
            serde_json::from_slice(&to_bytes(res.into_body(), 65536).await.unwrap()).unwrap();
        assert!(v["authUrl"].as_str().unwrap().contains("Idp=Google") || v["authUrl"].as_str().unwrap().contains("idp=Google"));

        let res = app
            .clone()
            .oneshot(Request::get("/api/oauth/cursor/import").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let res = app
            .oneshot(
                Request::post("/api/oauth/iflow/cookie")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"cookie":"no-bxauth"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }
}
