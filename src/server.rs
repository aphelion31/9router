use crate::{
    api::{ApiKey, KeyAccess, executor::HttpExecutor},
    dashboard,
    handlers::{
        BoxFuture, Credentials,
        chat::{self, ChatHooks, UsageRecord},
    },
};
use axum::{
    Json, Router,
    body::Body,
    extract::{OriginalUri, Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone)]
struct AppState {
    db: Arc<Mutex<Connection>>,
    executor: Arc<HttpExecutor>,
    jwt_secret: Option<Vec<u8>>,
}

pub fn router() -> Router {
    let db = crate::db::open(&crate::db::data_file().expect("data directory")).expect("database");
    let secret = std::env::var("JWT_SECRET")
        .ok()
        .or_else(|| {
            std::env::var("DATA_DIR")
                .ok()
                .or_else(|| std::env::var("HOME").ok().map(|h| format!("{h}/.9router")))
                .and_then(|d| std::fs::read_to_string(format!("{d}/jwt-secret")).ok())
        })
        .map(|s| s.trim().as_bytes().to_vec());
    make_router(db, secret)
}

fn make_router(db: Connection, jwt_secret: Option<Vec<u8>>) -> Router {
    make_router_with_db(Arc::new(Mutex::new(db)), jwt_secret).0
}

/// Builds the router around a shared DB handle. The handle lets tests inspect rows.
fn make_router_with_db(
    db: Arc<Mutex<Connection>>,
    jwt_secret: Option<Vec<u8>>,
) -> (Router, Arc<Mutex<Connection>>) {
    let state = AppState {
        db: Arc::clone(&db),
        executor: Arc::new(HttpExecutor(reqwest::Client::new())),
        jwt_secret,
    };
    let management = Router::new()
        .route("/api/keys", get(keys).post(create_key).patch(bulk_keys))
        .route(
            "/api/keys/{id}",
            get(key).put(update_key).delete(delete_key),
        )
        .route("/api/providers", get(providers))
        .route("/api/combos", get(combos))
        .route("/api/settings", get(settings).put(save_settings))
        .route("/api/usage", get(usage))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            dashboard_auth,
        ));
    let v1 = Router::new()
        .route("/v1/chat/completions", post(completion))
        .route("/v1/messages", post(completion))
        .route("/v1/responses", post(completion))
        .route("/v1/models", get(models))
        .route("/v1beta/models/{*path}", post(gemini))
        .route_layer(middleware::from_fn_with_state(state.clone(), api_auth));
    let dashboard_pages = Router::new()
        .route("/dashboard", get(dashboard::overview_page))
        .route("/dashboard/endpoint", get(dashboard::endpoint_page))
        .route("/dashboard/providers", get(dashboard::providers_page))
        .route("/dashboard/providers/new", get(dashboard::providers_new_page))
        .route("/dashboard/providers/{id}", get(dashboard::provider_detail_page))
        .route("/dashboard/combos", get(dashboard::combos_page))
        .route("/dashboard/usage", get(dashboard::usage_page))
        .route("/dashboard/quota", get(dashboard::quota_page))
        .route("/dashboard/token-saver", get(dashboard::token_saver_page))
        .route("/dashboard/prompt-injection", get(dashboard::prompt_injection_page))
        .route("/dashboard/profile", get(dashboard::profile_page))
        .route("/dashboard/skills", get(dashboard::skills_page))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            dashboard_page_auth,
        ));
    let router = Router::new()
        .route("/", get(|| async { Redirect::to("/dashboard") }))
        .route("/login", get(dashboard::login_page))
        .route("/dashboard/style.css", get(dashboard::style_css))
        .route("/dashboard/app.js", get(dashboard::app_js))
        .route("/api/auth/login", post(auth_login))
        .route("/api/auth/logout", post(auth_logout))
        .route(
            "/api/health",
            get(|| async { Json(json!({"status":"ok"})) }),
        )
        .merge(dashboard_pages)
        .merge(management)
        .merge(v1)
        .with_state(state);
    (router, db)
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({"error":"Unauthorized"})),
    )
        .into_response()
}
fn has_valid_auth_cookie(headers: &HeaderMap, secret: Option<&[u8]>) -> bool {
    let token = headers
        .get("cookie")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.split(';')
                .map(str::trim)
                .find_map(|c| c.strip_prefix("auth_token="))
        });
    token.zip(secret).is_some_and(|(token, secret)| {
        let parts: Vec<_> = token.split('.').collect();
        if parts.len() != 3 {
            return false;
        }
        let header: Value = URL_SAFE_NO_PAD
            .decode(parts[0])
            .ok()
            .and_then(|v| serde_json::from_slice(&v).ok())
            .unwrap_or(Value::Null);
        let payload: Value = URL_SAFE_NO_PAD
            .decode(parts[1])
            .ok()
            .and_then(|v| serde_json::from_slice(&v).ok())
            .unwrap_or(Value::Null);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        header["alg"] == "HS256"
            && payload["exp"].as_u64().is_some_and(|exp| exp > now)
            && URL_SAFE_NO_PAD.decode(parts[2]).ok().is_some_and(|sig| {
                ring::hmac::verify(
                    &ring::hmac::Key::new(ring::hmac::HMAC_SHA256, secret),
                    format!("{}.{}", parts[0], parts[1]).as_bytes(),
                    &sig,
                )
                .is_ok()
            })
    })
}
async fn dashboard_auth(
    State(s): State<AppState>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    if !has_valid_auth_cookie(request.headers(), s.jwt_secret.as_deref()) {
        return unauthorized();
    }
    next.run(request).await
}
async fn dashboard_page_auth(
    State(s): State<AppState>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    if !has_valid_auth_cookie(request.headers(), s.jwt_secret.as_deref()) {
        return Redirect::to("/login").into_response();
    }
    next.run(request).await
}
async fn auth_login(State(s): State<AppState>, Json(body): Json<Value>) -> Response {
    let Some(password) = body["password"].as_str() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let ok = s
        .db
        .lock()
        .ok()
        .is_some_and(|db| dashboard::verify_password(&db, password));
    if !ok {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Invalid password"})),
        )
            .into_response();
    }
    let Some(secret) = s.jwt_secret.as_deref() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "JWT secret not configured"})),
        )
            .into_response();
    };
    let token = dashboard::issue_auth_token(secret);
    let Ok(cookie) = HeaderValue::from_str(&dashboard::auth_cookie_header(&token)) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    (
        StatusCode::OK,
        [(header::SET_COOKIE, cookie)],
        Json(json!({"success": true})),
    )
        .into_response()
}
async fn auth_logout() -> Response {
    (
        StatusCode::OK,
        [(
            header::SET_COOKIE,
            HeaderValue::from_static(dashboard::clear_auth_cookie_header()),
        )],
        Json(json!({"success": true})),
    )
        .into_response()
}
async fn api_auth(
    State(s): State<AppState>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    let headers = request.headers();
    let key = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .or_else(|| {
            headers
                .get("x-api-key")
                .or_else(|| headers.get("x-goog-api-key"))
                .and_then(|v| v.to_str().ok())
        });
    let valid = key.is_some_and(|key| s.db.lock().ok().and_then(|db| db.query_row("SELECT 1 FROM apiKeys WHERE key=?1 AND isActive=1 AND (expiresAt IS NULL OR expiresAt > strftime('%Y-%m-%dT%H:%M:%fZ','now')) AND COALESCE(accessRestricted,0)=0", [key], |_|Ok(())).optional().ok().flatten()).is_some());
    // Restricted keys fail closed until model access policies are enforced.
    if !valid {
        return unauthorized();
    }
    next.run(request).await
}
fn read_keys(db: &Connection, id: Option<&str>) -> rusqlite::Result<Vec<ApiKey>> {
    let mut stmt = db.prepare("SELECT id,key,name,machineId,isActive,createdAt,expiresAt,accessRestricted,accessAllow FROM apiKeys WHERE (?1 IS NULL OR id=?1) ORDER BY createdAt")?;
    let rows = stmt.query_map([id], |r| {
        let restricted = r.get::<_, Option<i64>>(7)?.unwrap_or(0) != 0;
        let raw: Option<String> = r.get(8)?;
        let allow = if restricted {
            raw.and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
                .unwrap_or_default()
        } else {
            vec![]
        };
        Ok(ApiKey {
            id: r.get(0)?,
            key: r.get(1)?,
            name: r.get(2)?,
            machine_id: r.get(3)?,
            is_active: r.get::<_, Option<i64>>(4)?.unwrap_or(0) != 0,
            created_at: r.get(5)?,
            expires_at: r.get(6)?,
            access: KeyAccess { restricted, allow },
            quota: None,
            limits: None,
        })
    })?;
    rows.collect()
}
async fn keys(State(s): State<AppState>) -> Response {
    match s.db.lock().ok().and_then(|db| read_keys(&db, None).ok()) {
        Some(keys) => Json(json!({"keys":keys})).into_response(),
        None => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
async fn key(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    match s
        .db
        .lock()
        .ok()
        .and_then(|db| read_keys(&db, Some(&id)).ok())
        .and_then(|mut keys| keys.pop())
    {
        Some(key) => Json(json!({"key":key})).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
async fn create_key(State(s): State<AppState>, Json(body): Json<Value>) -> Response {
    use ring::rand::SecureRandom;
    let Some(name) = body["name"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let mut bytes = [0u8; 32];
    if ring::rand::SystemRandom::new().fill(&mut bytes).is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let mut id_bytes = [0u8; 16];
    if ring::rand::SystemRandom::new().fill(&mut id_bytes).is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let id = URL_SAFE_NO_PAD.encode(id_bytes);
    let key = body["customKey"]
        .as_str()
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("sk-{}", URL_SAFE_NO_PAD.encode(bytes)));
    let result=s.db.lock().ok().and_then(|db|db.execute("INSERT INTO apiKeys(id,key,name,isActive,createdAt,accessRestricted,accessAllow) VALUES(?1,?2,?3,1,strftime('%Y-%m-%dT%H:%M:%fZ','now'),0,'[]')",params![id,key,name]).ok());
    if result.is_none() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    (
        StatusCode::CREATED,
        Json(json!({"id":id,"key":key,"name":name})),
    )
        .into_response()
}
async fn update_key(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let Ok(db) = s.db.lock() else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let Some(mut key) = read_keys(&db, Some(&id)).ok().and_then(|mut v| v.pop()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if let Some(name) = body.get("name") {
        let Some(name) = name.as_str().map(str::trim).filter(|s| !s.is_empty()) else {
            return StatusCode::BAD_REQUEST.into_response();
        };
        key.name = Some(name.to_owned())
    }
    if let Some(active) = body.get("isActive") {
        let Some(active) = active.as_bool() else {
            return StatusCode::BAD_REQUEST.into_response();
        };
        key.is_active = active
    }
    if let Some(access) = body.get("access") {
        if !access.get("restricted").is_some_and(Value::is_boolean) {
            return StatusCode::BAD_REQUEST.into_response();
        }
        let Ok(value) = serde_json::from_value::<KeyAccess>(access.clone()) else {
            return StatusCode::BAD_REQUEST.into_response();
        };
        key.access = value;
        if !key.access.restricted {
            key.access.allow.clear()
        }
    }
    let allow = serde_json::to_string(&key.access.allow).unwrap();
    if db
        .execute(
            "UPDATE apiKeys SET name=?1,isActive=?2,accessRestricted=?3,accessAllow=?4 WHERE id=?5",
            params![key.name, key.is_active, key.access.restricted, allow, id],
        )
        .is_err()
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    Json(json!({"key":key})).into_response()
}
async fn delete_key(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    match s
        .db
        .lock()
        .ok()
        .and_then(|db| db.execute("DELETE FROM apiKeys WHERE id=?1", [id]).ok())
    {
        Some(0) => StatusCode::NOT_FOUND.into_response(),
        Some(_) => Json(json!({"success":true})).into_response(),
        None => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
async fn bulk_keys(State(s): State<AppState>, Json(body): Json<Value>) -> Response {
    let (Some(ids), Some(active)) = (body["ids"].as_array(), body["isActive"].as_bool()) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if ids.iter().any(|id| id.as_str().is_none()) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Ok(mut db) = s.db.lock() else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let Ok(tx) = db.transaction() else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    for id in ids {
        if tx
            .execute(
                "UPDATE apiKeys SET isActive=?1 WHERE id=?2",
                params![active, id.as_str()],
            )
            .is_err()
        {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }
    if tx.commit().is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    Json(json!({"success":true})).into_response()
}
async fn providers(State(s): State<AppState>) -> Response {
    let rows = s.db.lock().ok().and_then(|db| { let mut stmt=db.prepare("SELECT id,provider,name,isActive FROM providerConnections").ok()?; let rows=stmt.query_map([],|r|Ok(json!({"id":r.get::<_,String>(0)?,"provider":r.get::<_,String>(1)?,"name":r.get::<_,Option<String>>(2)?,"isActive":r.get::<_,Option<i64>>(3)?.unwrap_or(0)!=0}))).ok()?.collect::<rusqlite::Result<Vec<_>>>().ok(); rows });
    match rows {
        Some(rows) => Json(json!({"connections":rows})).into_response(),
        None => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
async fn combos(State(s): State<AppState>) -> Response {
    let rows = s.db.lock().ok().and_then(|db| {let mut st=db.prepare("SELECT id,name,kind,models,createdAt,updatedAt FROM combos").ok()?; let rows=st.query_map([],|r| {let models:String=r.get(3)?;Ok(json!({"id":r.get::<_,String>(0)?,"name":r.get::<_,String>(1)?,"kind":r.get::<_,Option<String>>(2)?,"models":serde_json::from_str::<Value>(&models).unwrap_or(json!([])),"createdAt":r.get::<_,String>(4)?,"updatedAt":r.get::<_,String>(5)?}))}).ok()?.collect::<rusqlite::Result<Vec<_>>>().ok(); rows});
    match rows {
        Some(rows) => Json(json!({"combos":rows})).into_response(),
        None => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
/// Usage totals for the last `days` (default 7, max 366).
async fn usage(State(s): State<AppState>, Query(q): Query<HashMap<String, String>>) -> Response {
    let days = q.get("days").and_then(|d| d.parse::<u32>().ok()).unwrap_or(7).clamp(1, 366);
    let Ok(db) = s.db.lock() else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let since: Result<String, _> = db.query_row(
        "SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now', ?1)",
        [format!("-{days} days")],
        |r| r.get(0),
    );
    match since.ok().and_then(|since| crate::db::usage_summary(&db, &since).ok()) {
        Some(summary) => Json(summary).into_response(),
        None => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
/// Settings document (`settings` row 1). Empty object when never saved.
async fn settings(State(s): State<AppState>) -> Response {
    match s.db.lock().ok().and_then(|db| crate::db::settings_get(&db).ok()) {
        Some(v) => Json(v).into_response(),
        None => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
/// Replaces the settings document. Body must be a JSON object.
async fn save_settings(State(s): State<AppState>, Json(body): Json<Value>) -> Response {
    if !body.is_object() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    match s.db.lock().ok().and_then(|db| crate::db::settings_put(&db, &body).ok()) {
        Some(()) => Json(body).into_response(),
        None => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
async fn models() -> Json<Value> {
    Json(
        json!({"object":"list","data":crate::providers::all().iter().flat_map(|p|p.models.iter().map(move |m|json!({"id":format!("{}/{}",p.id,m.id),"object":"model","owned_by":p.id}))).collect::<Vec<_>>()}),
    )
}
/// Per-request chat hooks. `api_key_id` is the `apiKeys.id` of the caller, if known.
struct Hooks {
    state: AppState,
    api_key_id: Option<String>,
}
impl ChatHooks for Hooks {
    fn on_usage_record<'a>(&'a self, r: UsageRecord) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let Ok(db) = self.state.db.lock() else { return };
            let row = crate::db::UsageRow {
                timestamp: String::new(),
                provider: r.provider,
                model: r.model,
                connection_id: r.connection_id,
                api_key: self.api_key_id.clone(),
                endpoint: None,
                prompt_tokens: r.prompt_tokens as i64,
                completion_tokens: r.completion_tokens as i64,
                status: r.status as i64,
                latency_ms: r.latency_ms as i64,
                request_id: uuid_like(),
            };
            if let Err(err) = crate::db::usage_insert(&db, &row) {
                tracing::warn!("usage insert failed: {err}");
            }
        })
    }
    fn get_credentials<'a>(
        &'a self,
        provider: &'a str,
        _: &'a str,
        excluded: &'a HashSet<String>,
    ) -> BoxFuture<'a, Option<Credentials>> {
        Box::pin(async move {
            let db = self.state.db.lock().ok()?;
            let mut st=db.prepare("SELECT id,name,data FROM providerConnections WHERE provider=?1 AND isActive=1 ORDER BY priority ASC").ok()?;
            let rows = st
                .query_map([provider], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })
                .ok()?;
            for row in rows.flatten() {
                if excluded.contains(&row.0) {
                    continue;
                }
                let data: Value = serde_json::from_str(&row.2).ok()?;
                return Some(Credentials {
                    connection_id: row.0,
                    connection_name: row.1,
                    access_token: data["accessToken"].as_str().map(str::to_owned),
                    refresh_token: data["refreshToken"].as_str().map(str::to_owned),
                    api_key: data["apiKey"].as_str().map(str::to_owned),
                    provider_specific_data: data["providerSpecificData"].clone(),
                    ..Default::default()
                });
            }
            None
        })
    }
    fn get_combo_models<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Option<Vec<String>>> {
        Box::pin(async move {
            let db = self.state.db.lock().ok()?;
            let models: String = db
                .query_row("SELECT models FROM combos WHERE name=?1", [name], |r| {
                    r.get(0)
                })
                .ok()?;
            serde_json::from_str(&models).ok()
        })
    }
}
/// Resolves the request's API key to its `apiKeys.id` for usage attribution.
fn api_key_id(s: &AppState, headers: &HeaderMap) -> Option<String> {
    let key = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .or_else(|| {
            headers
                .get("x-api-key")
                .or_else(|| headers.get("x-goog-api-key"))
                .and_then(|v| v.to_str().ok())
        })?;
    s.db.lock()
        .ok()?
        .query_row("SELECT id FROM apiKeys WHERE key=?1", [key], |r| r.get(0))
        .optional()
        .ok()
        .flatten()
}

/// Random 128-bit hex request ID.
fn uuid_like() -> String {
    use ring::rand::SecureRandom;
    let mut b = [0u8; 16];
    let _ = ring::rand::SystemRandom::new().fill(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

async fn completion(
    State(s): State<AppState>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let hooks = Hooks {
        api_key_id: api_key_id(&s, &headers),
        state: s.clone(),
    };
    chat::handle_chat(
        body,
        &headers,
        Some(uri.path()),
        s.executor.as_ref(),
        Arc::new(hooks),
    )
    .await
}
async fn gemini(
    State(s): State<AppState>,
    Path(path): Path<String>,
    headers: HeaderMap,
    Json(mut body): Json<Value>,
) -> Response {
    let Some(model) = path.strip_suffix(":generateContent") else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(obj) = body.as_object_mut() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    obj.insert("model".into(), json!(model));
    let hooks = Hooks {
        api_key_id: api_key_id(&s, &headers),
        state: s.clone(),
    };
    chat::handle_chat(
        body,
        &headers,
        Some("/v1beta/models"),
        s.executor.as_ref(),
        Arc::new(hooks),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::to_bytes, http::Request};
    use tower::ServiceExt;
    fn test_router() -> Router {
        let db = Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        db.execute("INSERT INTO apiKeys(id,key,createdAt,accessRestricted,accessAllow) VALUES('1','secret','now',1,?1)",params!["{\"restricted\":true}"]).unwrap();
        make_router(db, Some(b"test-secret".to_vec()))
    }
    fn token() -> String {
        let h = URL_SAFE_NO_PAD.encode(r#"{"alg":"HS256"}"#);
        let p = URL_SAFE_NO_PAD.encode(format!(
            "{{\"exp\":{}}}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
                + 60
        ));
        let input = format!("{h}.{p}");
        let sig = ring::hmac::sign(
            &ring::hmac::Key::new(ring::hmac::HMAC_SHA256, b"test-secret"),
            input.as_bytes(),
        );
        format!("{input}.{}", URL_SAFE_NO_PAD.encode(sig.as_ref()))
    }
    #[tokio::test]
    async fn legacy_access_returns_allow() {
        let res = test_router()
            .oneshot(
                Request::get("/api/keys")
                    .header("cookie", format!("auth_token={}", token()))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), 200);
        let body = to_bytes(res.into_body(), 10000).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            v["keys"][0]["access"],
            json!({"restricted":true,"allow":[]})
        );
    }
    #[tokio::test]
    async fn auth_required() {
        for path in ["/api/keys", "/v1/models"] {
            let r = test_router()
                .oneshot(Request::get(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(r.status(), 401);
        }
    }
    /// Starts a mock OpenAI-compatible upstream. Non-stream and stream responses are chosen by
    /// the request's `stream` flag. Returns its base URL.
    async fn mock_upstream() -> String {
        use axum::{Router as R, body::Bytes, routing::post as p};
        let app = R::new().route(
            "/v1/chat/completions",
            p(|body: Bytes| async move {
                let req: Value = serde_json::from_slice(&body).unwrap();
                if req["stream"] == true {
                    let frames = concat!(
                        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-test\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}\n\n",
                        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-test\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":3,\"total_tokens\":10}}\n\n",
                        "data: [DONE]\n\n"
                    );
                    ([("content-type", "text/event-stream")], frames).into_response()
                } else {
                    axum::Json(json!({
                        "id": "c1", "object": "chat.completion", "model": "gpt-test",
                        "choices": [{"index": 0, "message": {"role": "assistant", "content": "hi"}, "finish_reason": "stop"}],
                        "usage": {"prompt_tokens": 7, "completion_tokens": 3, "total_tokens": 10}
                    }))
                    .into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1/chat/completions", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        base
    }

    /// Router with one API key and one OpenAI connection pointed at `upstream`.
    fn e2e_router(upstream: &str) -> (Router, Arc<Mutex<Connection>>) {
        let db = Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        db.execute(
            "INSERT INTO apiKeys(id,key,name,createdAt) VALUES('k1','sk-test-key','t','now')",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO providerConnections(id,provider,authType,name,isActive,priority,data,createdAt,updatedAt)
             VALUES('c1','openai','apikey','mock',1,1,?1,'now','now')",
            [json!({"apiKey": "upstream-key", "providerSpecificData": {"baseUrl": upstream}}).to_string()],
        )
        .unwrap();
        make_router_with_db(Arc::new(Mutex::new(db)), Some(b"test-secret".to_vec()))
    }

    #[tokio::test]
    async fn chat_completions_stream_and_non_stream_record_usage() {
        let upstream = mock_upstream().await;
        let (app, db) = e2e_router(&upstream);
        let call = |stream: bool| {
            Request::post("/v1/chat/completions")
                .header("authorization", "Bearer sk-test-key")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"model":"openai/gpt-test","stream":stream,"messages":[{"role":"user","content":"hi"}]}).to_string(),
                ))
                .unwrap()
        };

        let res = app.clone().oneshot(call(false)).await.unwrap();
        assert_eq!(res.status(), 200);
        let v: Value = serde_json::from_slice(&to_bytes(res.into_body(), 100_000).await.unwrap()).unwrap();
        assert_eq!(v["choices"][0]["message"]["content"], "hi");

        let res = app.clone().oneshot(call(true)).await.unwrap();
        assert_eq!(res.status(), 200);
        let text = String::from_utf8(to_bytes(res.into_body(), 100_000).await.unwrap().to_vec()).unwrap();
        assert!(text.contains("\"content\":\"hi\""), "{text}");
        assert!(text.contains("[DONE]"), "{text}");

        // Usage is written after each completed call; the stream writes once it is drained.
        // Allow the spawned usage task to finish.
        let mut rows = 0i64;
        for _ in 0..50 {
            rows = {
                let conn = db.lock().unwrap();
                conn.query_row("SELECT COUNT(*) FROM usageHistory WHERE apiKey='k1' AND status='200'", [], |r| r.get(0)).unwrap()
            };
            if rows == 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(rows, 2);
        let conn = db.lock().unwrap();
        let (p, c): (i64, i64) = conn
            .query_row("SELECT SUM(promptTokens), SUM(completionTokens) FROM usageHistory", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!((p, c), (14, 6));
    }

    #[tokio::test]
    async fn health_ok() {
        let r = test_router()
            .oneshot(Request::get("/api/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
    }
    #[tokio::test]
    async fn dashboard_endpoint_redirects_when_unauthenticated() {
        let r = test_router()
            .oneshot(
                Request::get("/dashboard/endpoint")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(r.status().is_redirection());
        assert_eq!(r.headers().get("location").unwrap(), "/login");
    }
    #[tokio::test]
    async fn dashboard_endpoint_returns_html_when_authenticated() {
        let r = test_router()
            .oneshot(
                Request::get("/dashboard/endpoint")
                    .header("cookie", format!("auth_token={}", token()))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let ct = r
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(ct.contains("text/html"));
        let body = to_bytes(r.into_body(), 100_000).await.unwrap();
        let html = std::str::from_utf8(&body).unwrap();
        assert!(html.contains("Endpoint &amp; API Keys"));
    }
    #[tokio::test]
    async fn login_sets_auth_cookie_and_allows_dashboard() {
        let db = Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        crate::db::kv_set(&db, "settings", "password", "test-pass").unwrap();
        let app = make_router(db, Some(b"test-secret".to_vec()));

        let bad = app
            .clone()
            .oneshot(
                Request::post("/api/auth/login")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"password":"wrong"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bad.status(), 401);

        let res = app
            .clone()
            .oneshot(
                Request::post("/api/auth/login")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"password":"test-pass"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), 200);
        let cookie = res
            .headers()
            .get("set-cookie")
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        assert!(cookie.starts_with("auth_token="));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Lax"));

        let cookie_pair = cookie.split(';').next().unwrap();
        let page = app
            .oneshot(
                Request::get("/dashboard/endpoint")
                    .header("cookie", cookie_pair)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(page.status(), 200);
    }
}
