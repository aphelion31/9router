use crate::{
    api::executor::HttpExecutor,
    dashboard,
    handlers::{
        BoxFuture, Credentials,
        chat::{self, ChatHooks, UsageRecord},
    },
};
use axum::{
    Json, Router,
    extract::{OriginalUri, Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rusqlite::{Connection, OptionalExtension};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

pub(crate) fn shutdown_notify() -> &'static tokio::sync::Notify {
    static SHUTDOWN: std::sync::OnceLock<tokio::sync::Notify> = std::sync::OnceLock::new();
    SHUTDOWN.get_or_init(tokio::sync::Notify::new)
}

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) db: Arc<Mutex<Connection>>,
    pub(crate) executor: Arc<HttpExecutor>,
    pub(crate) jwt_secret: Option<Vec<u8>>,
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
        executor: Arc::new(HttpExecutor(reqwest::Client::builder().connect_timeout(std::time::Duration::from_millis(crate::config::FETCH_CONNECT_TIMEOUT_MS)).build().expect("valid HTTP client"))),
        jwt_secret,
    };
    let management = Router::new()
        .route("/api/usage", get(usage))
        .merge(crate::api::routes::management())
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
        .merge(crate::api::routes::v1())
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
        .route("/dashboard/pxpipe", get(dashboard::pxpipe_page))
        .route("/dashboard/proxy-pools", get(dashboard::proxy_pools_page))
        .route("/dashboard/mitm", get(dashboard::mitm_page))
        .route("/dashboard/automation", get(dashboard::automation_page))
        .route("/dashboard/cli-tools", get(dashboard::cli_tools_page))
        .route("/dashboard/cli-tools/{toolId}", get(dashboard::cli_tool_detail_page))
        .route("/dashboard/basic-chat", get(dashboard::basic_chat_page))
        .route("/dashboard/console-log", get(dashboard::console_log_page))
        .route("/dashboard/translator", get(dashboard::translator_page))
        .route("/dashboard/media-providers/web", get(dashboard::media_web_page))
        .route("/dashboard/media-providers/combo/{id}", get(dashboard::media_combo_detail_page))
        .route("/dashboard/media-providers/{kind}", get(dashboard::media_kind_page))
        .route("/dashboard/media-providers/{kind}/{id}", get(dashboard::media_detail_page))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            dashboard_page_auth,
        ));
    let router = Router::new()
        .route("/", get(|| async { Redirect::to("/dashboard") }))
        .route("/login", get(dashboard::login_page))
        .route("/dashboard/style.css", get(dashboard::style_css))
        .route("/dashboard/app.js", get(dashboard::app_js))
        .merge(crate::api::routes::public())
        .merge(dashboard_pages)
        .merge(management)
        .merge(v1)
        .with_state(state);
    // Router::layer runs after route matching, so rewrite in an outer service instead.
    let router = Router::new().fallback_service(tower::ServiceExt::map_request(router, rewrite_uri));
    (router, db)
}

/// Mirrors upstream `next.config.mjs` rewrites plus the native `/api/v1*` paths Next.js serves.
fn rewrite_path(path: &str) -> Option<String> {
    let tail = |p: &str| path.strip_prefix(p).filter(|t| t.is_empty() || t.starts_with('/'));
    if let Some(t) = tail("/api/v1beta") {
        return Some(format!("/v1beta{t}"));
    }
    if let Some(t) = tail("/api/v1") {
        return Some(format!("/v1{t}"));
    }
    if let Some(t) = tail("/v1/v1") {
        return Some(format!("/v1{t}"));
    }
    if tail("/codex").is_some() || path == "/responses" {
        return Some("/v1/responses".into());
    }
    if path == "/systemone" {
        return Some("/v1/systemone".into());
    }
    None
}

fn rewrite_uri(mut req: axum::extract::Request) -> axum::extract::Request {
    if let Some(path) = rewrite_path(req.uri().path()) {
        let pq = match req.uri().query() {
            Some(q) => format!("{path}?{q}"),
            None => path,
        };
        let mut parts = req.uri().clone().into_parts();
        if let Ok(pq) = pq.parse() {
            parts.path_and_query = Some(pq);
            if let Ok(uri) = axum::http::Uri::from_parts(parts) {
                *req.uri_mut() = uri;
            }
        }
    }
    req
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

fn quota_verdict(db: &Connection, key_id: &str, key: &str) -> Option<Value> {
    let raw = crate::db::kv_get(db, "keyQuotas", key_id).ok()??;
    let q: Value = serde_json::from_str(&raw).ok()?;
    if q["enabled"] == false { return None; }
    let limit = q["limit"].as_i64().filter(|n| *n > 0)?;
    let hours = q["windowHours"].as_i64().filter(|h| [24, 168, 720].contains(h)).unwrap_or(24);
    let modifier = format!("-{hours} hours");
    let (used, cost): (i64, f64) = db.query_row("SELECT COALESCE(SUM(COALESCE(promptTokens,0)+COALESCE(completionTokens,0)),0), COALESCE(SUM(cost),0) FROM usageHistory WHERE (apiKey=?1 OR apiKey=?2) AND timestamp >= strftime('%Y-%m-%dT%H:%M:%fZ','now',?3)", rusqlite::params![key_id, key, modifier], |r| Ok((r.get(0)?, r.get(1)?))).ok()?;
    let cost_limit = q["costLimit"].as_f64().filter(|n| *n > 0.0);
    let cost_blocked = used < limit && cost_limit.is_some_and(|n| cost >= n);
    if used < limit && !cost_blocked { return None; }
    let name: Option<String> = db.query_row("SELECT name FROM apiKeys WHERE id=?1", [key_id], |r| r.get(0)).ok().flatten();
    Some(json!({"keyName":name,"used":used,"limit":limit,"windowHours":hours,"costBlocked":cost_blocked,"costUsed":if cost_blocked { Some(cost) } else { None },"costLimit":cost_limit}))
}

async fn api_auth(
    State(s): State<AppState>,
    mut request: axum::extract::Request,
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
    let Some((key_id, restricted, allow)) = key.and_then(|key| s.db.lock().ok().and_then(|db| db.query_row("SELECT id, COALESCE(accessRestricted,0), accessAllow FROM apiKeys WHERE key=?1 AND isActive=1 AND (expiresAt IS NULL OR expiresAt > strftime('%Y-%m-%dT%H:%M:%fZ','now'))", [key], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, Option<String>>(2)?))).optional().ok().flatten())) else {
        return unauthorized();
    };
    let raw_key = key.unwrap_or("").to_owned();
    if restricted != 0 {
        let access = match s.db.lock() { Ok(db) => crate::services::key_access::KeyAccess::load(&db, allow), Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response() };
        let path = request.uri().path().to_owned();
        if request.method() == axum::http::Method::POST {
            let (parts, body) = request.into_parts();
            let bytes = match axum::body::to_bytes(body, 2 * 1024 * 1024).await { Ok(b) => b, Err(_) => return StatusCode::PAYLOAD_TOO_LARGE.into_response() };
            let value: Value = serde_json::from_slice(&bytes).unwrap_or_default();
            let requested = if path.starts_with("/v1beta/models/") { path.strip_prefix("/v1beta/models/").and_then(|v| v.strip_suffix(":generateContent")).unwrap_or("") } else if path == "/v1/search" || path == "/v1/web/fetch" { value["provider"].as_str().or_else(|| value["model"].as_str()).unwrap_or("") } else { value["model"].as_str().unwrap_or("") };
            let auto = path == "/v1/chat/completions" && crate::services::auto_routing::parse_auto_target(requested).is_some();
            let target = crate::handlers::chat::strip_model_context_marker(requested);
            let allowed = auto || s.db.lock().ok().is_some_and(|db| if path == "/v1/search" || path == "/v1/web/fetch" { access.allows_provider(&db, requested) } else { access.allows(&db, target) });
            if !allowed { return crate::handlers::chat::error_response(403, &format!("This API key is not allowed to use model \"{requested}\"")); }
            request = axum::http::Request::from_parts(parts, axum::body::Body::from(bytes));
        } else if path != "/v1/models" {
            return crate::handlers::chat::error_response(403, "This API key is not allowed to use model \"\"");
        }
        request.extensions_mut().insert(access);
    }
    let has_quota = s.db.lock().ok().and_then(|db| crate::db::kv_get(&db, "keyQuotas", &key_id).ok().flatten()).and_then(|raw| serde_json::from_str::<Value>(&raw).ok()).is_some_and(|q| q["enabled"] != false && q["limit"].as_i64().is_some_and(|n| n > 0));
    let quota = s.db.lock().ok().and_then(|db| quota_verdict(&db, &key_id, &raw_key));
    if let Some(quota) = quota {
        crate::services::notify::emit(s.db.clone(), "quota", quota);
        return (StatusCode::TOO_MANY_REQUESTS, Json(json!({"error":"API key quota exceeded"}))).into_response();
    }
    // Per-key rpm/rph/rpd/concurrency admission. Lookup errors fail open.
    let limits = s
        .db
        .lock()
        .ok()
        .and_then(|db| crate::services::key_rate_limiter::load_limits(&db, &key_id));
    let limiter = crate::services::key_rate_limiter::limiter();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let slot = match limits {
        Some(limits) => {
            match limiter.try_admit(&key_id, limits, now) {
              Ok(slot) => Some(slot),
              Err(v) => {
                let msg = match v.kind {
                    "concurrency" => format!(
                        "API key concurrency limit exceeded ({} requests already in flight)",
                        v.limit
                    ),
                    kind => format!(
                        "API key rate limit exceeded ({}/{} requests in the {} window)",
                        v.used,
                        v.limit,
                        match kind {
                            "rpm" => "last minute",
                            "rph" => "last hour",
                            _ => "last 24 hours",
                        }
                    ),
                };
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    [(header::RETRY_AFTER, v.retry_after_sec.to_string())],
                    Json(json!({"error": msg})),
                )
                    .into_response();
              }
            }
        }
        None => None,
    };
    let response = next.run(request).await;
    let (parts, body) = response.into_parts();
    struct GuardedBody {
        stream: axum::body::BodyDataStream,
        slot: Option<crate::services::key_rate_limiter::Slot>,
        recheck: Option<(Arc<Mutex<Connection>>, String, String)>,
    }
    impl Drop for GuardedBody {
        fn drop(&mut self) {
            if let Some((db, key_id, key)) = self.recheck.take() {
                tokio::spawn(async move {
                    let verdict = db.lock().ok().and_then(|db| quota_verdict(&db, &key_id, &key));
                    if let Some(mut verdict) = verdict {
                        verdict.as_object_mut().unwrap().remove("costUsed");
                        verdict.as_object_mut().unwrap().remove("costLimit");
                        verdict["crossedMidRequest"] = json!(true);
                        crate::services::notify::emit(db, "quota", verdict);
                    }
                });
            }
        }
    }
    impl futures_core::Stream for GuardedBody {
        type Item = Result<axum::body::Bytes, axum::Error>;
        fn poll_next(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Option<Self::Item>> {
            let result = std::pin::Pin::new(&mut self.stream).poll_next(cx);
            if matches!(result, std::task::Poll::Ready(None) | std::task::Poll::Ready(Some(Err(_)))) {
                self.slot.take();
            }
            result
        }
    }
    if slot.is_none() && !has_quota { return Response::from_parts(parts, body); }
    Response::from_parts(parts, axum::body::Body::from_stream(GuardedBody { stream: body.into_data_stream(), slot, recheck: has_quota.then_some((s.db.clone(), key_id, raw_key)) }))
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
async fn models(State(state): State<AppState>, access: Option<axum::extract::Extension<crate::services::key_access::KeyAccess>>) -> Json<Value> {
    let connections: Vec<(String, Value)> = state.db.lock().ok().and_then(|db| {
        let mut stmt = db.prepare("SELECT id,provider,data FROM providerConnections WHERE isActive=1 ORDER BY priority ASC").ok()?;
        let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))).ok()?;
        Some(rows.flatten().filter_map(|(id, provider, raw)| {
            let mut data: Value = crate::crypto::parse_data(&raw)?;
            data.as_object_mut()?.insert("id".into(), json!(id));
            data.as_object_mut()?.insert("provider".into(), json!(provider));
            Some((provider, data))
        }).collect())
    }).unwrap_or_default();
    let mut live = HashMap::new();
    for (provider, conn) in connections {
        if !live.contains_key(&provider) {
            if let Some(models) = crate::api::routes::mgmt_providers::live_models(&provider, &conn, false).await.filter(|m| !m.is_empty()) {
                live.insert(provider, models);
            }
        }
    }
    let mut data = Vec::new();
    for p in crate::providers::all() {
        if let Some(models) = live.remove(&p.id) {
            data.extend(models.iter().filter_map(|m| m["id"].as_str()).map(|id| json!({"id":format!("{}/{}",p.id,id),"object":"model","owned_by":p.id})));
        } else {
            data.extend(p.models.iter().map(|m| json!({"id":format!("{}/{}",p.id,m.id),"object":"model","owned_by":p.id})));
        }
    }
    if let Ok(db) = state.db.lock() {
        if let Ok(mut stmt) = db.prepare("SELECT name FROM combos ORDER BY name") {
            if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) {
                data.extend(rows.flatten().map(|name| json!({"id":name,"object":"model","owned_by":"combo"})));
            }
        }
    }
    if let Some(axum::extract::Extension(access)) = access {
        if let Ok(db) = state.db.lock() { data.retain(|entry| access.allows_entry(&db, entry)); } else { data.clear(); }
    }
    Json(json!({"object":"list","data":data}))
}
/// Sticky-random current key per provider+model (upstream sticky-random). In-memory; dropped when the key locks.
static STICKY_CURRENT: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, String>>> = std::sync::LazyLock::new(Default::default);

fn sticky_forget(provider: &str, model: &str, connection_id: &str) {
    if let Ok(mut map) = STICKY_CURRENT.lock() {
        let key = format!("{provider}:{model}");
        if map.get(&key).is_some_and(|id| id == connection_id) { map.remove(&key); }
    }
}

fn rand_index(n: usize) -> usize {
    use ring::rand::SecureRandom;
    let mut b = [0u8; 8];
    let _ = ring::rand::SystemRandom::new().fill(&mut b);
    (u64::from_le_bytes(b) as usize) % n.max(1)
}

/// Per-request chat hooks. `api_key_id` is the `apiKeys.id` of the caller, if known.
struct Hooks {
    state: AppState,
    api_key_id: Option<String>,
}
impl ChatHooks for Hooks {
    fn pair_enabled<'a>(&'a self, name: &'a str) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            let Ok(db) = self.state.db.lock() else { return false };
            let load = |key| crate::db::kv_get(&db, "promptInjections", key).ok().flatten().and_then(|s| serde_json::from_str::<Value>(&s).ok()).unwrap_or_default();
            let global: Value = load("prompts");
            let keyed: Value = load("byKeyModel");
            let entry = if global[name].is_object() { &global[name] } else { &keyed[self.api_key_id.as_deref().unwrap_or("")][name] };
            entry["enabled"] == true
        })
    }
    fn capacity_connections<'a>(&'a self) -> BoxFuture<'a, Vec<Value>> {
        Box::pin(async move {
            let Ok(db) = self.state.db.lock() else { return Vec::new() };
            let Ok(mut st) = db.prepare("SELECT provider,isActive,data FROM providerConnections") else { return Vec::new() };
            let Ok(rows) = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?))) else { return Vec::new() };
            rows.flatten().filter_map(|(provider, active, raw)| {
                let mut data = crate::crypto::parse_data(&raw)?;
                data["provider"] = json!(provider);
                data["isActive"] = json!(active != 0);
                Some(data)
            }).collect()
        })
    }
    fn get_pinned_credentials<'a>(&'a self, provider: &'a str, model: &'a str, excluded: &'a HashSet<String>, pin: Option<&'a str>) -> BoxFuture<'a, Option<Credentials>> {
        Box::pin(async move {
            if let Some(pin) = pin.filter(|p| !excluded.contains(*p)) {
                let others = self.state.db.lock().ok().and_then(|db| {
                    let mut st = db.prepare("SELECT id FROM providerConnections WHERE provider=?1 AND isActive=1 AND id<>?2").ok()?;
                    let ids = st.query_map(rusqlite::params![provider, pin], |r| r.get::<_, String>(0)).ok()?.flatten().collect::<HashSet<_>>();
                    Some(ids)
                });
                if let Some(mut others) = others {
                    others.extend(excluded.iter().cloned());
                    if let Some(creds) = self.get_credentials(provider, model, &others).await {
                        if creds.connection_id == pin { return Some(creds); }
                    }
                }
            }
            self.get_credentials(provider, model, excluded).await
        })
    }
    fn filter_adapter_models<'a>(&'a self, augmented: Vec<String>, original: &'a [String]) -> BoxFuture<'a, Vec<String>> {
        Box::pin(async move {
            let Ok(db) = self.state.db.lock() else { return original.to_vec(); };
            let Some(id) = &self.api_key_id else { return augmented; };
            let record = db.query_row("SELECT COALESCE(accessRestricted,0),accessAllow FROM apiKeys WHERE id=?1", [id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?))).optional();
            match record {
                Ok(Some((0, _))) => augmented,
                Ok(Some((_, allow))) => {
                    let access = crate::services::key_access::KeyAccess::load(&db, allow);
                    augmented.into_iter().filter(|m| original.contains(m) || access.allows(&db, m)).collect()
                }
                _ => original.to_vec(),
            }
        })
    }
    fn settings<'a>(&'a self) -> BoxFuture<'a, Value> {
        Box::pin(async move {
            self.state.db.lock().ok().and_then(|db| crate::db::settings_get(&db).ok()).unwrap_or_default()
        })
    }
    fn smart_choice<'a>(&'a self, body: &'a Value) -> BoxFuture<'a, Option<Value>> {
        Box::pin(async move {
            let model = body["model"].as_str()?.trim();
            let resolved = if model.contains('/') { model.to_owned() } else {
                let db = self.state.db.lock().ok()?;
                let raw = crate::db::kv_get(&db, "modelAliases", model).ok().flatten()?;
                let value: Value = serde_json::from_str(&raw).ok()?;
                value.as_str().map(str::to_owned).or_else(|| Some(format!("{}/{}", value["provider"].as_str()?, value["model"].as_str()?)))?
            };
            let mut target = crate::handlers::chat::parse_model_target(&resolved)?;
            if crate::providers::lookup(&target.provider).is_none() {
                if let Some(id) = self.resolve_node_prefix(&target.provider).await { target.provider = id; }
            }
            self.get_credentials(&target.provider, &target.model, &std::collections::HashSet::new()).await?;
            let mut request = body.clone();
            request["model"] = json!(format!("{}/{}", target.provider, target.model));
            let response = crate::handlers::media::systemone::handle_systemone(State(self.state.clone()), Json(request)).await;
            if !response.status().is_success() { return None; }
            let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024).await.ok()?;
            serde_json::from_slice(&bytes).ok()
        })
    }
    fn prompt_injections<'a>(&'a self, model: &'a str) -> BoxFuture<'a, Vec<Value>> {
        Box::pin(async move {
            let Ok(db) = self.state.db.lock() else { return Vec::new() };
            let load = |key| crate::db::kv_get(&db, "promptInjections", key).ok().flatten().and_then(|raw| serde_json::from_str::<Value>(&raw).ok()).unwrap_or_default();
            let global = load("global");
            let by_key = load("byKey");
            let by_model = load("prompts");
            let by_key_model = load("byKeyModel");
            let variants = load("variants");
            let key = self.api_key_id.as_deref().unwrap_or("");
            let variant = &variants[model];
            let model_entry = if !model.contains('/') && variant.is_object() && variant["enabled"] == true { variant.clone() } else { by_model[model].clone() };
            [global, by_key[key].clone(), model_entry, if !model.contains('/') && variant.is_object() && variant["enabled"] == true { Value::Null } else { by_key_model[key][model].clone() }]
                .into_iter().filter(|entry| entry.is_object() && entry["enabled"] != false).collect()
        })
    }
    fn anti_flaky_config<'a>(&'a self) -> BoxFuture<'a, (bool, u8)> {
        Box::pin(async move {
            let settings = self.state.db.lock().ok().and_then(|db| crate::db::settings_get(&db).ok()).unwrap_or_default();
            crate::services::flaky_guard::resolve_anti_flaky_config(&settings)
        })
    }
    fn resolve_node_prefix<'a>(&'a self, prefix: &'a str) -> BoxFuture<'a, Option<String>> {
        Box::pin(async move {
            let db = self.state.db.lock().ok()?;
            let mut st = db
                .prepare("SELECT id,type,data FROM providerNodes ORDER BY createdAt")
                .ok()?;
            let rows = st
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, Option<String>>(2)?)))
                .ok()?;
            // Upstream order: openai-compatible, then anthropic-compatible, then custom-embedding.
            let mut found: Vec<(u8, String)> = rows
                .flatten()
                .filter_map(|(id, kind, data)| {
                    let data: Value = serde_json::from_str(data.as_deref().unwrap_or("{}")).ok()?;
                    (data["prefix"].as_str() == Some(prefix)).then_some(())?;
                    let rank = match kind.as_deref() {
                        Some("openai-compatible") => 0,
                        Some("anthropic-compatible") => 1,
                        Some("custom-embedding") => 2,
                        _ => return None,
                    };
                    Some((rank, id))
                })
                .collect();
            found.sort_by_key(|(rank, _)| *rank);
            found.into_iter().next().map(|(_, id)| id)
        })
    }
    fn on_error<'a>(&'a self, status: u16, message: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            crate::services::notify::emit(self.state.db.clone(), "error", json!({"status":status,"error":message.chars().take(500).collect::<String>()}));
        })
    }
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
                meta: Value::Null,
            };
            let tokens = crate::services::pricing::TokenCounts { prompt_tokens: row.prompt_tokens, completion_tokens: row.completion_tokens, cached_tokens: r.cached_tokens as i64, cache_creation_tokens: r.cache_creation_tokens as i64, reasoning_tokens: r.reasoning_tokens as i64 };
            match crate::db::usage_record(&db, &row, &tokens, None) {
                Err(err) => tracing::warn!("usage insert failed: {err}"),
                Ok(false) => {}
                Ok(true) => crate::services::notify::emit(self.state.db.clone(), "usage", json!({
                    "provider": row.provider, "model": row.model, "connectionId": row.connection_id,
                    "apiKeyMasked": null, "endpoint": row.endpoint, "promptTokens": row.prompt_tokens,
                    "completionTokens": row.completion_tokens, "cost": crate::services::pricing::cost_for(&db, &row.provider, &row.model, &tokens), "status": row.status.to_string(), "requestId": row.request_id
                })),
            }
            let settings = crate::db::settings_get(&db).unwrap_or_default();
            let enabled = settings.get("observabilityEnabled").or_else(|| settings.get("enableRequestDetails")).and_then(Value::as_bool).unwrap_or(true);
            if enabled {
                let detail = json!({"id":row.request_id,"provider":row.provider,"model":row.model,"latency":{"total":row.latency_ms}}).to_string();
                if let Err(err) = db.execute("INSERT INTO requestDetails(id,timestamp,provider,model,connectionId,status,data) VALUES(?1,strftime('%Y-%m-%dT%H:%M:%fZ','now'),?2,?3,?4,?5,?6)", rusqlite::params![row.request_id, row.provider, row.model, row.connection_id, row.status.to_string(), detail]) {
                    tracing::warn!("request latency insert failed: {err}");
                }
            }
        })
    }
    fn get_credentials<'a>(
        &'a self,
        provider: &'a str,
        model: &'a str,
        excluded: &'a HashSet<String>,
    ) -> BoxFuture<'a, Option<Credentials>> {
        self.get_credentials_preferred(provider, model, excluded, None)
    }
    fn get_credentials_preferred<'a>(&'a self, provider: &'a str, model: &'a str, excluded: &'a HashSet<String>, preferred: Option<&'a str>) -> BoxFuture<'a, Option<Credentials>> {
        Box::pin(async move {
            if matches!(provider, "opencode" | "mimo-free") {
                if excluded.contains("noauth") { return None; }
                let db = self.state.db.lock().ok()?;
                let settings = crate::db::settings_get(&db).unwrap_or_default();
                let config = &settings["providerStrategies"][provider];
                let mut pool_id = config["proxyPoolId"].as_str().unwrap_or("").to_owned();
                if pool_id.is_empty() && config["rotateStrategy"].as_str().unwrap_or("none") != "none" {
                    let mut stmt = db.prepare("SELECT id,data FROM proxyPools WHERE isActive=1 ORDER BY createdAt DESC").ok()?;
                    let pools: Vec<Value> = stmt.query_map([], |row| {
                        let id: String = row.get(0)?;
                        let data: String = row.get(1)?;
                        let mut value: Value = serde_json::from_str(&data).unwrap_or(json!({}));
                        value["id"] = json!(id);
                        Ok(value)
                    }).ok()?.flatten().collect();
                    let selected: Vec<String> = config["proxyPoolIds"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect();
                    let ids = crate::services::connection_proxy::resolve_rotation_pool_ids(&pools, &selected);
                    static ROTATION: std::sync::OnceLock<std::sync::Mutex<crate::services::connection_proxy::ProxyRotation>> = std::sync::OnceLock::new();
                    pool_id = ROTATION.get_or_init(Default::default).lock().ok()?.pick(&ids, config["rotateStrategy"].as_str().unwrap_or("none"), provider).unwrap_or_default();
                }
                let pool = if pool_id.is_empty() { Ok(None) } else {
                    db.query_row("SELECT data,isActive FROM proxyPools WHERE id=?1", [&pool_id], |row| {
                        let raw: String = row.get(0)?;
                        let mut pool: Value = serde_json::from_str(&raw).unwrap_or(json!({}));
                        pool["isActive"] = json!(row.get::<_, i64>(1)? != 0);
                        Ok(pool)
                    }).optional().map_err(|_| ())
                };
                let pool = pool.and_then(|p| p.ok_or(()));
                let proxy = crate::services::connection_proxy::resolve_connection_proxy_config(&json!({"proxyPoolId":pool_id}), pool.as_ref().map(Some).map_err(|_| ()));
                return Some(Credentials { connection_id: "noauth".into(), access_token: Some("public".into()), provider_specific_data: proxy, ..Default::default() });
            }
            let candidate = {
                let db = self.state.db.lock().ok()?;
                let mut st = db.prepare("SELECT id,name,data FROM providerConnections WHERE provider=?1 AND isActive=1 ORDER BY priority ASC").ok()?;
                let rows = st
                    .query_map([provider], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, String>(2)?)))
                    .ok()?;
                let mut candidates: Vec<_> = rows.flatten().filter(|row| {
                    if excluded.contains(&row.0) { return false; }
                    if provider == "antigravity" && crate::services::antigravity_quota::blocked(&row.0, model) { return false; }
                    let Some(data) = crate::crypto::parse_data(&row.2) else { return false };
                    // Upstream auth.js: auto-quarantined accounts never serve until re-enabled by hand.
                    if data["quarantinedAt"].is_string() { return false; }
                    // Upstream auth.js: Kiro accounts with an unresolved profileArn are skipped for OAuth/social auth.
                    if provider == "kiro" {
                        let psd = &data["providerSpecificData"];
                        let account_bound = matches!(psd["authMethod"].as_str(), Some("api_key" | "idc" | "external_idp"));
                        if !account_bound && psd["profileArnUnresolved"] == true && psd["profileArn"].as_str().is_none_or(str::is_empty) { return false; }
                    }
                    // Upstream auth.js: Codex accounts with an enabledModels list only serve those models.
                    if provider == "codex" && let Some(enabled) = data["providerSpecificData"]["enabledModels"].as_array().filter(|a| !a.is_empty()) && !model.is_empty() && !enabled.iter().any(|m| m.as_str() == Some(model)) { return false; }
                    [format!("modelLock_{model}"), "modelLock___all".into()].iter().all(|key| {
                        data[key].as_str().is_none_or(|expiry| db.query_row("SELECT julianday(?1) > julianday('now')", [expiry], |r| r.get::<_, Option<bool>>(0)).ok().flatten() != Some(true))
                    })
                }).collect();
                let settings = crate::db::settings_get(&db).unwrap_or_default();
                let strategy = settings["providerStrategies"][provider]["fallbackStrategy"].as_str().or_else(|| settings["fallbackStrategy"].as_str()).unwrap_or("balanced");
                if strategy == "balanced" {
                    candidates.sort_by_key(|row| {
                        let data = crate::crypto::parse_data(&row.2).unwrap_or_default();
                        (data["lastError"].as_str().is_some(), data["lastUsedAt"].as_str().unwrap_or("").to_owned())
                    });
                }
                candidates.sort_by_key(|row| preferred != Some(row.0.as_str()));
                // Upstream: a pinned connection beats the strategy; only unpinned picks rotate.
                let pinned = preferred.is_some_and(|id| candidates.iter().any(|c| c.0 == id));
                let now_sql = |db: &rusqlite::Connection| db.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |r| r.get::<_, String>(0)).ok();
                if !pinned && strategy == "round-robin" && !candidates.is_empty() {
                    // Upstream round-robin: stay on the most recent account while it has served fewer than
                    // stickyRoundRobinLimit requests in a row; otherwise rotate to the least recently used.
                    let limit = settings["providerStrategies"][provider]["stickyRoundRobinLimit"].as_u64().or_else(|| settings["stickyRoundRobinLimit"].as_u64()).unwrap_or(3).max(1);
                    let parsed: Vec<Value> = candidates.iter().map(|r| crate::crypto::parse_data(&r.2).unwrap_or_default()).collect();
                    let last = |i: usize| parsed[i]["lastUsedAt"].as_str().unwrap_or("").to_owned();
                    let uses = |i: usize| parsed[i]["consecutiveUseCount"].as_u64().unwrap_or(0);
                    let any_used = (0..candidates.len()).any(|i| !last(i).is_empty());
                    // Most recent use is current; equal ms timestamps fall back to the longer streak, then priority order.
                    let current = (0..candidates.len()).max_by(|&a, &b| last(a).cmp(&last(b)).then(uses(a).cmp(&uses(b))).then(b.cmp(&a))).unwrap_or(0);
                    // Least recently used, excluding the current account when another candidate exists.
                    let rotate = || (0..candidates.len()).filter(|&i| candidates.len() == 1 || i != current).min_by(|&a, &b| last(a).cmp(&last(b)).then(a.cmp(&b))).unwrap_or(current);
                    let (chosen, count) = if !any_used { (0, 1) } else if !last(current).is_empty() && uses(current) < limit { (current, uses(current) + 1) } else { (rotate(), 1) };
                    if let Some(now) = now_sql(&db) {
                        let mut data = parsed[chosen].clone();
                        data["lastUsedAt"] = json!(now);
                        data["consecutiveUseCount"] = json!(count);
                        let mut stored = data.clone();
                        crate::crypto::encrypt_fields(&mut stored);
                        if db.execute("UPDATE providerConnections SET data=?1 WHERE id=?2 AND data=?3", rusqlite::params![stored.to_string(), candidates[chosen].0, candidates[chosen].2]).ok() == Some(1) { candidates[chosen].2 = stored.to_string(); }
                    }
                    candidates.swap(0, chosen);
                } else if !pinned && strategy == "sticky-random" && !candidates.is_empty() {
                    // Upstream sticky-random: keep the current key while it succeeds; re-roll once it locks (sticky_forget).
                    let key = format!("{provider}:{model}");
                    let sticky_id = STICKY_CURRENT.lock().ok().and_then(|m| m.get(&key).cloned());
                    let idx = sticky_id.as_deref().and_then(|id| candidates.iter().position(|r| r.0 == id)).unwrap_or_else(|| rand_index(candidates.len()));
                    let fresh = sticky_id.as_deref() != Some(candidates[idx].0.as_str());
                    if let Some(now) = now_sql(&db) {
                        let mut data: Value = crate::crypto::parse_data(&candidates[idx].2).unwrap_or_default();
                        let prev = data["consecutiveUseCount"].as_u64().unwrap_or(0);
                        data["lastUsedAt"] = json!(now);
                        data["consecutiveUseCount"] = json!(if fresh { 1 } else { prev + 1 });
                        let mut stored = data.clone();
                        crate::crypto::encrypt_fields(&mut stored);
                        if db.execute("UPDATE providerConnections SET data=?1 WHERE id=?2 AND data=?3", rusqlite::params![stored.to_string(), candidates[idx].0, candidates[idx].2]).ok() == Some(1) { candidates[idx].2 = stored.to_string(); }
                    }
                    candidates.swap(0, idx);
                    if let Ok(mut m) = STICKY_CURRENT.lock() { m.insert(key, candidates[0].0.clone()); }
                }
                if preferred.is_none_or(|id| !candidates.iter().any(|c| c.0 == id)) && strategy == "balanced" && let Some(row) = candidates.first_mut() {
                    if let Ok(mut data) = serde_json::from_str::<Value>(&row.2) {
                        if let Ok(now) = db.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |r| r.get::<_, String>(0)) {
                            data["lastUsedAt"] = json!(now);
                            let mut stored = data.clone();
                            crate::crypto::encrypt_fields(&mut stored);
                            if db.execute("UPDATE providerConnections SET data=?1 WHERE id=?2 AND data=?3", rusqlite::params![stored.to_string(), row.0, row.2]).ok() == Some(1) { row.2 = stored.to_string(); }
                        }
                    }
                }
                candidates.into_iter().next()?
            };
            let mut data: Value = crate::crypto::parse_data(&candidate.2)?;
            // Proactive refresh like upstream checkAndRefreshToken. On failure keep the old token
            // so the request still reaches upstream and the 401 fallback path can run.
            if data["refreshToken"].is_string()
                && (data["expiresAt"].is_string() || data["tokenExpiresAt"].is_string())
                && crate::oauth::provider(provider).is_some()
                && let Ok(path) = crate::db::data_file()
            {
                let conn = crate::oauth::Connection { id: candidate.0.clone(), provider: provider.to_owned(), data: data.clone(), db_path: path };
                match crate::oauth::refresh_if_needed(conn).await {
                    Ok(c) => data = c.data,
                    Err(e) => tracing::warn!("oauth refresh for {} failed: {e}", candidate.0),
                }
            }
            if provider == "github" && let Ok(path) = crate::db::data_file() {
                let conn = crate::oauth::Connection { id: candidate.0.clone(), provider: provider.to_owned(), data: data.clone(), db_path: path };
                match crate::oauth::refresh_copilot(conn, "https://api.github.com/copilot_internal/v2/token").await {
                    Ok(c) => data = c.data,
                    Err(e) => tracing::warn!("Copilot exchange for {} failed: {e}", candidate.0),
                }
            }
            let mut specific = data["providerSpecificData"].as_object().cloned().unwrap_or_default();
            if let Some(project) = data.get("projectId").filter(|p| p.is_string()) { specific.entry("projectId").or_insert_with(|| project.clone()); }
            let pool_id = specific.get("proxyPoolId").and_then(Value::as_str).unwrap_or("").to_owned();
            if !pool_id.is_empty() && pool_id != "__none__" {
                let pool = self.state.db.lock().map_err(|_| ()).and_then(|db| {
                    db.query_row("SELECT data,isActive FROM proxyPools WHERE id=?1", [&pool_id], |row| {
                        let raw: String = row.get(0)?;
                        let mut pool: Value = serde_json::from_str(&raw).unwrap_or(json!({}));
                        pool["isActive"] = json!(row.get::<_, i64>(1)? != 0);
                        Ok(pool)
                    }).optional().map_err(|_| ())
                });
                let pool = pool.and_then(|p| p.ok_or(()));
                let proxy = crate::services::connection_proxy::resolve_connection_proxy_config(&json!({"proxyPoolId":pool_id}), pool.as_ref().map(Some).map_err(|_| ()));
                if let Some(config) = proxy.as_object() { specific.extend(config.clone()); }
            }
            Some(Credentials {
                connection_id: candidate.0,
                connection_name: candidate.1,
                access_token: if provider == "github" { data["providerSpecificData"]["copilotToken"].as_str().or_else(|| data["accessToken"].as_str()) } else { data["accessToken"].as_str() }.map(str::to_owned),
                refresh_token: data["refreshToken"].as_str().map(str::to_owned),
                api_key: data["apiKey"].as_str().map(str::to_owned),
                expires_at: data["expiresAt"].as_str().map(str::to_owned),
                provider_specific_data: Value::Object(specific),
                ..Default::default()
            })
        })
    }
    fn on_account_error<'a>(&'a self, connection_id: &'a str, model: &'a str, status: u16, message: &'a str) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            let antigravity_token = self.state.db.lock().ok().and_then(|db| db.query_row("SELECT provider,data FROM providerConnections WHERE id=?1", [connection_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).ok()).and_then(|(p, raw)| (p == "antigravity" && matches!(status, 409 | 429)).then(|| crate::crypto::parse_data(&raw)?.get("accessToken")?.as_str().map(str::to_owned)).flatten());
            if let Some(token) = antigravity_token { crate::services::antigravity_quota::on_error(connection_id, model, &token).await; }
            let fallback = matches!(status, 401..=404 | 408 | 409 | 429 | 500..=599);
            if fallback && let Ok(db) = self.state.db.lock() {
                let result = (|| -> rusqlite::Result<()> {
                    let (raw, provider): (String, String) = db.query_row("SELECT data,provider FROM providerConnections WHERE id=?1", [connection_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
                    sticky_forget(&provider, model, connection_id);
                    let mut data: Value = serde_json::from_str(&raw).map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))?;
                    let level = data["backoffLevel"].as_u64().unwrap_or(0).min(15);
                    let verdict = crate::routing::classify::classify(&provider, status, message, crate::routing::classify::now_ms());
                    let seconds = if provider == "antigravity" && let Some(reset) = crate::services::antigravity_quota::reset_seconds(connection_id, model) { reset.max(2_u64.saturating_mul(1_u64 << level.min(14)).min(300)) } else if provider == "codex" && message.to_ascii_lowercase().contains("model is not supported when using codex with a chatgpt account") { 1800 } else if message.to_ascii_lowercase().contains("request not allowed") { 5 } else { match verdict {
                        crate::routing::classify::Verdict::Backoff => {
                            let next = (level + 1).min(15);
                            data["backoffLevel"] = json!(next);
                            (2_u64.saturating_mul(1_u64 << (next - 1))).min(300)
                        }
                        crate::routing::classify::Verdict::Fixed(d) => d.as_secs(),
                        crate::routing::classify::Verdict::Quota { reset_ms } => reset_ms.saturating_sub(crate::routing::classify::now_ms()).div_ceil(1000).min(1800),
                        _ => 30,
                    }};
                    let until: String = db.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now',printf('+%d seconds',?1))", [seconds as i64], |r| r.get(0))?;
                    data[format!("modelLock_{model}")] = json!(until);
                    data["lastError"] = json!(message.chars().take(200).collect::<String>());
                    data["errorCode"] = json!(status);
                    data["lastErrorAt"] = json!(db.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |r| r.get::<_, String>(0))?);
                    if seconds >= 60 { data["testStatus"] = json!("unavailable"); }
                    let mut quarantine = false;
                    if matches!(status, 401 | 403) {
                        static FAILURES: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, std::collections::HashMap<String, std::time::Instant>>>> = std::sync::OnceLock::new();
                        if let Ok(mut failures) = FAILURES.get_or_init(Default::default).lock() {
                            let entries = failures.entry(provider).or_default();
                            entries.retain(|_, at| at.elapsed() <= std::time::Duration::from_secs(600));
                            entries.insert(connection_id.to_owned(), std::time::Instant::now());
                            if entries.len() < 5 {
                                let count = data["consecutiveAuthFailures"].as_u64().unwrap_or(0).saturating_add(1);
                                data["consecutiveAuthFailures"] = json!(count);
                                if count >= 5 {
                                    data["testStatus"] = json!("unavailable");
                                    data["quarantinedAt"] = data["lastErrorAt"].clone();
                                    data["quarantineReason"] = json!(format!("Auto-quarantined after {count} consecutive auth failures (last: {status})"));
                                    data["lastError"] = data["quarantineReason"].clone();
                                    quarantine = true;
                                }
                            }
                        }
                    }
                    let mut stored = data.clone();
                    crate::crypto::encrypt_fields(&mut stored);
                    db.execute("UPDATE providerConnections SET data=?1, isActive=CASE WHEN ?4 THEN 0 ELSE isActive END WHERE id=?2 AND data=?3", rusqlite::params![stored.to_string(), connection_id, raw, quarantine])?;
                    Ok(())
                })();
                if let Err(e) = result { tracing::warn!("account lock persistence failed: {e}"); }
            }
            fallback
        })
    }
    fn on_request_success<'a>(&'a self, connection_id: &'a str, model: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            crate::services::antigravity_quota::clear(connection_id, model);
            if let Ok(db) = self.state.db.lock() {
                let result = (|| -> rusqlite::Result<()> {
                    let raw: String = db.query_row("SELECT data FROM providerConnections WHERE id=?1", [connection_id], |r| r.get(0))?;
                    let mut data: Value = serde_json::from_str(&raw).map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))?;
                    if let Some(obj) = data.as_object_mut() {
                        obj.retain(|k, v| !k.starts_with("modelLock_") || (k != &format!("modelLock_{model}") && k != "modelLock___all" && v.as_str().is_some_and(|s| db.query_row("SELECT julianday(?1) > julianday('now')", [s], |r| r.get::<_, Option<bool>>(0)).ok().flatten() == Some(true))));
                        if !obj.keys().any(|k| k.starts_with("modelLock_")) {
                            obj.insert("testStatus".into(), json!("active"));
                            for k in ["lastError", "errorCode", "lastErrorAt"] { obj.insert(k.into(), Value::Null); }
                            obj.insert("backoffLevel".into(), json!(0));
                        }
                        obj.insert("consecutiveAuthFailures".into(), json!(0));
                    }
                    let mut stored = data.clone();
                    crate::crypto::encrypt_fields(&mut stored);
                    db.execute("UPDATE providerConnections SET data=?1 WHERE id=?2 AND data=?3", rusqlite::params![stored.to_string(), connection_id, raw])?;
                    Ok(())
                })();
                if let Err(e) = result { tracing::warn!("account success persistence failed: {e}"); }
            }
        })
    }
    fn get_token_saver_config<'a>(&'a self, model: &'a str) -> BoxFuture<'a, Option<crate::handlers::chat_core::TokenSaverConfig>> {
        Box::pin(async move {
            let db = self.state.db.lock().ok()?;
            let settings = crate::db::settings_get(&db).ok()?;
            let token_saver_enabled = settings.get("tokenSaverEnabled").and_then(Value::as_bool).unwrap_or(false);
            let rtk_enabled = settings.get("rtkEnabled").and_then(Value::as_bool).unwrap_or(true);
            let caveman_enabled = settings.get("cavemanEnabled").and_then(Value::as_bool).unwrap_or(false);
            let caveman_level = settings.get("cavemanLevel").and_then(Value::as_str).map(str::to_owned);
            let ponytail_enabled = settings.get("ponytailEnabled").and_then(Value::as_bool).unwrap_or(false);
            let ponytail_level = settings.get("ponytailLevel").and_then(Value::as_str).map(str::to_owned);

            let by_model = crate::db::kv_get(&db, "briefMode", "byModel")
                .ok().flatten().and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .and_then(|v| v.get(model).and_then(Value::as_str).map(str::to_owned));
            let by_key = self.api_key_id.as_deref().and_then(|kid| {
                crate::db::kv_get(&db, "briefMode", "byKey")
                    .ok().flatten().and_then(|s| serde_json::from_str::<Value>(&s).ok())
                    .and_then(|v| v.get(kid).and_then(Value::as_str).map(str::to_owned))
            });
            let global_brief = crate::db::kv_get(&db, "briefMode", "global")
                .ok().flatten().and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .and_then(|v| v.as_str().map(str::to_owned));
            let brief_level = by_model.or(by_key).or(global_brief);

            Some(crate::handlers::chat_core::TokenSaverConfig {
                token_saver_enabled,
                rtk_enabled,
                caveman_enabled,
                caveman_level,
                brief_level,
                ponytail_enabled,
                ponytail_level,
            })
        })
    }
    fn get_combo_models<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Option<Vec<String>>> {
        Box::pin(async move {
            let db = self.state.db.lock().ok()?;
            if let Some(intent) = crate::services::auto_routing::parse_auto_target(name) {
                let mut candidates = crate::services::auto_routing::build_auto_candidates(&db, intent);
                if let Some(id) = &self.api_key_id {
                    let record = db.query_row("SELECT COALESCE(accessRestricted,0),accessAllow FROM apiKeys WHERE id=?1", [id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?))).optional().ok()?;
                    if let Some((restricted, allow)) = record {
                        if restricted != 0 { let access = crate::services::key_access::KeyAccess::load(&db, allow); candidates.retain(|m| access.allows(&db, m)); }
                    } else { candidates.clear(); }
                }
                return Some(candidates);
            }
            let models: String = db
                .query_row("SELECT models FROM combos WHERE name=?1", [name], |r| {
                    r.get(0)
                })
                .ok()?;
            serde_json::from_str(&models).ok()
        })
    }

    fn variant_backing<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Option<String>> {
        Box::pin(async move {
            let db = self.state.db.lock().ok()?;
            let raw = crate::db::kv_get(&db, "promptInjections", "variants").ok().flatten()?;
            let variants: Value = serde_json::from_str(&raw).ok()?;
            variants[name]["model"].as_str().map(str::to_owned)
        })
    }
}
/// Resolves the request's API key to its `apiKeys.id` for usage attribution.
pub(crate) fn api_key_id(s: &AppState, headers: &HeaderMap) -> Option<String> {
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
pub(crate) fn uuid_like() -> String {
    use ring::rand::SecureRandom;
    let mut b = [0u8; 16];
    let _ = ring::rand::SystemRandom::new().fill(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub(crate) async fn completion(
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
    use axum::{body::{Body, to_bytes}, http::Request};
    use rusqlite::params;

    #[tokio::test]
    async fn restricted_access_denies_combo_alias_and_prefix_bypasses() {
        use tower::ServiceExt;
        let db = Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        db.execute("INSERT INTO apiKeys(id,key,createdAt,accessRestricted,accessAllow) VALUES('r','restricted','now',1,?1)", [json!(["openai/gpt-4o"]).to_string()]).unwrap();
        db.execute("INSERT INTO combos(id,name,models,createdAt,updatedAt) VALUES('c','Mixed',?1,'now','now')", [json!(["openai/gpt-4o","openai/secret"]).to_string()]).unwrap();
        crate::db::kv_set(&db, "modelAliases", "bad", &json!("openai/secret").to_string()).unwrap();
        let app = make_router(db, None);
        for model in ["Mixed", "bad", "openai/gpt-4o-extra", "openai/secret"] {
            let response = app.clone().oneshot(Request::post("/v1/chat/completions").header("authorization", "Bearer restricted").header("content-type", "application/json").body(Body::from(json!({"model":model,"messages":[]}).to_string())).unwrap()).await.unwrap();
            assert_eq!(response.status(), 403, "{model}");
            let body = to_bytes(response.into_body(), 8192).await.unwrap();
            let json: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(json["error"]["message"], format!("This API key is not allowed to use model \"{model}\""));
        }
        let response = app.oneshot(Request::get("/v1/models").header("authorization", "Bearer restricted").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), 200);
        let body = to_bytes(response.into_body(), 1_000_000).await.unwrap();
        let list: Value = serde_json::from_slice(&body).unwrap();
        assert!(list["data"].as_array().unwrap().iter().all(|m| m["id"] == "openai/gpt-4o"));
    }

    #[test]
    fn auto_target_parses_supported_intents() {
        assert_eq!(crate::services::auto_routing::parse_auto_target(" AUTO:Coding "), Some("coding"));
        assert_eq!(crate::services::auto_routing::parse_auto_target("auto:unknown"), None);
    }
    use tower::ServiceExt;
    #[tokio::test]
    async fn cooldown_backoff_and_auth_quarantine() {
        let db = Connection::open_in_memory().unwrap(); crate::db::migrate(&db).unwrap();
        db.execute("INSERT INTO providerConnections(id,provider,authType,isActive,priority,data,createdAt,updatedAt) VALUES('failure','cooldown-test','apikey',1,1,?1,'now','now')", [json!({"apiKey":"secret"}).to_string()]).unwrap();
        let hooks = Hooks { state: AppState { db: Arc::new(Mutex::new(db)), executor: Arc::new(HttpExecutor(reqwest::Client::new())), jwt_secret: None }, api_key_id: None };
        hooks.on_account_error("failure", "m", 429, "rate limit").await;
        hooks.on_account_error("failure", "m", 429, "rate limit").await;
        {
            let db = hooks.state.db.lock().unwrap(); let raw: String = db.query_row("SELECT data FROM providerConnections WHERE id='failure'", [], |r| r.get(0)).unwrap();
            let data: Value = serde_json::from_str(&raw).unwrap(); assert_eq!(data["backoffLevel"], 2); assert!(data["testStatus"].is_null());
        }
        for _ in 0..5 { hooks.on_account_error("failure", "m", 401, "invalid").await; }
        let db = hooks.state.db.lock().unwrap();
        let (active, raw): (bool, String) = db.query_row("SELECT isActive,data FROM providerConnections WHERE id='failure'", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        let data: Value = serde_json::from_str(&raw).unwrap(); assert!(!active); assert_eq!(data["consecutiveAuthFailures"], 5); assert_eq!(data["testStatus"], "unavailable");
    }
    /// Builds a Hooks over an in-memory DB with connections `(id, priority, extra data)` for provider "test".
    fn selection_hooks(rows: &[(&str, i64, Value)], strategy: Option<&str>) -> Hooks {
        let db = Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        for (id, priority, extra) in rows {
            let mut data = json!({"apiKey": "secret"});
            if let (Some(base), Some(add)) = (data.as_object_mut(), extra.as_object()) { base.extend(add.clone()); }
            db.execute("INSERT INTO providerConnections(id,provider,authType,isActive,priority,data,createdAt,updatedAt) VALUES(?1,'test','apikey',1,?2,?3,'now','now')", params![id, priority, data.to_string()]).unwrap();
        }
        if let Some(s) = strategy {
            crate::db::settings_put(&db, &json!({"fallbackStrategy": s})).unwrap();
        }
        Hooks { state: AppState { db: Arc::new(Mutex::new(db)), executor: Arc::new(HttpExecutor(reqwest::Client::new())), jwt_secret: None }, api_key_id: None }
    }

    // upstream: sse/services/auth.js getProviderCredentials, strategy "round-robin" (stick up to stickyRoundRobinLimit, then rotate to least recently used)
    #[tokio::test]
    async fn round_robin_sticks_then_rotates_to_least_recent() {
        let hooks = selection_hooks(&[("a", 1, json!({})), ("b", 2, json!({}))], Some("round-robin"));
        let none = HashSet::new();
        let mut picks = Vec::new();
        for _ in 0..5 { picks.push(hooks.get_credentials("test", "m", &none).await.unwrap().connection_id); }
        assert_eq!(picks, ["a", "a", "a", "b", "b"], "limit 3 default: 3 in a row on a, then rotate");
    }

    // upstream: sse/services/auth.js getProviderCredentials, strategy "sticky-random" (keep current key until it locks)
    #[tokio::test]
    async fn sticky_random_keeps_key_until_it_locks() {
        let hooks = selection_hooks(&[("a", 1, json!({})), ("b", 2, json!({}))], Some("sticky-random"));
        let none = HashSet::new();
        let first = hooks.get_credentials("test", "sticky-model", &none).await.unwrap().connection_id;
        for _ in 0..4 { assert_eq!(hooks.get_credentials("test", "sticky-model", &none).await.unwrap().connection_id, first); }
        hooks.on_account_error(&first, "sticky-model", 429, "limited").await;
        let mut excluded = HashSet::new(); excluded.insert(first.clone());
        let next = hooks.get_credentials("test", "sticky-model", &excluded).await.unwrap().connection_id;
        assert_ne!(next, first, "a locked sticky key must be re-rolled");
    }

    // upstream: sse/services/auth.js getProviderCredentials, priority order (lower priority number wins under fill-first)
    #[tokio::test]
    async fn priority_order_wins_under_fill_first() {
        let hooks = selection_hooks(&[("low", 5, json!({})), ("top", 1, json!({}))], Some("fill-first"));
        assert_eq!(hooks.get_credentials("test", "m", &HashSet::new()).await.unwrap().connection_id, "top");
    }

    // upstream: sse/services/auth.js getProviderCredentials, preferredConnectionId pins over strategy
    #[tokio::test]
    async fn preferred_pin_beats_round_robin() {
        let hooks = selection_hooks(&[("a", 1, json!({})), ("b", 2, json!({}))], Some("round-robin"));
        let none = HashSet::new();
        for _ in 0..3 { assert_eq!(hooks.get_credentials_preferred("test", "m", &none, Some("b")).await.unwrap().connection_id, "b"); }
    }

    // upstream: sse/services/auth.js isAvailable (kiro profileArnUnresolved skip for social/OAuth auth)
    #[tokio::test]
    async fn kiro_unresolved_arn_is_skipped_but_idc_is_not() {
        let unresolved = json!({"providerSpecificData": {"authMethod": "social", "profileArnUnresolved": true}});
        let idc = json!({"providerSpecificData": {"authMethod": "idc", "profileArnUnresolved": true}});
        let hooks = selection_hooks(&[("bad", 1, unresolved), ("good", 2, idc)], None);
        let db_provider = "kiro";
        {
            let db = hooks.state.db.lock().unwrap();
            db.execute("UPDATE providerConnections SET provider=?1", [db_provider]).unwrap();
        }
        assert_eq!(hooks.get_credentials(db_provider, "m", &HashSet::new()).await.unwrap().connection_id, "good");
    }

    // upstream: sse/services/auth.js isAvailable (codex enabledModels gates model routing)
    #[tokio::test]
    async fn codex_enabled_models_gate_the_account() {
        let enabled = json!({"providerSpecificData": {"enabledModels": ["gpt-5"]}});
        let hooks = selection_hooks(&[("only-5", 1, enabled)], None);
        {
            let db = hooks.state.db.lock().unwrap();
            db.execute("UPDATE providerConnections SET provider='codex'", []).unwrap();
        }
        assert!(hooks.get_credentials("codex", "gpt-4o", &HashSet::new()).await.is_none());
        assert_eq!(hooks.get_credentials("codex", "gpt-5", &HashSet::new()).await.unwrap().connection_id, "only-5");
    }

    // upstream: sse/services/auth.js auto-quarantine: quarantined accounts never serve
    #[tokio::test]
    async fn quarantined_account_is_skipped() {
        let hooks = selection_hooks(&[("q", 1, json!({"quarantinedAt": "2026-01-01T00:00:00Z"})), ("ok", 2, json!({}))], None);
        assert_eq!(hooks.get_credentials("test", "m", &HashSet::new()).await.unwrap().connection_id, "ok");
    }

    // upstream: sse/services/auth.js encryption contract; lock write-back must not persist plaintext tokens
    #[tokio::test]
    async fn lock_write_back_keeps_tokens_encrypted() {
        const CHILD: &str = "NINEROUTER_LOCK_ENCRYPT_TEST";
        if std::env::var_os(CHILD).is_none() {
            // encrypt_fields is a no-op without a key, so run the body in a child process with one set.
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "server::tests::lock_write_back_keeps_tokens_encrypted", "--nocapture"])
                .env(CHILD, "1").env("PRO_CREDENTIAL_KEY", "0707070707070707070707070707070707070707070707070707070707070707")
                .status().unwrap();
            assert!(status.success());
            return;
        }
        let hooks = selection_hooks(&[("enc", 1, json!({"accessToken": "plain-access-value", "refreshToken": "plain-refresh-value"}))], None);
        {
            let db = hooks.state.db.lock().unwrap();
            let mut data = json!({"accessToken": "plain-access-value", "refreshToken": "plain-refresh-value"});
            crate::crypto::encrypt_fields(&mut data);
            db.execute("UPDATE providerConnections SET data=?1 WHERE id='enc'", [data.to_string()]).unwrap();
        }
        hooks.on_account_error("enc", "m", 429, "limited").await;
        let db = hooks.state.db.lock().unwrap();
        let raw: String = db.query_row("SELECT data FROM providerConnections WHERE id='enc'", [], |r| r.get(0)).unwrap();
        assert!(!raw.contains("plain-access-value") && !raw.contains("plain-refresh-value"), "tokens must stay encrypted at rest");
        assert_eq!(crate::crypto::parse_data(&raw).unwrap()["accessToken"], "plain-access-value");
    }

    #[tokio::test]
    async fn preferred_connection_respects_locks_and_exclusions() {
        let db = Connection::open_in_memory().unwrap(); crate::db::migrate(&db).unwrap();
        for (id, priority) in [("a", 1), ("b", 2)] {
            db.execute("INSERT INTO providerConnections(id,provider,authType,isActive,priority,data,createdAt,updatedAt) VALUES(?1,'test','apikey',1,?2,?3,'now','now')", params![id, priority, json!({"apiKey":"secret"}).to_string()]).unwrap();
        }
        let hooks = Hooks { state: AppState { db: Arc::new(Mutex::new(db)), executor: Arc::new(HttpExecutor(reqwest::Client::new())), jwt_secret: None }, api_key_id: None };
        let mut excluded = HashSet::new();
        assert_eq!(hooks.get_credentials_preferred("test", "m", &excluded, Some("b")).await.unwrap().connection_id, "b");
        excluded.insert("b".into());
        assert_eq!(hooks.get_credentials_preferred("test", "m", &excluded, Some("b")).await.unwrap().connection_id, "a");
        excluded.clear(); hooks.on_account_error("b", "m", 429, "limited").await;
        assert_eq!(hooks.get_credentials_preferred("test", "m", &excluded, Some("b")).await.unwrap().connection_id, "a");
    }
    #[tokio::test]
    async fn account_model_locks_persist_expire_and_clear_on_success() {
        let db = Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        db.execute("INSERT INTO providerConnections(id,provider,authType,isActive,priority,data,createdAt,updatedAt) VALUES('a','test','apikey',1,1,?1,'now','now')", [json!({"apiKey":"secret"}).to_string()]).unwrap();
        let state = AppState { db: Arc::new(Mutex::new(db)), executor: Arc::new(HttpExecutor(reqwest::Client::new())), jwt_secret: None };
        let hooks = Hooks { state, api_key_id: None };
        let excluded = HashSet::new();
        assert!(hooks.on_account_error("a", "model", 429, "limited").await);
        assert!(hooks.get_credentials("test", "model", &excluded).await.is_none());
        assert!(hooks.get_credentials("test", "other", &excluded).await.is_some());
        {
            let db = hooks.state.db.lock().unwrap();
            let raw: String = db.query_row("SELECT data FROM providerConnections WHERE id='a'", [], |r| r.get(0)).unwrap();
            let mut data: Value = serde_json::from_str(&raw).unwrap();
            assert_eq!(data["lastError"], "limited");
            data["modelLock_model"] = json!("2000-01-01T00:00:00Z");
            db.execute("UPDATE providerConnections SET data=?1 WHERE id='a'", [data.to_string()]).unwrap();
        }
        assert!(hooks.get_credentials("test", "model", &excluded).await.is_some());
        hooks.on_request_success("a", "model").await;
        let db = hooks.state.db.lock().unwrap();
        let raw: String = db.query_row("SELECT data FROM providerConnections WHERE id='a'", [], |r| r.get(0)).unwrap();
        let data: Value = serde_json::from_str(&raw).unwrap();
        assert!(data.get("modelLock_model").is_none());
        assert_eq!(data["testStatus"], "active");
        assert!(data["lastError"].is_null());
    }
    fn test_router() -> Router {
        let db = Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        db.execute("INSERT INTO apiKeys(id,key,createdAt,accessRestricted,accessAllow) VALUES('1','secret','now',1,?1)",params!["{\"restricted\":true}"]).unwrap();
        make_router(db, Some(b"test-secret".to_vec()))
    }
    #[tokio::test]
    async fn free_provider_rotates_selected_active_proxy_pools() {
        let db = Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        for (id, url, created) in [("a", "http://a:8080", "2"), ("b", "http://b:8080", "1")] {
            db.execute("INSERT INTO proxyPools(id,isActive,data,createdAt,updatedAt) VALUES(?1,1,?2,?3,?3)", params![id, json!({"proxyUrl":url}).to_string(), created]).unwrap();
        }
        crate::db::settings_put(&db, &json!({"providerStrategies":{"opencode":{"rotateStrategy":"round-robin","proxyPoolIds":["a","b"]}}})).unwrap();
        let state = AppState { db: Arc::new(Mutex::new(db)), executor: Arc::new(HttpExecutor(reqwest::Client::new())), jwt_secret: None };
        let hooks = Hooks { state, api_key_id: None };
        let excluded = HashSet::new();
        let first = hooks.get_credentials("opencode", "test", &excluded).await.unwrap();
        let second = hooks.get_credentials("opencode", "test", &excluded).await.unwrap();
        let urls = [first.provider_specific_data["connectionProxyUrl"].as_str().unwrap(), second.provider_specific_data["connectionProxyUrl"].as_str().unwrap()];
        assert!(urls.contains(&"http://a:8080") && urls.contains(&"http://b:8080"));
    }
    #[tokio::test]
    async fn configured_quota_blocks_exhausted_key() {
        let db = Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        db.execute("INSERT INTO apiKeys(id,key,name,createdAt) VALUES('quota-key','quota-secret','test','now')", []).unwrap();
        crate::db::kv_set(&db, "keyQuotas", "quota-key", r#"{"enabled":true,"limit":5,"windowHours":24}"#).unwrap();
        db.execute("INSERT INTO usageHistory(timestamp,apiKey,promptTokens,completionTokens) VALUES(strftime('%Y-%m-%dT%H:%M:%fZ','now'),'quota-key',3,2)", []).unwrap();
        let response = make_router(db, None).oneshot(Request::get("/v1/models").header("authorization", "Bearer quota-secret").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    }
    // Upstream quota: sums usageHistory.cost per key within windowHours and blocks at costLimit.
    // A priced modality row (here an embeddings call, the same writer) must move that sum.
    #[test]
    fn cost_quota_counts_priced_modality_usage() {
        let db = Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        db.execute("INSERT INTO apiKeys(id,key,name,createdAt) VALUES('cq-key','cq-secret','cost-key','now')", []).unwrap();
        crate::db::kv_set(&db, "keyQuotas", "cq-key", r#"{"enabled":true,"limit":1000000000,"windowHours":24,"costLimit":0.005}"#).unwrap();
        assert!(quota_verdict(&db, "cq-key", "cq-secret").is_none(), "no spend yet");
        // gpt-4o 1000 prompt / 500 completion = $0.0075 upstream, over the $0.005 cap.
        let row = crate::db::UsageRow { provider: "openai".into(), model: "gpt-4o".into(), api_key: Some("cq-key".into()), endpoint: Some("/v1/embeddings".into()), prompt_tokens: 1000, completion_tokens: 500, status: 200, request_id: "cq-1".into(), ..Default::default() };
        let tokens = crate::services::pricing::TokenCounts { prompt_tokens: 1000, completion_tokens: 500, ..Default::default() };
        assert!(crate::db::usage_record(&db, &row, &tokens, None).unwrap());
        let v = quota_verdict(&db, "cq-key", "cq-secret").expect("cost quota trips");
        assert_eq!(v["costBlocked"], true);
        assert!((v["costUsed"].as_f64().unwrap() - 0.0075).abs() < 1e-12);
    }

    #[tokio::test]
    async fn key_limit_blocks_until_response_body_is_dropped() {
        let db = Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        db.execute("INSERT INTO apiKeys(id,key,createdAt) VALUES('limit-body-test','limit-body-secret','now')", []).unwrap();
        crate::db::kv_set(&db, "keyLimits", "limit-body-test", r#"{"maxConcurrent":1}"#).unwrap();
        let app = make_router(db, None);
        let request = || Request::get("/v1/models").header("authorization", "Bearer limit-body-secret").body(Body::empty()).unwrap();
        let first = app.clone().oneshot(request()).await.unwrap();
        assert_eq!(first.status(), 200);
        let blocked = app.clone().oneshot(request()).await.unwrap();
        assert_eq!(blocked.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(blocked.headers()[header::RETRY_AFTER], "5");
        drop(first);
        assert_eq!(app.oneshot(request()).await.unwrap().status(), 200);
    }
    #[tokio::test]
    async fn usage_records_latency_only_when_observability_enabled() {
        let db = Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        let state = AppState { db: Arc::new(Mutex::new(db)), executor: Arc::new(HttpExecutor(reqwest::Client::new())), jwt_secret: None };
        let hooks = Hooks { api_key_id: None, state: state.clone() };
        hooks.on_usage_record(UsageRecord { provider: "test".into(), model: "model".into(), status: 200, latency_ms: 42, ..Default::default() }).await;
        {
            let db = state.db.lock().unwrap();
            let stats = crate::services::latency_stats::get_latency_stats(&db, None, 10).unwrap();
            assert_eq!(stats.total.p50, Some(42.0));
            crate::db::settings_put(&db, &json!({"observabilityEnabled":false})).unwrap();
        }
        hooks.on_usage_record(UsageRecord { provider: "test".into(), status: 200, latency_ms: 20, ..Default::default() }).await;
        let db = state.db.lock().unwrap();
        assert_eq!(crate::services::latency_stats::get_latency_stats(&db, None, 10).unwrap().samples, 1);
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
    async fn management_settings_password_is_hashed_and_hidden() {
        let db = Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        let (app, db) = make_router_with_db(Arc::new(Mutex::new(db)), Some(b"test-secret".to_vec()));
        let request = Request::patch("/api/settings")
            .header("cookie", format!("auth_token={}", token()))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"newPassword":"securepassword","password":"injected"}"#)).unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), 200);
        let body = to_bytes(response.into_body(), 10000).await.unwrap();
        assert!(serde_json::from_slice::<Value>(&body).unwrap().get("password").is_none());
        let stored = crate::db::settings_get(&db.lock().unwrap()).unwrap();
        assert!(bcrypt::verify("securepassword", stored["password"].as_str().unwrap()).unwrap());
        let response = app.oneshot(Request::post("/api/locale").header("content-type", "application/json").body(Body::from(r#"{"locale":"en"}"#)).unwrap()).await.unwrap();
        assert_eq!(response.status(), 401);
    }
    #[tokio::test]
    async fn management_node_and_combo_crud() {
        let app = test_router();
        let request = |method: &str, path: &str, data: Value| Request::builder().method(method).uri(path)
            .header("cookie", format!("auth_token={}", token())).header("content-type", "application/json")
            .body(Body::from(data.to_string())).unwrap();
        let res = app.clone().oneshot(request("POST", "/api/provider-nodes", json!({"name":"Example", "prefix":"ex", "apiType":"chat"}))).await.unwrap();
        assert_eq!(res.status(), 201);
        let node: Value = serde_json::from_slice(&to_bytes(res.into_body(), 10000).await.unwrap()).unwrap();
        let id = node["node"]["id"].as_str().unwrap();
        let path = format!("/api/provider-nodes/{id}");
        let res = app.clone().oneshot(request("PUT", &path, json!({"name":"Changed","prefix":"new","apiType":"chat","baseUrl":"https://example.com"}))).await.unwrap();
        assert_eq!(res.status(), 200);
        let res = app.clone().oneshot(request("DELETE", &path, json!({}))).await.unwrap();
        assert_eq!(res.status(), 200);
        let res = app.clone().oneshot(request("POST", "/api/combos", json!({"name":"test","models":["openai/model"]}))).await.unwrap();
        assert_eq!(res.status(), 201);
        let combo: Value = serde_json::from_slice(&to_bytes(res.into_body(), 10000).await.unwrap()).unwrap();
        let path = format!("/api/combos/{}", combo["id"].as_str().unwrap());
        assert_eq!(app.clone().oneshot(request("PUT", &path, json!({"models":[]}))).await.unwrap().status(), 400);
        assert_eq!(app.clone().oneshot(request("GET", &path, json!({}))).await.unwrap().status(), 200);
        assert_eq!(app.oneshot(request("DELETE", &path, json!({}))).await.unwrap().status(), 200);
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
    async fn restricted_allowed_model_and_auto_route() {
        let upstream = mock_upstream().await;
        let (app, db) = e2e_router(&upstream);
        let seed = crate::providers::lookup("openai").unwrap().models.first().unwrap().id.clone();
        {
            let db = db.lock().unwrap();
            db.execute("UPDATE apiKeys SET accessRestricted=1,accessAllow=?1 WHERE id='k1'", [json!([format!("openai/{seed}")]).to_string()]).unwrap();
            db.execute("UPDATE providerConnections SET data=?1 WHERE id='c1'", [json!({"apiKey":"upstream-key","providerSpecificData":{"baseUrl":upstream,"enabledModels":[seed]}}).to_string()]).unwrap();
        }
        for model in [format!("openai/{seed}"), "auto:coding".to_owned()] {
            let response = app.clone().oneshot(Request::post("/v1/chat/completions").header("authorization", "Bearer sk-test-key").header("content-type", "application/json").body(Body::from(json!({"model":model,"messages":[{"role":"user","content":"hi"}]}).to_string())).unwrap()).await.unwrap();
            assert_eq!(response.status(), 200, "{model}");
        }
    }

    async fn mock_webhooks() -> (String, tokio::sync::mpsc::UnboundedReceiver<Value>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let app = Router::new().route("/", post(move |Json(value): Json<Value>| {
            let tx = tx.clone();
            async move { tx.send(value).unwrap(); }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (url, rx)
    }

    async fn event(rx: &mut tokio::sync::mpsc::UnboundedReceiver<Value>, expected: Value) {
        let actual = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await.unwrap().unwrap();
        let timestamp = actual["timestamp"].as_str().unwrap();
        assert!(timestamp.ends_with('Z'));
        assert_eq!(actual, json!({"event":expected["event"],"timestamp":timestamp,"data":expected["data"]}));
    }

    #[tokio::test]
    async fn error_webhook_matches_upstream_payload() {
        let (url, mut rx) = mock_webhooks().await;
        let upstream = Router::new().route("/v1/chat/completions", post(|| async { (StatusCode::BAD_GATEWAY, Json(json!({"error":{"message":"upstream failed"}}))) }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = format!("http://{}/v1/chat/completions", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let (app, db) = e2e_router(&address);
        crate::db::kv_set(&db.lock().unwrap(), "notify", "config", &json!({"webhook":{"enabled":true,"url":url},"webhookEvents":{"error":true}}).to_string()).unwrap();
        let response = app.oneshot(Request::post("/v1/chat/completions").header("authorization", "Bearer sk-test-key").header("content-type", "application/json").body(Body::from(json!({"model":"openai/gpt-test","messages":[{"role":"user","content":"hi"}]}).to_string())).unwrap()).await.unwrap();
        assert_eq!(response.status(), 502);
        event(&mut rx, json!({"event":"error","data":{"status":502,"error":"upstream failed"}})).await;
    }

    #[tokio::test]
    async fn completion_rechecks_quota_and_blocks_next_request() {
        let upstream = mock_upstream().await;
        let (url, mut rx) = mock_webhooks().await;
        let (app, db) = e2e_router(&upstream);
        {
            let db = db.lock().unwrap();
            crate::db::kv_set(&db, "keyQuotas", "k1", &json!({"enabled":true,"limit":10,"windowHours":24}).to_string()).unwrap();
            crate::db::kv_set(&db, "notify", "config", &json!({"webhook":{"enabled":true,"url":url},"webhookEvents":{"quota":true}}).to_string()).unwrap();
        }
        let request = || Request::post("/v1/chat/completions").header("authorization", "Bearer sk-test-key").header("content-type", "application/json").body(Body::from(json!({"model":"openai/gpt-test","stream":true,"messages":[{"role":"user","content":"hi"}]}).to_string())).unwrap();
        let response = app.clone().oneshot(request()).await.unwrap();
        assert_eq!(response.status(), 200);
        to_bytes(response.into_body(), 100_000).await.unwrap();
        event(&mut rx, json!({"event":"quota","data":{"keyName":"t","used":10,"limit":10,"windowHours":24,"costBlocked":false,"crossedMidRequest":true}})).await;
        assert_eq!(app.oneshot(request()).await.unwrap().status(), 429);
        event(&mut rx, json!({"event":"quota","data":{"keyName":"t","used":10,"limit":10,"windowHours":24,"costBlocked":false,"costUsed":null,"costLimit":null}})).await;
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
    async fn all_dashboard_pages_require_auth_and_return_200_when_logged_in() {
        let paths = [
            "/dashboard",
            "/dashboard/endpoint",
            "/dashboard/providers",
            "/dashboard/providers/new",
            "/dashboard/providers/openai",
            "/dashboard/combos",
            "/dashboard/usage",
            "/dashboard/quota",
            "/dashboard/token-saver",
            "/dashboard/prompt-injection",
            "/dashboard/profile",
            "/dashboard/skills",
            "/dashboard/pxpipe",
            "/dashboard/proxy-pools",
            "/dashboard/mitm",
            "/dashboard/automation",
            "/dashboard/cli-tools",
            "/dashboard/cli-tools/claude",
            "/dashboard/basic-chat",
            "/dashboard/console-log",
            "/dashboard/translator",
            "/dashboard/media-providers/web",
            "/dashboard/media-providers/image",
            "/dashboard/media-providers/image/dall-e",
            "/dashboard/media-providers/combo/default",
        ];
        let app = test_router();
        let tok = token();
        for p in paths {
            let unauth = app
                .clone()
                .oneshot(Request::get(p).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert!(
                unauth.status().is_redirection(),
                "expected redirect for unauthenticated {p}, got {}",
                unauth.status()
            );
            assert_eq!(unauth.headers().get("location").unwrap(), "/login");

            let auth = app
                .clone()
                .oneshot(
                    Request::get(p)
                        .header("cookie", format!("auth_token={tok}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(auth.status(), 200, "expected 200 for {p}");
            let ct = auth
                .headers()
                .get("content-type")
                .unwrap()
                .to_str()
                .unwrap();
            assert!(ct.contains("text/html"), "expected html for {p}");
            let body = to_bytes(auth.into_body(), 200_000).await.unwrap();
            let html = std::str::from_utf8(&body).unwrap();
            assert!(html.contains("/dashboard/app.js"), "missing app.js on {p}");
        }
    }
    #[tokio::test]
    async fn next_rewrite_aliases_reach_v1_routes() {
        assert_eq!(rewrite_path("/api/v1/models").as_deref(), Some("/v1/models"));
        assert_eq!(rewrite_path("/api/v1beta/models/x").as_deref(), Some("/v1beta/models/x"));
        assert_eq!(rewrite_path("/v1/v1/chat/completions").as_deref(), Some("/v1/chat/completions"));
        assert_eq!(rewrite_path("/codex/responses").as_deref(), Some("/v1/responses"));
        assert_eq!(rewrite_path("/api/v1x"), None);
        assert_eq!(rewrite_path("/v1/models"), None);
        let db = Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        let app = make_router(db, None);
        for path in ["/api/v1/models", "/v1/v1/models", "/v1/models"] {
            let res = app.clone().oneshot(Request::get(path).body(Body::empty()).unwrap()).await.unwrap();
            assert_eq!(res.status(), 401, "{path} should hit the authed /v1/models route");
        }
    }
    #[tokio::test]
    async fn login_locks_out_after_five_failures() {
        let db = Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        crate::db::kv_set(&db, "settings", "password", "test-pass").unwrap();
        let app = make_router(db, Some(b"test-secret".to_vec()));
        let attempt = |pw: &'static str| {
            app.clone().oneshot(
                Request::post("/api/auth/login")
                    .header("content-type", "application/json")
                    .header("cf-connecting-ip", "198.51.100.2")
                    .body(Body::from(format!(r#"{{"password":"{pw}"}}"#)))
                    .unwrap(),
            )
        };
        for _ in 0..4 {
            assert_eq!(attempt("wrong").await.unwrap().status(), 401);
        }
        let locked = attempt("wrong").await.unwrap();
        assert_eq!(locked.status(), 429);
        assert_eq!(locked.headers()["retry-after"], "30");
        // Correct password is still refused while locked.
        assert_eq!(attempt("test-pass").await.unwrap().status(), 429);
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
                    .header("cf-connecting-ip", "198.51.100.1")
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
                    .header("cf-connecting-ip", "198.51.100.1")
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
