use crate::server::AppState;
use axum::{
    Json, Router,
    extract::State,
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
        .route("/api/auth/status", get(status))
        .route("/api/auth/reset-password", post(unavailable))
        .route("/api/auth/oidc/callback", get(unavailable))
        .route("/api/auth/oidc/start", get(unavailable))
        .route("/api/auth/oidc/test", post(unavailable))
        .route("/api/auth/saml/acs", post(unavailable))
        .route("/api/auth/saml/metadata", get(unavailable))
        .route("/api/auth/saml/start", get(unavailable))
        .route("/api/auth/saml/test", post(unavailable))
        .route("/api/auth/totp", get(unavailable).post(unavailable))
        .route("/api/settings", get(settings).patch(update_settings))
        .route("/api/settings/require-login", get(require_login))
        .route("/api/settings/backup", get(unavailable).post(unavailable))
        .route(
            "/api/settings/backup/{id}",
            get(unavailable).post(unavailable),
        )
        .route("/api/settings/database", get(unavailable).post(unavailable))
        .route(
            "/api/settings/encryption",
            get(unavailable).post(unavailable),
        )
        .route(
            "/api/settings/maintenance",
            get(unavailable).post(unavailable),
        )
        .route(
            "/api/settings/notifications",
            get(unavailable).post(unavailable),
        )
        .route("/api/settings/notifications/test", post(unavailable))
        .route("/api/settings/proxy-test", post(unavailable))
        .route(
            "/api/settings/webhook-events",
            get(unavailable).post(unavailable),
        )
        .route("/api/version", get(unavailable))
        .route("/api/version/update", post(unavailable))
        .route("/api/version/shutdown", post(unavailable))
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
fn safe_settings(mut value: Value) -> Value {
    if let Some(o) = value.as_object_mut() {
        let password = o.remove("password");
        let secret = o.remove("oidcClientSecret");
        o.remove("mitmSudoEncrypted");
        o.remove("ssoLastTestOk");
        let configured = [
            o.get("oidcIssuerUrl"),
            o.get("oidcClientId"),
            secret.as_ref(),
        ]
        .iter()
        .all(|v| v.and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty()));
        o.insert("oidcConfigured".into(), json!(configured));
        o.insert(
            "hasPassword".into(),
            json!(
                password.is_some_and(|v| v.as_str().is_some_and(|s| !s.is_empty()))
                    || std::env::var("INITIAL_PASSWORD").is_ok_and(|s| !s.is_empty())
            ),
        );
        o.insert(
            "enableRequestLogs".into(),
            json!(std::env::var("ENABLE_REQUEST_LOGS").as_deref() == Ok("true")),
        );
        o.insert(
            "enableTranslator".into(),
            json!(std::env::var("ENABLE_TRANSLATOR").as_deref() == Ok("true")),
        );
        if let Ok(path) = crate::db::data_file() {
            o.insert("databasePath".into(), json!(path.to_string_lossy()));
        }
    }
    value
}
async fn settings(State(s): State<AppState>) -> Response {
    match s
        .db
        .lock()
        .ok()
        .and_then(|db| crate::db::settings_get(&db).ok())
    {
        Some(v) => (
            [(header::CACHE_CONTROL, "no-store")],
            Json(safe_settings(v)),
        )
            .into_response(),
        None => error(StatusCode::INTERNAL_SERVER_ERROR, "Failed to read settings"),
    }
}
async fn update_settings(State(s): State<AppState>, Json(body): Json<Value>) -> Response {
    let Some(mut patch) = body.as_object().cloned() else {
        return error(StatusCode::BAD_REQUEST, "Expected object");
    };
    let Ok(db) = s.db.lock() else {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "Database unavailable");
    };
    let Ok(mut current) = crate::db::settings_get(&db) else {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "Failed to read settings");
    };
    let Some(obj) = current.as_object_mut() else {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "Invalid settings");
    };
    for key in ["password", "mitmSudoEncrypted", "ssoLastTestOk"] {
        patch.remove(key);
    }
    if let Some(new) = patch.remove("newPassword") {
        let Some(new) = new.as_str().filter(|s| !s.is_empty()) else {
            return error(StatusCode::BAD_REQUEST, "Invalid new password");
        };
        if new.chars().count() < 8 {
            return error(
                StatusCode::BAD_REQUEST,
                "New password must be at least 8 characters",
            );
        }
        let supplied = patch.remove("currentPassword");
        let supplied = supplied.as_ref().and_then(Value::as_str).unwrap_or("");
        if let Some(hash) = obj
            .get("password")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            if supplied.is_empty() {
                return error(StatusCode::BAD_REQUEST, "Current password required");
            }
            if !bcrypt::verify(supplied, hash).unwrap_or(false) {
                return error(StatusCode::UNAUTHORIZED, "Invalid current password");
            }
        } else if !supplied.is_empty()
            && supplied != "9routerpro"
            && std::env::var("INITIAL_PASSWORD").as_deref() != Ok(supplied)
        {
            return error(StatusCode::UNAUTHORIZED, "Invalid current password");
        }
        let Ok(hash) = bcrypt::hash(new, 10) else {
            return error(StatusCode::INTERNAL_SERVER_ERROR, "Failed to hash password");
        };
        patch.insert("password".into(), json!(hash));
    } else {
        patch.remove("currentPassword");
    }
    // ponytail: SSO test gate is unavailable, so fail closed rather than allow lockout.
    if patch
        .get("authMode")
        .and_then(Value::as_str)
        .is_some_and(|v| v != "password")
        || (obj
            .get("authMode")
            .and_then(Value::as_str)
            .is_some_and(|v| v != "password")
            && [
                "samlEntryPoint",
                "samlIssuer",
                "samlCert",
                "oidcIssuerUrl",
                "oidcClientId",
                "oidcClientSecret",
                "ssoType",
            ]
            .iter()
            .any(|k| patch.contains_key(*k)))
    {
        return error(
            StatusCode::NOT_IMPLEMENTED,
            "SSO settings are not implemented",
        );
    }
    if patch
        .get("oidcClientSecret")
        .is_some_and(|v| v.as_str().is_none_or(|s| s.trim().is_empty()))
    {
        patch.remove("oidcClientSecret");
    }
    for key in ["responseCache", "dbMaintenance"] {
        if let Some(Value::Object(next)) = patch.get_mut(key) {
            if let Some(Value::Object(old)) = obj.get(key) {
                let mut merged = old.clone();
                merged.append(next);
                *next = merged;
            }
        }
    }
    obj.extend(patch);
    if crate::db::settings_put(&db, &current).is_err() {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "Failed to save settings");
    }
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(safe_settings(current)),
    )
        .into_response()
}
async fn require_login(State(s): State<AppState>) -> Response {
    let v =
        s.db.lock()
            .ok()
            .and_then(|db| crate::db::settings_get(&db).ok())
            .unwrap_or(json!({}));
    Json(json!({"requireLogin":v["requireLogin"] != false,"tunnelDashboardAccess":v["tunnelDashboardAccess"] != false,"tunnelUrl":v["tunnelUrl"].as_str().unwrap_or(""),"tailscaleUrl":v["tailscaleUrl"].as_str().unwrap_or("")})).into_response()
}
async fn status(State(s): State<AppState>) -> Response {
    let v =
        s.db.lock()
            .ok()
            .and_then(|db| crate::db::settings_get(&db).ok())
            .unwrap_or(json!({}));
    Json(json!({"requireLogin":v["requireLogin"] != false,"authMode":v["authMode"].as_str().unwrap_or("password"),"ssoType":v["ssoType"].as_str().unwrap_or("oidc"),"oidcConfigured":false,"oidcLoginLabel":v["oidcLoginLabel"].as_str().unwrap_or("Sign in with OIDC"),"samlConfigured":false,"samlLoginLabel":v["samlLoginLabel"].as_str().unwrap_or("Sign in with SAML SSO"),"hasPassword":v["password"].as_str().is_some_and(|s|!s.is_empty()),"totpEnabled":false,"displayName":"Password user","loginMethod":"Password","authenticated":true,"oidcName":null,"oidcEmail":null,"oidcLogin":false,"samlName":null,"samlEmail":null,"samlLogin":false})).into_response()
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
