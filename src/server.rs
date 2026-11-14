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
    // Restricted keys fail closed until model access policies are enforced.
    let Some(key_id) = key.and_then(|key| s.db.lock().ok().and_then(|db| db.query_row("SELECT id FROM apiKeys WHERE key=?1 AND isActive=1 AND (expiresAt IS NULL OR expiresAt > strftime('%Y-%m-%dT%H:%M:%fZ','now')) AND COALESCE(accessRestricted,0)=0", [key], |r| r.get::<_, String>(0)).optional().ok().flatten())) else {
        return unauthorized();
    };
    let has_quota = s.db.lock().ok().and_then(|db| crate::db::kv_get(&db, "keyQuotas", &key_id).ok().flatten()).and_then(|raw| serde_json::from_str::<Value>(&raw).ok()).is_some_and(|q| q["enabled"] != false && q["limit"].as_i64().is_some_and(|n| n > 0));
    let quota = s.db.lock().ok().and_then(|db| quota_verdict(&db, &key_id, key.unwrap_or("")));
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
    let raw_key = key.unwrap_or("").to_owned();
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
async fn models(State(state): State<AppState>) -> Json<Value> {
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
    Json(json!({"object":"list","data":data}))
}
/// Per-request chat hooks. `api_key_id` is the `apiKeys.id` of the caller, if known.
struct Hooks {
    state: AppState,
    api_key_id: Option<String>,
}
impl ChatHooks for Hooks {
    fn settings<'a>(&'a self) -> BoxFuture<'a, Value> {
        Box::pin(async move {
            self.state.db.lock().ok().and_then(|db| crate::db::settings_get(&db).ok()).unwrap_or_default()
        })
    }
    fn smart_choice<'a>(&'a self, body: &'a Value) -> BoxFuture<'a, Option<Value>> {
        Box::pin(async move {
            let model = body["model"].as_str()?;
            let target = crate::handlers::chat::parse_model_target(model)?;
            self.get_credentials(&target.provider, &target.model, &std::collections::HashSet::new()).await?;
            let response = crate::handlers::media::systemone::handle_systemone(State(self.state.clone()), Json(body.clone())).await;
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
            let key = self.api_key_id.as_deref().unwrap_or("");
            [global, by_key[key].clone(), by_model[model].clone(), by_key_model[key][model].clone()]
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
            };
            match crate::db::usage_insert(&db, &row) {
                Err(err) => tracing::warn!("usage insert failed: {err}"),
                Ok(()) => crate::services::notify::emit(self.state.db.clone(), "usage", json!({
                    "provider": row.provider, "model": row.model, "connectionId": row.connection_id,
                    "apiKeyMasked": null, "endpoint": row.endpoint, "promptTokens": row.prompt_tokens,
                    "completionTokens": row.completion_tokens, "cost": 0, "status": row.status.to_string(), "requestId": row.request_id
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
        _: &'a str,
        excluded: &'a HashSet<String>,
    ) -> BoxFuture<'a, Option<Credentials>> {
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
                let proxy = crate::services::connection_proxy::resolve_connection_proxy_config(&json!({"proxyPoolId":pool_id}), pool.as_ref().map(|p| p.as_ref()).map_err(|_| ()));
                return Some(Credentials { connection_id: "noauth".into(), access_token: Some("public".into()), provider_specific_data: proxy, ..Default::default() });
            }
            let candidate = {
                let db = self.state.db.lock().ok()?;
                let mut st = db.prepare("SELECT id,name,data FROM providerConnections WHERE provider=?1 AND isActive=1 ORDER BY priority ASC").ok()?;
                let rows = st
                    .query_map([provider], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, String>(2)?)))
                    .ok()?;
                rows.flatten().find(|row| !excluded.contains(&row.0))?
            };
            let mut data: Value = crate::crypto::parse_data(&candidate.2)?;
            // Proactive refresh like upstream checkAndRefreshToken. On failure keep the old token
            // so the request still reaches upstream and the 401 fallback path can run.
            if data["refreshToken"].is_string()
                && data["expiresAt"].is_string()
                && crate::oauth::provider(provider).is_some()
                && let Ok(path) = crate::db::data_file()
            {
                let conn = crate::oauth::Connection { id: candidate.0.clone(), provider: provider.to_owned(), data: data.clone(), db_path: path };
                match crate::oauth::refresh_if_needed(conn).await {
                    Ok(c) => data = c.data,
                    Err(e) => tracing::warn!("oauth refresh for {} failed: {e}", candidate.0),
                }
            }
            Some(Credentials {
                connection_id: candidate.0,
                connection_name: candidate.1,
                access_token: data["accessToken"].as_str().map(str::to_owned),
                refresh_token: data["refreshToken"].as_str().map(str::to_owned),
                api_key: data["apiKey"].as_str().map(str::to_owned),
                expires_at: data["expiresAt"].as_str().map(str::to_owned),
                provider_specific_data: data["providerSpecificData"].clone(),
                ..Default::default()
            })
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
    use tower::ServiceExt;
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
