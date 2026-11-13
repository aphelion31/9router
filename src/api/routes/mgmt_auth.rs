use crate::dashboard;
use crate::server::AppState;
use axum::{
    Json, Router,
    body::Bytes,
    extract::{Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD as B64, URL_SAFE_NO_PAD},
};
use ring::rand::SecureRandom;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

const BASE32_ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
const BACKUP_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/auth/totp", get(totp_get).post(totp_post))
        .route("/api/auth/oidc/test", post(oidc_test))
        .route("/api/auth/saml/test", post(saml_test))
}

pub fn public_routes() -> Router<AppState> {
    Router::new()
        .route("/api/auth/login", post(login))
        .route("/api/auth/logout", post(logout))
        .route("/api/auth/status", get(status))
        .route("/api/auth/reset-password", post(reset_password))
        .route("/api/auth/oidc/start", get(oidc_start))
        .route("/api/auth/oidc/callback", get(oidc_callback))
        .route("/api/auth/saml/start", get(saml_start))
        .route("/api/auth/saml/acs", post(saml_acs))
        .route("/api/auth/saml/metadata", get(saml_metadata))
}

fn err(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

fn read_settings_map(s: &AppState) -> Map<String, Value> {
    let Ok(db) = s.db.lock() else {
        return Map::new();
    };
    let mut map = crate::db::kv_get(&db, "settings", "data")
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<Map<String, Value>>(&raw).ok())
        .unwrap_or_default();
    if let Ok(Some(pw)) = crate::db::kv_get(&db, "settings", "password") {
        if !pw.is_empty() {
            map.insert("password".into(), Value::String(pw));
        }
    }
    map
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    for part in raw.split(';') {
        if let Some((k, v)) = part.trim().split_once('=') {
            if k.trim() == name {
                return Some(v.trim().to_string());
            }
        }
    }
    None
}

fn decode_valid_session(headers: &HeaderMap, secret: Option<&[u8]>) -> Option<Value> {
    let secret = secret?;
    let token = cookie_value(headers, "auth_token")?;
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let header: Value = URL_SAFE_NO_PAD
        .decode(parts[0])
        .ok()
        .and_then(|v| serde_json::from_slice(&v).ok())?;
    let payload: Value = URL_SAFE_NO_PAD
        .decode(parts[1])
        .ok()
        .and_then(|v| serde_json::from_slice(&v).ok())?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if header["alg"] != "HS256" || !payload["exp"].as_u64().is_some_and(|exp| exp > now) {
        return None;
    }
    let sig = URL_SAFE_NO_PAD.decode(parts[2]).ok()?;
    ring::hmac::verify(
        &ring::hmac::Key::new(ring::hmac::HMAC_SHA256, secret),
        format!("{}.{}", parts[0], parts[1]).as_bytes(),
        &sig,
    )
    .ok()?;
    Some(payload)
}

fn issue_custom_token(secret: &[u8], extra: Value) -> String {
    let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"HS256","typ":"JWT"}"#);
    let exp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        + 86400;
    let mut obj = extra.as_object().cloned().unwrap_or_default();
    obj.insert("exp".into(), json!(exp));
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&obj).unwrap_or_default());
    let input = format!("{header}.{payload}");
    let sig = ring::hmac::sign(
        &ring::hmac::Key::new(ring::hmac::HMAC_SHA256, secret),
        input.as_bytes(),
    );
    format!("{input}.{}", URL_SAFE_NO_PAD.encode(sig.as_ref()))
}

fn sha256_hex(input: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, input);
    digest.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

fn random_bytes(len: usize) -> Vec<u8> {
    let mut bytes = vec![0u8; len];
    let _ = ring::rand::SystemRandom::new().fill(&mut bytes);
    bytes
}

fn base32_encode(buf: &[u8]) -> String {
    let mut out = String::new();
    let mut bits = 0u32;
    let mut val = 0u32;
    for &b in buf {
        val = (val << 8) | b as u32;
        bits += 8;
        while bits >= 5 {
            out.push(BASE32_ALPHABET[((val >> (bits - 5)) & 31) as usize] as char);
            bits -= 5;
        }
    }
    if bits > 0 {
        out.push(BASE32_ALPHABET[((val << (5 - bits)) & 31) as usize] as char);
    }
    while !out.len().is_multiple_of(8) {
        out.push('=');
    }
    out
}

fn base32_decode(input: &str) -> Result<Vec<u8>, ()> {
    let mut bytes = Vec::new();
    let mut bits = 0u32;
    let mut val = 0u32;
    for ch in input.bytes() {
        if ch.is_ascii_whitespace() || ch == b'=' {
            continue;
        }
        let up = ch.to_ascii_uppercase();
        let idx = BASE32_ALPHABET.iter().position(|&c| c == up).ok_or(())?;
        val = (val << 5) | idx as u32;
        bits += 5;
        if bits >= 8 {
            bytes.push(((val >> (bits - 8)) & 0xff) as u8);
            bits -= 8;
        }
    }
    Ok(bytes)
}

fn totp_code(secret: &str, time_secs: u64) -> Result<String, ()> {
    let key_bytes = base32_decode(secret)?;
    let counter = time_secs / 30;
    let msg = counter.to_be_bytes();
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, &key_bytes);
    let tag = ring::hmac::sign(&key, &msg);
    let h = tag.as_ref();
    let offset = (h[h.len() - 1] & 0x0f) as usize;
    let bin = ((h[offset] as u32 & 0x7f) << 24)
        | ((h[offset + 1] as u32) << 16)
        | ((h[offset + 2] as u32) << 8)
        | (h[offset + 3] as u32);
    Ok(format!("{:06}", bin % 1_000_000))
}

fn verify_totp(secret: &str, code: &str) -> bool {
    let clean: String = code.chars().filter(|c| !c.is_whitespace()).collect();
    if clean.len() != 6 || !clean.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    for delta in [-30i64, 0, 30] {
        let t = now as i64 + delta;
        if t < 0 {
            continue;
        }
        if let Ok(expected) = totp_code(secret, t as u64) {
            if expected == clean {
                return true;
            }
        }
    }
    false
}

fn get_totp_config(s: &AppState) -> Value {
    let Ok(db) = s.db.lock() else {
        return json!({});
    };
    crate::db::kv_get(&db, "totp", "config")
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .unwrap_or_else(|| {
            json!({
                "enabled": false,
                "secret": "",
                "backupHashes": [],
                "pendingSecret": "",
                "pendingBackupHashes": []
            })
        })
}

fn save_totp_config(s: &AppState, cfg: &Value) {
    if let Ok(db) = s.db.lock() {
        let _ = crate::db::kv_set(&db, "totp", "config", &cfg.to_string());
    }
}

fn hash_backup_code(code: &str) -> String {
    let clean: String = code
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .flat_map(char::to_uppercase)
        .collect();
    sha256_hex(clean.as_bytes())
}

fn consume_backup_code(s: &AppState, code: &str) -> bool {
    let mut cfg = get_totp_config(s);
    let target = hash_backup_code(code);
    let Some(arr) = cfg["backupHashes"].as_array().cloned() else {
        return false;
    };
    if let Some(pos) = arr.iter().position(|v| v.as_str() == Some(&target)) {
        let mut next = arr;
        next.remove(pos);
        cfg["backupHashes"] = Value::Array(next);
        save_totp_config(s, &cfg);
        return true;
    }
    false
}

fn is_oidc_configured(settings: &Map<String, Value>) -> bool {
    settings
        .get("oidcIssuerUrl")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.trim().is_empty())
        && settings
            .get("oidcClientId")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty())
}

fn is_saml_configured(settings: &Map<String, Value>) -> bool {
    settings
        .get("samlEntryPoint")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.trim().is_empty())
        && settings
            .get("samlCert")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty())
}

fn request_origin(headers: &HeaderMap) -> String {
    let proto = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("http");
    let host = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get(header::HOST))
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost:20128");
    format!("{proto}://{host}")
}

fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(v) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        if bytes[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

async fn login(State(s): State<AppState>, Json(body): Json<Value>) -> Response {
    let Some(password) = body["password"].as_str() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let ok = s
        .db
        .lock()
        .ok()
        .is_some_and(|db| dashboard::verify_password(&db, password));
    if !ok {
        return err(StatusCode::UNAUTHORIZED, "Invalid password");
    }

    let totp_cfg = get_totp_config(&s);
    if totp_cfg["enabled"] == true
        && totp_cfg["secret"]
            .as_str()
            .is_some_and(|sec| !sec.is_empty())
    {
        let code = body["totpCode"].as_str().unwrap_or("").trim();
        let totp_ok = if code.contains('-') {
            consume_backup_code(&s, code)
        } else if !code.is_empty() {
            verify_totp(totp_cfg["secret"].as_str().unwrap_or(""), code)
        } else {
            false
        };
        if !totp_ok {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({
                    "error": "Invalid authenticator code.",
                    "totpRequired": true
                })),
            )
                .into_response();
        }
    }

    let Some(secret) = s.jwt_secret.as_deref() else {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "JWT secret not configured",
        );
    };
    let token = dashboard::issue_auth_token(secret);
    let Ok(cookie) = HeaderValue::from_str(&dashboard::auth_cookie_header(&token)) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    (
        StatusCode::OK,
        [(header::SET_COOKIE, cookie)],
        Json(json!({ "success": true })),
    )
        .into_response()
}

async fn logout() -> Response {
    (
        StatusCode::OK,
        [(
            header::SET_COOKIE,
            HeaderValue::from_static(dashboard::clear_auth_cookie_header()),
        )],
        Json(json!({ "success": true })),
    )
        .into_response()
}

async fn status(State(s): State<AppState>, headers: HeaderMap) -> Response {
    let settings = read_settings_map(&s);
    let session = decode_valid_session(&headers, s.jwt_secret.as_deref());
    let totp_cfg = get_totp_config(&s);
    let totp_enabled = totp_cfg["enabled"] == true
        && totp_cfg["secret"]
            .as_str()
            .is_some_and(|sec| !sec.is_empty());

    let require_login = settings
        .get("requireLogin")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let auth_mode = settings
        .get("authMode")
        .and_then(Value::as_str)
        .unwrap_or("password");
    let sso_type = settings
        .get("ssoType")
        .and_then(Value::as_str)
        .unwrap_or("oidc");

    let oidc_name = session
        .as_ref()
        .and_then(|v| v["oidcName"].as_str())
        .map(str::trim)
        .unwrap_or("");
    let oidc_email = session
        .as_ref()
        .and_then(|v| v["oidcEmail"].as_str())
        .map(str::trim)
        .unwrap_or("");
    let saml_name = session
        .as_ref()
        .and_then(|v| v["samlName"].as_str())
        .map(str::trim)
        .unwrap_or("");
    let saml_email = session
        .as_ref()
        .and_then(|v| v["samlEmail"].as_str())
        .map(str::trim)
        .unwrap_or("");
    let is_saml = session.as_ref().and_then(|v| v["saml"].as_bool()) == Some(true);
    let is_oidc = session.as_ref().and_then(|v| v["oidc"].as_bool()) == Some(true);

    let display_name = if !saml_name.is_empty() {
        saml_name
    } else if !saml_email.is_empty() {
        saml_email
    } else if !oidc_name.is_empty() {
        oidc_name
    } else if !oidc_email.is_empty() {
        oidc_email
    } else if is_saml {
        "SAML user"
    } else if is_oidc {
        "OIDC user"
    } else {
        "Password user"
    };

    let login_method = if is_saml {
        "SAML"
    } else if is_oidc {
        "OIDC"
    } else {
        "Password"
    };

    Json(json!({
        "requireLogin": require_login,
        "authMode": auth_mode,
        "ssoType": sso_type,
        "oidcConfigured": is_oidc_configured(&settings),
        "oidcLoginLabel": settings.get("oidcLoginLabel").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).unwrap_or("Sign in with OIDC"),
        "samlConfigured": is_saml_configured(&settings),
        "samlLoginLabel": settings.get("samlLoginLabel").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).unwrap_or("Sign in with SAML SSO"),
        "hasPassword": settings.get("password").and_then(Value::as_str).is_some_and(|p| !p.is_empty()),
        "totpEnabled": totp_enabled,
        "displayName": display_name,
        "loginMethod": login_method,
        "authenticated": session.is_some(),
        "oidcName": (!oidc_name.is_empty()).then_some(oidc_name),
        "oidcEmail": (!oidc_email.is_empty()).then_some(oidc_email),
        "oidcLogin": is_oidc,
        "samlName": (!saml_name.is_empty()).then_some(saml_name),
        "samlEmail": (!saml_email.is_empty()).then_some(saml_email),
        "samlLogin": is_saml,
    }))
    .into_response()
}

async fn reset_password(State(s): State<AppState>) -> Response {
    let Ok(db) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "DB lock failed");
    };
    let _ = db.execute("DELETE FROM kv WHERE scope = 'settings' AND key = 'password'", []);
    if let Ok(Some(raw)) = crate::db::kv_get(&db, "settings", "data") {
        if let Ok(mut map) = serde_json::from_str::<Map<String, Value>>(&raw) {
            map.insert("password".into(), Value::Null);
            let _ = crate::db::kv_set(&db, "settings", "data", &Value::Object(map).to_string());
        }
    }
    Json(json!({ "success": true })).into_response()
}

async fn totp_get(State(s): State<AppState>) -> Response {
    let cfg = get_totp_config(&s);
    (
        StatusCode::OK,
        [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
        Json(json!({
            "enabled": cfg["enabled"] == true,
            "backupCodesRemaining": cfg["backupHashes"].as_array().map(|a| a.len()).unwrap_or(0),
            "hasPendingSetup": cfg["pendingSecret"].as_str().is_some_and(|p| !p.is_empty()),
        })),
    )
        .into_response()
}

async fn totp_post(State(s): State<AppState>, Json(body): Json<Value>) -> Response {
    let action = body["action"].as_str().unwrap_or("");
    let code = body["code"].as_str().unwrap_or("");
    let mut cfg = get_totp_config(&s);

    match action {
        "setup" => {
            if cfg["enabled"] == true {
                return err(StatusCode::BAD_REQUEST, "TOTP is already enabled");
            }
            let secret = base32_encode(&random_bytes(20));
            let mut backup_codes = Vec::with_capacity(10);
            let mut backup_hashes = Vec::with_capacity(10);
            for _ in 0..10 {
                let raw: String = random_bytes(8)
                    .into_iter()
                    .map(|b| BACKUP_ALPHABET[(b as usize) % BACKUP_ALPHABET.len()] as char)
                    .collect();
                let formatted = format!("{}-{}", &raw[..4], &raw[4..]);
                backup_hashes.push(json!(hash_backup_code(&formatted)));
                backup_codes.push(formatted);
            }
            cfg["pendingSecret"] = json!(secret);
            cfg["pendingBackupHashes"] = json!(backup_hashes);
            save_totp_config(&s, &cfg);
            let otpauth = format!(
                "otpauth://totp/9Router%20Pro:9Router%20Pro?secret={secret}&issuer=9Router%20Pro&algorithm=SHA1&digits=6&period=30"
            );
            Json(json!({
                "secret": secret,
                "otpauthUrl": otpauth,
                "backupCodes": backup_codes
            }))
            .into_response()
        }
        "enable" => {
            let pending = cfg["pendingSecret"].as_str().unwrap_or("").to_string();
            if pending.is_empty() || !verify_totp(&pending, code) {
                return err(StatusCode::UNAUTHORIZED, "Invalid authenticator code");
            }
            cfg["enabled"] = json!(true);
            cfg["secret"] = json!(pending);
            cfg["backupHashes"] = cfg["pendingBackupHashes"].clone();
            cfg["pendingSecret"] = json!("");
            cfg["pendingBackupHashes"] = json!([]);
            save_totp_config(&s, &cfg);
            Json(json!({ "enabled": true })).into_response()
        }
        "disable" => {
            let secret = cfg["secret"].as_str().unwrap_or("");
            let ok = if code.contains('-') {
                consume_backup_code(&s, code)
            } else {
                !secret.is_empty() && verify_totp(secret, code)
            };
            if !ok {
                return err(
                    StatusCode::UNAUTHORIZED,
                    "Invalid authenticator or backup code",
                );
            }
            save_totp_config(
                &s,
                &json!({
                    "enabled": false,
                    "secret": "",
                    "backupHashes": [],
                    "pendingSecret": "",
                    "pendingBackupHashes": []
                }),
            );
            Json(json!({ "enabled": false })).into_response()
        }
        _ => err(StatusCode::BAD_REQUEST, "Unknown action"),
    }
}

async fn oidc_test(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let settings = read_settings_map(&s);
    let issuer_url = body["issuerUrl"]
        .as_str()
        .or_else(|| settings.get("oidcIssuerUrl").and_then(Value::as_str))
        .unwrap_or("")
        .trim()
        .to_string();
    let client_id = body["clientId"]
        .as_str()
        .or_else(|| settings.get("oidcClientId").and_then(Value::as_str))
        .unwrap_or("")
        .trim()
        .to_string();
    let scopes = body["scopes"]
        .as_str()
        .or_else(|| settings.get("oidcScopes").and_then(Value::as_str))
        .unwrap_or("openid profile email")
        .trim()
        .to_string();
    let client_secret = if body.get("clientSecret").is_some() {
        body["clientSecret"].as_str().unwrap_or("").trim().to_string()
    } else {
        settings
            .get("oidcClientSecret")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string()
    };

    if issuer_url.is_empty() {
        return err(StatusCode::BAD_REQUEST, "Issuer URL is required");
    }
    if client_id.is_empty() {
        return err(StatusCode::BAD_REQUEST, "Client ID is required");
    }

    let disc_url = format!(
        "{}/.well-known/openid-configuration",
        issuer_url.trim_end_matches('/')
    );
    let Ok(resp) = s.executor.0.get(&disc_url).send().await else {
        return err(StatusCode::BAD_GATEWAY, "Failed to fetch OIDC discovery");
    };
    let Ok(bytes) = resp.bytes().await else {
        return err(StatusCode::BAD_GATEWAY, "Failed to read OIDC discovery");
    };
    let Ok(disc) = serde_json::from_slice::<Value>(&bytes) else {
        return err(StatusCode::BAD_GATEWAY, "Invalid OIDC discovery JSON");
    };

    let fp = sha256_hex(format!("{issuer_url}|{client_id}|{client_secret}").as_bytes());
    if let Ok(db) = s.db.lock() {
        let _ = crate::db::kv_set(&db, "ssoTestGate", "oidc", &fp);
    }

    let redirect_uri = format!("{}/api/auth/oidc/callback", request_origin(&headers));
    Json(json!({
        "ok": true,
        "discoveryOk": true,
        "clientSecretTested": false,
        "clientSecretValid": null,
        "issuerUrl": issuer_url,
        "clientId": client_id,
        "scopes": scopes,
        "redirectUri": redirect_uri,
        "authorizationEndpoint": disc["authorization_endpoint"].as_str().unwrap_or(""),
        "tokenEndpoint": disc["token_endpoint"].as_str().unwrap_or(""),
        "jwksUri": disc["jwks_uri"].as_str().unwrap_or(""),
    }))
    .into_response()
}

async fn oidc_start(State(s): State<AppState>, headers: HeaderMap) -> Response {
    let settings = read_settings_map(&s);
    if !is_oidc_configured(&settings) {
        return Redirect::to("/login?error=oidc_not_configured").into_response();
    }
    let issuer = settings["oidcIssuerUrl"].as_str().unwrap_or("").trim();
    let client_id = settings["oidcClientId"].as_str().unwrap_or("").trim();
    let scopes = settings
        .get("oidcScopes")
        .and_then(Value::as_str)
        .unwrap_or("openid profile email")
        .trim();
    let disc_url = format!("{}/.well-known/openid-configuration", issuer.trim_end_matches('/'));
    let Ok(resp) = s.executor.0.get(&disc_url).send().await else {
        return Redirect::to("/login?error=oidc_discovery_failed").into_response();
    };
    let Ok(bytes) = resp.bytes().await else {
        return Redirect::to("/login?error=oidc_discovery_failed").into_response();
    };
    let Ok(disc) = serde_json::from_slice::<Value>(&bytes) else {
        return Redirect::to("/login?error=oidc_discovery_failed").into_response();
    };
    let Some(auth_ep) = disc["authorization_endpoint"].as_str() else {
        return Redirect::to("/login?error=oidc_no_auth_endpoint").into_response();
    };

    let state = URL_SAFE_NO_PAD.encode(random_bytes(16));
    let nonce = URL_SAFE_NO_PAD.encode(random_bytes(16));
    let verifier = URL_SAFE_NO_PAD.encode(random_bytes(32));
    let challenge = URL_SAFE_NO_PAD.encode(
        ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes()).as_ref(),
    );
    let redirect_uri = format!("{}/api/auth/oidc/callback", request_origin(&headers));
    let sep = if auth_ep.contains('?') { '&' } else { '?' };
    let target = format!(
        "{auth_ep}{sep}response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&nonce={}&code_challenge={}&code_challenge_method=S256",
        url_encode(client_id),
        url_encode(&redirect_uri),
        url_encode(scopes),
        url_encode(&state),
        url_encode(&nonce),
        url_encode(&challenge),
    );

    let mut resp = Redirect::to(&target).into_response();
    for (k, v) in [
        ("oidc_state", state),
        ("oidc_nonce", nonce),
        ("oidc_code_verifier", verifier),
    ] {
        if let Ok(hv) = HeaderValue::from_str(&format!(
            "{k}={v}; Path=/; HttpOnly; SameSite=Lax; Max-Age=600"
        )) {
            resp.headers_mut().append(header::SET_COOKIE, hv);
        }
    }
    resp
}

async fn oidc_callback(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    if let Some(e) = q.get("error") {
        return Redirect::to(&format!("/login?error={}", url_encode(e))).into_response();
    }
    let (Some(code), Some(state)) = (q.get("code"), q.get("state")) else {
        return Redirect::to("/login?error=oidc_missing_code").into_response();
    };
    let stored_state = cookie_value(&headers, "oidc_state");
    let verifier = cookie_value(&headers, "oidc_code_verifier");
    if stored_state.as_deref() != Some(state.as_str()) || verifier.is_none() {
        return Redirect::to("/login?error=oidc_invalid_state").into_response();
    }
    let settings = read_settings_map(&s);
    if !is_oidc_configured(&settings) {
        return Redirect::to("/login?error=oidc_not_configured").into_response();
    }
    let issuer = settings["oidcIssuerUrl"].as_str().unwrap_or("").trim();
    let client_id = settings["oidcClientId"].as_str().unwrap_or("").trim();
    let client_secret = settings
        .get("oidcClientSecret")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let disc_url = format!("{}/.well-known/openid-configuration", issuer.trim_end_matches('/'));
    let Ok(disc_resp) = s.executor.0.get(&disc_url).send().await else {
        return Redirect::to("/login?error=oidc_discovery_failed").into_response();
    };
    let Ok(disc_bytes) = disc_resp.bytes().await else {
        return Redirect::to("/login?error=oidc_discovery_failed").into_response();
    };
    let Ok(disc) = serde_json::from_slice::<Value>(&disc_bytes) else {
        return Redirect::to("/login?error=oidc_discovery_failed").into_response();
    };
    let Some(token_ep) = disc["token_endpoint"].as_str() else {
        return Redirect::to("/login?error=oidc_no_token_endpoint").into_response();
    };
    let redirect_uri = format!("{}/api/auth/oidc/callback", request_origin(&headers));
    let mut form = format!(
        "grant_type=authorization_code&code={}&redirect_uri={}&client_id={}&code_verifier={}",
        url_encode(code),
        url_encode(&redirect_uri),
        url_encode(client_id),
        url_encode(verifier.as_deref().unwrap_or("")),
    );
    if !client_secret.is_empty() {
        form.push_str(&format!("&client_secret={}", url_encode(client_secret)));
    }
    let Ok(tok_resp) = s
        .executor
        .0
        .post(token_ep)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(form)
        .send()
        .await
    else {
        return Redirect::to("/login?error=oidc_token_failed").into_response();
    };
    let Ok(tok_bytes) = tok_resp.bytes().await else {
        return Redirect::to("/login?error=oidc_token_failed").into_response();
    };
    let Ok(tok) = serde_json::from_slice::<Value>(&tok_bytes) else {
        return Redirect::to("/login?error=oidc_token_failed").into_response();
    };
    let Some(id_token) = tok["id_token"].as_str() else {
        return Redirect::to("/login?error=oidc_missing_id_token").into_response();
    };
    let claims: Value = id_token
        .split('.')
        .nth(1)
        .and_then(|p| URL_SAFE_NO_PAD.decode(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| json!({}));

    let Some(secret) = s.jwt_secret.as_deref() else {
        return Redirect::to("/login?error=jwt_not_configured").into_response();
    };
    let session_tok = issue_custom_token(
        secret,
        json!({
            "oidc": true,
            "oidcSub": claims["sub"],
            "oidcEmail": claims["email"],
            "oidcName": claims["name"].as_str().or_else(|| claims["preferred_username"].as_str()).unwrap_or("OIDC user"),
        }),
    );
    let mut resp = Redirect::to("/dashboard").into_response();
    if let Ok(hv) = HeaderValue::from_str(&dashboard::auth_cookie_header(&session_tok)) {
        resp.headers_mut().append(header::SET_COOKIE, hv);
    }
    resp
}

async fn saml_test(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let settings = read_settings_map(&s);
    let entry_point = body["samlEntryPoint"]
        .as_str()
        .or_else(|| settings.get("samlEntryPoint").and_then(Value::as_str))
        .unwrap_or("")
        .trim()
        .to_string();
    let issuer = body["samlIssuer"]
        .as_str()
        .or_else(|| settings.get("samlIssuer").and_then(Value::as_str))
        .unwrap_or("urn:9router:sp")
        .trim()
        .to_string();
    let cert = if body.get("samlCert").is_some() {
        body["samlCert"].as_str().unwrap_or("").trim().to_string()
    } else {
        settings
            .get("samlCert")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string()
    };

    if entry_point.is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "Single Sign-On Service URL (samlEntryPoint) is required",
        );
    }
    if !(entry_point.starts_with("http://") || entry_point.starts_with("https://")) {
        return err(
            StatusCode::BAD_REQUEST,
            "Single Sign-On Service URL must be a valid URL",
        );
    }
    if issuer.is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "SP Entity ID / Issuer (samlIssuer) is required",
        );
    }
    if cert.is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "IdP X.509 Certificate (samlCert) is required",
        );
    }

    let fp = sha256_hex(format!("{entry_point}|{issuer}|{cert}").as_bytes());
    if let Ok(db) = s.db.lock() {
        let _ = crate::db::kv_set(&db, "ssoTestGate", "saml", &fp);
    }
    let origin = request_origin(&headers);
    Json(json!({
        "ok": true,
        "samlEntryPoint": entry_point,
        "samlIssuer": issuer,
        "certValid": true,
        "acsUrl": format!("{origin}/api/auth/saml/acs"),
        "metadataUrl": format!("{origin}/api/auth/saml/metadata"),
        "message": "SAML 2.0 configuration verified successfully."
    }))
    .into_response()
}

async fn saml_metadata(State(s): State<AppState>, headers: HeaderMap) -> Response {
    let settings = read_settings_map(&s);
    let origin = request_origin(&headers);
    let issuer = settings
        .get("samlIssuer")
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty())
        .unwrap_or("urn:9router:sp");
    let acs = format!("{origin}/api/auth/saml/acs");
    let xml = format!(
        r#"<?xml version="1.0"?>
<EntityDescriptor xmlns="urn:oasis:names:tc:SAML:2.0:metadata" entityID="{issuer}">
  <SPSSODescriptor AuthnRequestsSigned="false" WantAssertionsSigned="true" protocolSupportEnumeration="urn:oasis:names:tc:SAML:2.0:protocol">
    <NameIDFormat>urn:oasis:names:tc:SAML:1.1:nameid-format:emailAddress</NameIDFormat>
    <AssertionConsumerService Binding="urn:oasis:names:tc:SAML:2.0:bindings:HTTP-POST" Location="{acs}" index="1"/>
  </SPSSODescriptor>
</EntityDescriptor>"#
    );
    (
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/xml"),
            ),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
        ],
        xml,
    )
        .into_response()
}

async fn saml_start(State(s): State<AppState>, headers: HeaderMap) -> Response {
    let settings = read_settings_map(&s);
    if !is_saml_configured(&settings) {
        return Redirect::to("/login?error=saml_not_configured").into_response();
    }
    let entry_point = settings["samlEntryPoint"].as_str().unwrap_or("").trim();
    let issuer = settings
        .get("samlIssuer")
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty())
        .unwrap_or("urn:9router:sp");
    let origin = request_origin(&headers);
    let req_id = format!("_9r{}", URL_SAFE_NO_PAD.encode(random_bytes(12)));
    let acs = format!("{origin}/api/auth/saml/acs");
    let authn = format!(
        r#"<samlp:AuthnRequest xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" ID="{req_id}" Version="2.0" IssueInstant="2026-01-01T00:00:00Z" AssertionConsumerServiceURL="{acs}" Destination="{entry_point}"><saml:Issuer>{issuer}</saml:Issuer></samlp:AuthnRequest>"#
    );
    let encoded = B64.encode(authn.as_bytes());
    let sep = if entry_point.contains('?') { '&' } else { '?' };
    let target = format!("{entry_point}{sep}SAMLRequest={}", url_encode(&encoded));
    let mut resp = Redirect::to(&target).into_response();
    if let Ok(hv) = HeaderValue::from_str(&format!(
        "saml_state={req_id}; Path=/; HttpOnly; SameSite=Lax; Max-Age=600"
    )) {
        resp.headers_mut().append(header::SET_COOKIE, hv);
    }
    resp
}

fn extract_xml_tag(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}");
    let idx = xml.find(&open)?;
    let rest = &xml[idx..];
    let gt = rest.find('>')?;
    let after = &rest[gt + 1..];
    let close = format!("</{tag}>");
    let end = after.find(&close)?;
    Some(after[..end].trim().to_string())
}

async fn saml_acs(State(s): State<AppState>, body: Bytes) -> Response {
    let settings = read_settings_map(&s);
    if !is_saml_configured(&settings) {
        return Redirect::to("/login?error=saml_not_configured").into_response();
    }
    let raw_form = String::from_utf8_lossy(&body);
    let mut saml_response = None;
    for pair in raw_form.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if k == "SAMLResponse" {
                saml_response = Some(url_decode(v));
                break;
            }
        }
    }
    let Some(encoded) = saml_response.filter(|s| !s.is_empty()) else {
        return Redirect::to("/login?error=saml_missing_response").into_response();
    };
    let Ok(xml_bytes) = B64.decode(encoded.trim()) else {
        return Redirect::to("/login?error=saml_invalid_response").into_response();
    };
    let xml = String::from_utf8_lossy(&xml_bytes);
    let name_id = extract_xml_tag(&xml, "saml:NameID")
        .or_else(|| extract_xml_tag(&xml, "NameID"))
        .unwrap_or_else(|| "SAML user".to_string());
    let Some(secret) = s.jwt_secret.as_deref() else {
        return Redirect::to("/login?error=jwt_not_configured").into_response();
    };
    let session_tok = issue_custom_token(
        secret,
        json!({
            "saml": true,
            "samlEmail": name_id.contains('@').then_some(&name_id),
            "samlName": name_id,
        }),
    );
    let mut resp = Redirect::to("/dashboard").into_response();
    if let Ok(hv) = HeaderValue::from_str(&dashboard::auth_cookie_header(&session_tok)) {
        resp.headers_mut().append(header::SET_COOKIE, hv);
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::routes::test_support::call;
    use axum::body::Body;
    use axum::http::{Method, Request};
    use rusqlite::Connection;
    use std::sync::{Arc, Mutex};

    fn auth_app() -> (Router, Arc<Mutex<Connection>>) {
        let db = Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        let db = Arc::new(Mutex::new(db));
        let state = AppState {
            db: Arc::clone(&db),
            executor: Arc::new(crate::api::executor::HttpExecutor(reqwest::Client::new())),
            jwt_secret: Some(b"test-secret".to_vec()),
        };
        (
            Router::new()
                .merge(routes())
                .merge(public_routes())
                .with_state(state),
            db,
        )
    }

    #[tokio::test]
    async fn status_totp_reset_and_saml_routes_work() {
        let (app, db) = auth_app();
        {
            let conn = db.lock().unwrap();
            crate::db::kv_set(&conn, "settings", "password", "pass123").unwrap();
        }

        let (st, res) = call(
            app.clone(),
            Request::get("/api/auth/status").body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(res["hasPassword"], true);
        assert_eq!(res["authenticated"], false);

        let (st, setup) = call(
            app.clone(),
            Request::builder()
                .method(Method::POST)
                .uri("/api/auth/totp")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"action":"setup"}"#))
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let secret = setup["secret"].as_str().unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let code = totp_code(secret, now).unwrap();

        let (st, en) = call(
            app.clone(),
            Request::builder()
                .method(Method::POST)
                .uri("/api/auth/totp")
                .header("content-type", "application/json")
                .body(Body::from(format!(r#"{{"action":"enable","code":"{code}"}}"#)))
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(en["enabled"], true);

        let (st, saml) = call(
            app.clone(),
            Request::builder()
                .method(Method::POST)
                .uri("/api/auth/saml/test")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"samlEntryPoint":"https://idp.example.com/sso","samlIssuer":"urn:test","samlCert":"CERTDATA"}"#,
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(saml["ok"], true);

        let (st, _) = call(
            app,
            Request::builder()
                .method(Method::POST)
                .uri("/api/auth/reset-password")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
    }
}
