use crate::{
    api::{ApiKey, KeyAccess, executor::HttpExecutor},
    dashboard,
    handlers::{
        BoxFuture, Credentials,
        chat::{self, ChatHooks},
    },
};
use axum::{
    Json, Router,
    body::Body,
    extract::{OriginalUri, Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
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
    let state = AppState {
        db: Arc::new(Mutex::new(db)),
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
        .route("/api/settings", get(settings))
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
        .route("/dashboard/combos", get(dashboard::combos_page))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            dashboard_page_auth,
        ));
    Router::new()
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
        .with_state(state)
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
async fn settings() -> Json<Value> {
    Json(json!({}))
}
async fn usage() -> Json<Value> {
    Json(json!({}))
}
async fn models() -> Json<Value> {
    Json(
        json!({"object":"list","data":crate::providers::all().iter().flat_map(|p|p.models.iter().map(move |m|json!({"id":format!("{}/{}",p.id,m.id),"object":"model","owned_by":p.id}))).collect::<Vec<_>>()}),
    )
}
struct Hooks(AppState);
impl ChatHooks for Hooks {
    fn get_credentials<'a>(
        &'a self,
        provider: &'a str,
        _: &'a str,
        excluded: &'a HashSet<String>,
    ) -> BoxFuture<'a, Option<Credentials>> {
        Box::pin(async move {
            let db = self.0.db.lock().ok()?;
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
            let db = self.0.db.lock().ok()?;
            let models: String = db
                .query_row("SELECT models FROM combos WHERE name=?1", [name], |r| {
                    r.get(0)
                })
                .ok()?;
            serde_json::from_str(&models).ok()
        })
    }
}
async fn completion(
    State(s): State<AppState>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    chat::handle_chat(
        body,
        &headers,
        Some(uri.path()),
        s.executor.as_ref(),
        Arc::new(Hooks(s.clone())),
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
    chat::handle_chat(
        body,
        &headers,
        Some("/v1beta/models"),
        s.executor.as_ref(),
        Arc::new(Hooks(s.clone())),
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
