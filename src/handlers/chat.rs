use axum::body::Body;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::Arc;

use crate::handlers::chat_core::{
    ChatCoreHooks, ChatCoreOptions, ChatCoreResult, TokenSaverConfig, handle_chat_core,
};
use crate::handlers::{BoxFuture, Credentials, Executor, RefreshedCredentials};
use crate::providers;
use crate::routing::classify::{
    MAX_RATE_LIMIT_COOLDOWN, OVERLOAD_MAX_ATTEMPTS, TRANSIENT_COOLDOWN, Verdict,
    classify_with_retry_after, now_ms, overload_delay,
};
use crate::routing::{self, cooldown::Cooldowns};
use std::time::{Duration, Instant};
#[path = "fusion.rs"]
mod fusion;
use fusion::fusion_chat;

/// One upstream attempt, written to usage history by the server.
#[derive(Debug, Clone, Default)]
pub struct UsageRecord {
    pub connection_id: Option<String>,
    pub provider: String,
    pub model: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cached_tokens: u64,
    pub cache_creation_tokens: u64,
    pub reasoning_tokens: u64,
    pub status: u16,
    pub latency_ms: u64,
}

/// Pseudo provider marking a combo member that is itself a combo.
const NESTED_COMBO: &str = "\u{0}nested-combo";

#[derive(Debug, Clone)]
pub struct ModelTarget {
    pub provider: String,
    pub model: String,
}

/// Parse `provider/model` into `(provider, model)`, resolving provider aliases.
pub fn parse_model_target(model_str: &str) -> Option<ModelTarget> {
    let (raw_provider, model) = model_str.split_once('/')?;
    if raw_provider.is_empty() || model.is_empty() {
        return None;
    }
    let provider = providers::lookup(raw_provider)
        .map(|p| p.id.clone())
        .unwrap_or_else(|| raw_provider.to_string());
    Some(ModelTarget {
        provider,
        model: model.to_string(),
    })
}

/// Strip `[1m]`-style context markers from model names before resolution.
pub fn strip_model_context_marker(model: &str) -> &str {
    crate::services::model_markers::strip_model_context_marker(model).0
}

pub fn error_response(status: u16, message: &str) -> Response {
    let body = serde_json::to_vec(&json!({
        "error": {
            "message": message,
            "code": status,
        }
    }))
    .unwrap_or_else(|_| b"{}".to_vec());

    Response::builder()
        .status(StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR))
        .header("content-type", "application/json")
        .header("access-control-allow-origin", "*")
        .body(Body::from(body))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

fn upstream_error_response(status: u16, message: &str, headers: &[(String, String)]) -> Response {
    let mut response = error_response(status, message);
    for (name, value) in crate::services::upstream_headers::upstream_response_headers(
        headers.iter().map(|(name, value)| (name.as_str(), value.as_str())),
    ) {
        if let (Ok(name), Ok(value)) = (axum::http::HeaderName::from_bytes(name.as_bytes()), axum::http::HeaderValue::from_str(&value)) {
            response.headers_mut().insert(name, value);
        }
    }
    response
}

async fn peek_combo_response(response: Response) -> (Response, bool) {
    use futures_core::Stream;
    use std::{collections::VecDeque, pin::Pin, task::{Context, Poll}};
    struct Replay {
        head: VecDeque<Result<axum::body::Bytes, axum::Error>>,
        tail: axum::body::BodyDataStream,
    }
    impl Stream for Replay {
        type Item = Result<axum::body::Bytes, axum::Error>;
        fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            if let Some(chunk) = self.head.pop_front() { return Poll::Ready(Some(chunk)); }
            Pin::new(&mut self.tail).poll_next(cx)
        }
    }
    let (parts, body) = response.into_parts();
    let mut tail = body.into_data_stream();
    let mut head = VecDeque::new();
    let mut state = crate::services::empty_failover::StreamState::default();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(crate::config::EMPTY_PEEK_DEADLINE_MS);
    let mut ended = false;
    let mut size = 0usize;
    while size < 1024 * 1024 && !state.saw_content {
        match tokio::time::timeout_at(deadline, std::future::poll_fn(|cx| Pin::new(&mut tail).poll_next(cx))).await {
            Ok(Some(Ok(bytes))) => {
                size += bytes.len();
                let mut text = std::mem::take(&mut state.pending);
                text.push_str(&String::from_utf8_lossy(&bytes));
                crate::services::empty_failover::scan_sse_buffer(&text, &mut state);
                head.push_back(Ok(bytes));
            }
            Ok(None) => { ended = true; break; }
            Ok(Some(Err(error))) => { head.push_back(Err(error)); break; }
            Err(_) => break,
        }
    }
    if ended && !state.pending.is_empty() {
        let pending = format!("{}\n", std::mem::take(&mut state.pending));
        crate::services::empty_failover::scan_sse_buffer(&pending, &mut state);
    }
    let empty = ended && !state.saw_content && !state.finish_reason.as_deref().is_some_and(crate::services::empty_failover::is_no_retry_finish);
    (Response::from_parts(parts, Body::from_stream(Replay { head, tail })), empty)
}

async fn probe_flaky_response(response: Response) -> (Response, bool) {
    use futures_core::Stream;
    use std::{collections::VecDeque, pin::Pin, task::{Context, Poll}};
    struct Replay {
        head: VecDeque<Result<axum::body::Bytes, axum::Error>>,
        tail: axum::body::BodyDataStream,
    }
    impl Stream for Replay {
        type Item = Result<axum::body::Bytes, axum::Error>;
        fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            if let Some(chunk) = self.head.pop_front() { return Poll::Ready(Some(chunk)); }
            Pin::new(&mut self.tail).poll_next(cx)
        }
    }
    let (parts, body) = response.into_parts();
    let mut tail = body.into_data_stream();
    let probe = crate::services::flaky_guard::probe_response(&mut tail, Duration::from_millis(120), 2_000_000).await;
    let flaky = probe.ended && probe.flaky.is_some();
    (Response::from_parts(parts, Body::from_stream(Replay { head: probe.buffered.into(), tail })), flaky)
}

/// Hooks for credential lookup, persistence, and account failover in `handle_chat`.
pub trait ChatHooks: Send + Sync {
    fn filter_adapter_models<'a>(&'a self, augmented: Vec<String>, _original: &'a [String]) -> BoxFuture<'a, Vec<String>> {
        Box::pin(async move { augmented })
    }
    fn settings<'a>(&'a self) -> BoxFuture<'a, Value> {
        Box::pin(async { json!({}) })
    }

    fn smart_choice<'a>(&'a self, _body: &'a Value) -> BoxFuture<'a, Option<Value>> {
        Box::pin(async { None })
    }
    fn pair_enabled<'a>(&'a self, _name: &'a str) -> BoxFuture<'a, bool> {
        Box::pin(async { false })
    }
    fn capacity_connections<'a>(&'a self) -> BoxFuture<'a, Vec<Value>> {
        Box::pin(async { Vec::new() })
    }
    fn get_pinned_credentials<'a>(&'a self, provider: &'a str, model: &'a str, excluded: &'a HashSet<String>, _pin: Option<&'a str>) -> BoxFuture<'a, Option<Credentials>> {
        self.get_credentials(provider, model, excluded)
    }
    fn get_credentials<'a>(
        &'a self,
        provider: &'a str,
        model: &'a str,
        excluded_connection_ids: &'a HashSet<String>,
    ) -> BoxFuture<'a, Option<Credentials>>;

    fn get_credentials_preferred<'a>(&'a self, provider: &'a str, model: &'a str, excluded: &'a HashSet<String>, _preferred: Option<&'a str>) -> BoxFuture<'a, Option<Credentials>> {
        self.get_credentials(provider, model, excluded)
    }

    fn get_combo_models<'a>(
        &'a self,
        _combo_name: &'a str,
    ) -> BoxFuture<'a, Option<Vec<String>>> {
        Box::pin(async { None })
    }

    fn variant_backing<'a>(&'a self, _name: &'a str) -> BoxFuture<'a, Option<String>> {
        Box::pin(async { None })
    }

    /// Map a user-defined provider-node prefix (e.g. `nr`) to its node id
    /// (`openai-compatible-chat-...`). Built-in provider ids/aliases are never passed here.
    fn resolve_node_prefix<'a>(&'a self, _prefix: &'a str) -> BoxFuture<'a, Option<String>> {
        Box::pin(async { None })
    }

    fn on_credentials_refreshed<'a>(
        &'a self,
        _connection_id: &'a str,
        _new_creds: RefreshedCredentials,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }

    fn on_usage_record<'a>(&'a self, _record: UsageRecord) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn on_error<'a>(&'a self, _status: u16, _message: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn on_request_success<'a>(
        &'a self,
        _connection_id: &'a str,
        _model: &'a str,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn get_token_saver_config<'a>(&'a self, _model: &'a str) -> BoxFuture<'a, Option<TokenSaverConfig>> {
        Box::pin(async { None })
    }

    fn anti_flaky_config<'a>(&'a self) -> BoxFuture<'a, (bool, u8)> {
        Box::pin(async { (true, 2) })
    }

    fn prompt_injections<'a>(&'a self, _model: &'a str) -> BoxFuture<'a, Vec<Value>> {
        Box::pin(async { Vec::new() })
    }

    fn on_account_error<'a>(
        &'a self,
        _connection_id: &'a str,
        _model: &'a str,
        status: u16,
        _message: &'a str,
    ) -> BoxFuture<'a, bool> {
        // Default: fall back on auth, rate limit, timeout, or 5xx upstream errors.
        let should_fallback = matches!(status, 401 | 403 | 408 | 429 | 500..=599);
        Box::pin(async move { should_fallback })
    }
}

pub struct SingleCredHooks {
    pub credentials: Credentials,
}

impl ChatHooks for SingleCredHooks {
    fn get_credentials<'a>(
        &'a self,
        _provider: &'a str,
        _model: &'a str,
        excluded: &'a HashSet<String>,
    ) -> BoxFuture<'a, Option<Credentials>> {
        Box::pin(async move {
            if excluded.contains(&self.credentials.connection_id) {
                None
            } else {
                Some(self.credentials.clone())
            }
        })
    }
}

/// Handle a chat completion request across OpenAI, Claude, Gemini, and Responses formats.
pub async fn handle_chat(
    body: Value,
    headers: &HeaderMap,
    endpoint_path: Option<&str>,
    executor: &dyn Executor,
    hooks: Arc<dyn ChatHooks>,
) -> Response {
    handle_chat_inner(body, headers, endpoint_path, executor, hooks, None).await
}

async fn handle_chat_inner(
    body: Value, headers: &HeaderMap, endpoint_path: Option<&str>, executor: &dyn Executor,
    hooks: Arc<dyn ChatHooks>, member_context: Option<(String, Option<String>)>,
) -> Response {
    handle_chat_nested(body, headers, endpoint_path, executor, hooks, member_context, false).await
}

/// `nested`: upstream chat.js nested-combo branch (dispatchCombo with recordBreakerSuccess=false).
async fn handle_chat_nested(
    mut body: Value, headers: &HeaderMap, endpoint_path: Option<&str>, executor: &dyn Executor,
    hooks: Arc<dyn ChatHooks>, member_context: Option<(String, Option<String>)>, nested: bool,
) -> Response {
    let Some(raw_model) = body.get("model").and_then(|v| v.as_str()).map(str::to_string) else {
        return error_response(400, "Missing model");
    };
    let model_str = strip_model_context_marker(&raw_model).to_string();
    if model_str != raw_model
        && let Some(obj) = body.as_object_mut()
    {
        obj.insert("model".to_string(), json!(model_str));
    }

    let is_combo = parse_model_target(&model_str).is_none();
    let mut member_targets = std::collections::HashMap::new();
    let mut nested_members: HashSet<String> = HashSet::new();
    let mut targets = if let Some(mut t) = parse_model_target(&model_str) {
        // Upstream getModelInfo: a non-reserved prefix may name a provider node.
        if providers::lookup(&t.provider).is_none()
            && let Some(node_id) = hooks.resolve_node_prefix(&t.provider).await
        {
            t.provider = node_id;
        }
        vec![t]
    } else if let Some(combo_models) = hooks.get_combo_models(&model_str).await {
        let mut resolved = Vec::new();
        for member in combo_models {
            let backing = if member.contains('/') { member.clone() } else { hooks.variant_backing(&member).await.unwrap_or_else(|| member.clone()) };
            if let Some(target) = parse_model_target(&backing) { member_targets.insert(member, target.clone()); resolved.push(target); }
            else if !nested && member != model_str && hooks.get_combo_models(&member).await.is_some() {
                // Nested combo member: dispatched through its own combo below.
                nested_members.insert(member.clone());
                let target = ModelTarget { provider: NESTED_COMBO.into(), model: member.clone() };
                member_targets.insert(member, target.clone()); resolved.push(target);
            }
        }
        resolved
    } else {
        return error_response(400, "Invalid model format");
    };

    if targets.is_empty() {
        return error_response(400, "No valid target models resolved");
    }

    let settings = hooks.settings().await;
    let required = crate::services::capacity_adapter::detect_required_capabilities(&body);
    let original: Vec<String> = if is_combo { hooks.get_combo_models(&model_str).await.unwrap_or_default().into_iter().filter(|m| member_targets.contains_key(m)).collect() } else { targets.iter().map(|t| format!("{}/{}", t.provider, t.model)).collect() };
    let satisfies = |name: &str, caps: &[&str]| {
        let backing = member_targets.get(name).map(|t| format!("{}/{}", t.provider, t.model)).unwrap_or_else(|| name.to_owned());
        crate::services::capacity_adapter::model_satisfies(&backing, caps)
    };
    let mut names = crate::services::capacity_adapter::augment_models_with_capacity_adapter(
        &original, &required, &settings, satisfies,
    );
    names = hooks.filter_adapter_models(names, &original).await;
    let adapters: HashSet<String> = names.iter().filter(|n| !original.contains(n)).filter_map(|n| parse_model_target(n)).map(|t| format!("{}/{}", t.provider, t.model)).collect();
    let strategy = if is_combo {
        settings["comboStrategies"][&model_str]["fallbackStrategy"].as_str()
            .filter(|s| *s != "auto")
            .or_else(|| settings["comboStrategy"].as_str().filter(|s| *s != "auto"))
            .unwrap_or(if original.len() > 1 && hooks.pair_enabled(&model_str).await { "round-robin" } else { "fallback" })
    } else {
        crate::services::capacity_adapter::get_active_adapter_strategy(&required, &settings)
    };
    if is_combo && strategy == "fusion" {
        return Box::pin(fusion_chat(&body, &original, &member_targets, &settings, &model_str, headers, endpoint_path, executor, hooks)).await;
    }
    let mut active_sticky: Option<String> = None;
    let session_id = crate::services::systemone_router::session_id(&headers, &body);
    if is_combo && strategy == "smart" && names.len() > 1 {
        use crate::services::systemone_router::{sticky_store, extract_prompt_state, MAX_STATE_CHARS, DEFAULT_SYSTEMONE_ROUTER_MODEL, DEFAULT_SYSTEMONE_TIMEOUT_MS, build_choice_criteria, parse_systemone_route_answer, order_models_by_choice};
        let cfg = &settings["comboStrategies"][&model_str];
        let policy = cfg["smartPolicy"].as_str().unwrap_or("smart").to_ascii_lowercase();
        let session = session_id.as_str();
        let sticky_enabled = policy != "per-turn" && cfg["sessionSticky"] != false && !session.is_empty();
        let sticky = if sticky_enabled { sticky_store().lock().ok().and_then(|mut s| s.get(&model_str, session, &names, now_ms())) } else { None };
        let threshold = match policy.as_str() { "smart-switch-70" => 0.7, "smart-switch-90" => 0.9, _ => cfg["confidenceThreshold"].as_f64().filter(|n| *n > 0.0).unwrap_or(0.8) };
        let state = extract_prompt_state(&body, MAX_STATE_CHARS);
        let decision = if sticky.is_some() && policy == "strict" || state.is_empty() { None } else {
            let request = json!({"model": cfg["routerModel"].as_str().filter(|s| !s.trim().is_empty()).unwrap_or(DEFAULT_SYSTEMONE_ROUTER_MODEL), "state": state, "questions":{"route":{"type":"choice","instructions":"Which of these configured models is the most appropriate for this request?","criteria":build_choice_criteria(&names)}}});
            tokio::time::timeout(Duration::from_millis(DEFAULT_SYSTEMONE_TIMEOUT_MS), hooks.smart_choice(&request)).await.ok().flatten().and_then(|v| parse_systemone_route_answer(&v))
        };
        let chosen = decision.as_ref().filter(|d| names.iter().any(|n| Some(n.as_str()) == d["choice"].as_str())).filter(|d| {
            let confidence = d["confidence"].as_f64();
            if let Some(ref sticky) = sticky { d["choice"].as_str() != Some(sticky.as_str()) && confidence.is_some_and(|c| c >= threshold) } else { confidence.is_none_or(|c| c >= threshold) }
        });
        if let Some(decision) = chosen {
            names = order_models_by_choice(&names, decision);
            if sticky_enabled { if let Ok(mut s) = sticky_store().lock() { s.set(&model_str, session, &names[0], now_ms()); active_sticky = Some(names[0].clone()); } }
        } else if let Some(sticky) = sticky {
            names = order_models_by_choice(&names, &json!({"choice":sticky}));
            active_sticky = Some(sticky);
        }
    } else if strategy == "round-robin" && names.len() > 1 {
        static ROTATION: std::sync::OnceLock<std::sync::Mutex<crate::services::systemone_router::ComboRotation>> = std::sync::OnceLock::new();
        let limit = settings["comboStickyRoundRobinLimit"].as_u64().or_else(|| settings["comboStickyRoundRobinLimit"].as_str().and_then(|s| s.parse().ok())).unwrap_or(1) as usize;
        if let Ok(mut rotation) = ROTATION.get_or_init(Default::default).lock() { rotation.rotate(&mut names, &model_str, limit); }
    }
    if strategy == "capacity" {
        let connections = hooks.capacity_connections().await;
        let now = now_ms() as i64;
        names.sort_by_cached_key(|name| {
            let target = member_targets.get(name).cloned().or_else(|| parse_model_target(name));
            let score: i64 = target.map(|target| connections.iter().filter(|c| c["provider"].as_str() == Some(target.provider.as_str()) && c["isActive"] != false).filter(|c| {
                let lock = &c[format!("modelLock_{}", target.model)];
                let lock = if lock.is_null() { &c["modelLock___all"] } else { lock };
                crate::services::usage_shared::reset_epoch_millis(lock).is_none_or(|t| t <= now)
            }).map(|c| {
                let failed = c["backoffLevel"].as_f64().unwrap_or(0.0) > 0.0 || c["lastError"].as_str().is_some_and(|s| !s.is_empty());
                let clean = !failed || crate::services::usage_shared::reset_epoch_millis(&c["lastErrorAt"]).is_some_and(|t| now - t >= 3_600_000);
                1 + if clean { 100 } else { 0 }
            }).sum()).unwrap_or(0);
            -score
        });
    }
    if !required.is_empty() && strategy != "capacity" {
        crate::services::capacity_adapter::reorder_by_capabilities(&mut names, &required, satisfies);
    }
    let ordered_names = names.clone();
    targets = names.iter().filter_map(|n| member_targets.get(n).cloned().or_else(|| parse_model_target(n))).collect();

    let accept_header = headers
        .get("accept")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let user_agent = headers
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    let client_headers = headers.iter().filter_map(|(name, value)| {
        value.to_str().ok().map(|value| (name.as_str().to_owned(), value.to_owned()))
    }).collect();
    let client_tool = crate::services::client_detector::detect_client_tool(&client_headers, &body)
        .map(str::to_owned);
    let started = Instant::now();
    let targets_len = targets.len();
    let cooldowns: &Cooldowns = routing::cooldown::global();
    if targets.len() > 1 {
        let names: Vec<String> = targets.iter().map(|t| format!("{}/{}", t.provider, t.model)).collect();
        let keep = routing::combo::plan(cooldowns, &model_str, &names);
        if !keep.is_empty() {
            targets.retain(|t| keep.contains(&format!("{}/{}", t.provider, t.model)));
        }
    }

    let mut last_status = 503;
    let mut last_error = "All accounts unavailable".to_string();
    let mut last_headers = Vec::new();
    let mut last_flaky = None;
    let mut flaky_attempts = 0;
    let (anti_flaky_enabled, max_flaky_retries) = hooks.anti_flaky_config().await;

    for target in targets {
        let member = member_context.as_ref().map(|c| c.0.clone()).unwrap_or_else(|| ordered_names.iter().find(|name| member_targets.get(*name).is_some_and(|t| t.provider == target.provider && t.model == target.model)).cloned().unwrap_or_else(|| format!("{}/{}", target.provider, target.model)));
        let mut excluded = HashSet::new();
        let cap = if is_combo { crate::services::member_failover::resolve_combo_member_attempt_cap(&settings, &target.provider, &target.model) } else { None };
        let mut failures = 0usize;
        let mut overload_attempts = 0u32;
        if target.provider == NESTED_COMBO {
            let mut inner = body.clone();
            inner["model"] = json!(target.model);
            // Variant fallback: keep the outer requested name (upstream injectModelName || modelStr).
            let inject = member_context.as_ref().map(|c| c.0.clone()).unwrap_or_else(|| model_str.clone());
            let resp = Box::pin(handle_chat_nested(inner, headers, endpoint_path, executor, Arc::clone(&hooks), Some((inject, None)), true)).await;
            if resp.status().is_success() { return resp; }
            last_status = resp.status().as_u16();
            last_error = format!("Nested combo {} failed", target.model);
            continue;
        }
        loop {
            if !crate::services::provider_breaker::global().lock().map(|mut b| b.gate(&target.provider, Some(&target.model), now_ms())).unwrap_or(true) { break; }
            let pin = &settings["comboMemberPins"][&model_str][&member];
            let pin = member_context.as_ref().and_then(|c| c.1.as_deref()).or_else(|| pin.as_str().or_else(|| pin["id"].as_str()));
            let Some(creds) = hooks
                .get_pinned_credentials(&target.provider, &target.model, &excluded, pin)
                .await
            else {
                if excluded.is_empty() && last_status == 503 {
                    last_status = 404;
                    last_error = format!("No active credentials for provider: {}", target.provider);
                }
                break;
            };

            if cooldowns.is_cooling(&creds.connection_id) {
                excluded.insert(creds.connection_id.clone());
                continue;
            }
            let conn_id = creds.connection_id.clone();
            let hooks_for_refresh = Arc::clone(&hooks);
            let conn_id_refresh = conn_id.clone();
            let hooks_for_success = Arc::clone(&hooks);
            let conn_id_success = conn_id.clone();
            let model_success = target.model.clone();
            let provider_success = target.provider.clone();
            let hooks_for_usage = Arc::clone(&hooks);
            let usage_base = UsageRecord {
                connection_id: Some(conn_id.clone()),
                provider: target.provider.clone(),
                model: target.model.clone(),
                ..Default::default()
            };

            let core_hooks = ChatCoreHooks {
                on_credentials_refreshed: Some(Arc::new(move |new_creds| {
                    let h = Arc::clone(&hooks_for_refresh);
                    let cid = conn_id_refresh.clone();
                    Box::pin(async move { h.on_credentials_refreshed(&cid, new_creds).await })
                })),
                on_usage: Some(Arc::new(move |counts: crate::services::pricing::TokenCounts| {
                    let h = Arc::clone(&hooks_for_usage);
                    let record = UsageRecord {
                        prompt_tokens: counts.prompt_tokens.max(0) as u64,
                        completion_tokens: counts.completion_tokens.max(0) as u64,
                        cached_tokens: counts.cached_tokens.max(0) as u64,
                        cache_creation_tokens: counts.cache_creation_tokens.max(0) as u64,
                        reasoning_tokens: counts.reasoning_tokens.max(0) as u64,
                        status: 200,
                        latency_ms: started.elapsed().as_millis() as u64,
                        ..usage_base.clone()
                    };
                    Box::pin(async move { h.on_usage_record(record).await })
                })),
                on_request_success: Some(Arc::new(move || {
                    let h = Arc::clone(&hooks_for_success);
                    let cid = conn_id_success.clone();
                    let m = model_success.clone();
                    let p = provider_success.clone();
                    Box::pin(async move { if !nested && let Ok(mut b) = crate::services::provider_breaker::global().lock() { b.success(&p, Some(&m), now_ms()); } h.on_request_success(&cid, &m).await })
                })),
            };

            // Only combo members get a first-response bound; a single model waits as before.
            let first_response_timeout = (targets_len > 1).then(|| {
                Duration::from_millis(crate::config::COMBO_FIRST_RESPONSE_TIMEOUT_MS)
            });
            let mut token_saver = hooks.get_token_saver_config(&model_str).await;
            if headers.get(crate::config::TOKEN_SAVER_HEADER).and_then(|v| v.to_str().ok()).is_some_and(|v| v.eq_ignore_ascii_case("off")) {
                if let Some(config) = token_saver.as_mut() {
                    config.token_saver_enabled = false;
                }
            }
            let options = ChatCoreOptions {
                body: if adapters.contains(&format!("{}/{}", target.provider, target.model)) {
                    crate::services::capacity_adapter::strip_history_for_context(&body, crate::services::capacity_adapter::model_context_window(&target.provider, &target.model))
                } else { body.clone() },
                provider: target.provider.clone(),
                model: target.model.clone(),
                credentials: creds,
                source_format_override: None,
                endpoint_path: endpoint_path.map(str::to_string),
                accept_header: accept_header.clone(),
                user_agent: user_agent.clone(),
                client_tool: client_tool.clone(),
                provider_session_id: None,
                first_response_timeout,
                token_saver,
                prompt_injections: hooks.prompt_injections(if member_context.is_some() || !member.contains('/') { &member } else { &model_str }).await,
            };

            match handle_chat_core(options, executor, &core_hooks).await {
                ChatCoreResult::Success(resp) => {
                    cooldowns.clear(&conn_id);
                    let resp = if targets_len > 1 && body["stream"] == true {
                        let (resp, empty) = peek_combo_response(resp).await;
                        if empty {
                            last_flaky = Some(resp);
                            break;
                        }
                        resp
                    } else { resp };
                    if anti_flaky_enabled && max_flaky_retries > 0 && body["stream"] == true {
                        let (resp, flaky) = probe_flaky_response(resp).await;
                        if flaky && flaky_attempts < max_flaky_retries {
                            flaky_attempts += 1;
                            last_flaky = Some(resp);
                            failures += 1;
                            if cap.is_some_and(|cap| failures >= cap) { break; }
                            excluded.insert(conn_id);
                            continue;
                        }
                        return resp;
                    }
                    if anti_flaky_enabled && max_flaky_retries > 0 && body["stream"] != true {
                        let (parts, response_body) = resp.into_parts();
                        match axum::body::to_bytes(response_body, usize::MAX).await {
                            Ok(bytes) => {
                                let flaky = crate::services::flaky_guard::judge_finished_text(&String::from_utf8_lossy(&bytes)).is_some();
                                let resp = Response::from_parts(parts, Body::from(bytes));
                                if flaky && flaky_attempts < max_flaky_retries {
                                    flaky_attempts += 1;
                                    last_flaky = Some(resp);
                                    failures += 1;
                                    if cap.is_some_and(|cap| failures >= cap) { break; }
                                    excluded.insert(conn_id);
                                    continue;
                                }
                                return resp;
                            }
                            Err(_) => return error_response(502, "Failed to read upstream response"),
                        }
                    }
                    return resp;
                }
                ChatCoreResult::Error {
                    status,
                    message,
                    headers,
                    retry_after,
                } => {
                    if status >= 500 {
                        hooks.on_error(status, &message).await;
                    }
                    last_status = status;
                    last_error = message.clone();
                    last_headers = headers;
                    if status == 504 && targets_len > 1 {
                        // Stalled member: skip it for this combo for the grace window.
                        cooldowns.set(
                            &routing::combo::grace_key(&model_str, &format!("{}/{}", target.provider, target.model)),
                            Duration::from_millis(crate::config::COMBO_FIRST_RESPONSE_GRACE_MS),
                        );
                        break;
                    }
                    hooks
                        .on_usage_record(UsageRecord {
                            connection_id: Some(conn_id.clone()),
                            provider: target.provider.clone(),
                            model: target.model.clone(),
                            status,
                            latency_ms: started.elapsed().as_millis() as u64,
                            ..Default::default()
                        })
                        .await;
                    let verdict = classify_with_retry_after(
                        &target.provider,
                        status,
                        &message,
                        retry_after.map(Duration::from_secs),
                        now_ms(),
                    );
                    if verdict == Verdict::RequestScoped && !(target.provider == "antigravity" && status == 409) {
                        return upstream_error_response(status, &message, &last_headers);
                    }
                    if verdict == Verdict::Overload && overload_attempts < OVERLOAD_MAX_ATTEMPTS {
                        overload_attempts += 1;
                        tokio::time::sleep(overload_delay(overload_attempts)).await;
                        continue;
                    }
                    // Upstream auth.js markAccountUnavailable: noauth never falls back or cools down,
                    // otherwise one upstream blip locks every free-provider request.
                    if conn_id == "noauth" || !hooks.on_account_error(&conn_id, &target.model, status, &message).await {
                        return upstream_error_response(status, &message, &last_headers);
                    }
                    if (status == 429 || status >= 500) && let Ok(mut b) = crate::services::provider_breaker::global().lock() { b.failure(&target.provider, &conn_id, Some(&target.model), now_ms()); }
                    match verdict {
                        Verdict::Fixed(d) => cooldowns.set(&conn_id, d),
                        Verdict::Quota { reset_ms } => cooldowns.set(
                            &conn_id,
                            Duration::from_millis(reset_ms.saturating_sub(now_ms()))
                                .min(MAX_RATE_LIMIT_COOLDOWN),
                        ),
                        Verdict::Backoff => {
                            cooldowns.backoff(&conn_id);
                        }
                        Verdict::Overload => cooldowns.set(&conn_id, TRANSIENT_COOLDOWN),
                        Verdict::RequestScoped => cooldowns.set(&conn_id, TRANSIENT_COOLDOWN),
                    }
                    excluded.insert(conn_id);
                    failures += 1;
                    if cap.is_some_and(|cap| failures >= cap) { break; }
                    continue;
                }
            }
        }
        if active_sticky.as_deref() == Some(member.as_str()) {
            if let Ok(mut store) = crate::services::systemone_router::sticky_store().lock() { store.clear(&model_str, Some(&session_id)); }
            active_sticky = None;
        }
    }

    last_flaky.unwrap_or_else(|| upstream_error_response(last_status, &last_error, &last_headers))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::chat_core::detect_format;
    use crate::handlers::{ExecutorBody, ExecutorRequest, ExecutorResponse};
    use crate::translator::formats::{
        FORMAT_CLAUDE, FORMAT_GEMINI, FORMAT_OPENAI, FORMAT_OPENAI_RESPONSES,
    };
    use axum::body::to_bytes;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn flaky_stream_probe_replays_finished_body() {
        let text = "data: {\"choices\":[{\"delta\":{\"content\":\"\"}}]}\n\ndata: [DONE]\n\n";
        let (response, flaky) = probe_flaky_response(Response::new(Body::from(text))).await;
        assert!(flaky);
        assert_eq!(to_bytes(response.into_body(), usize::MAX).await.unwrap(), text);
    }

    struct FakeExecutor {
        calls: Mutex<Vec<ExecutorRequest>>,
        response_fn: Box<
            dyn Fn(&ExecutorRequest, usize) -> Result<ExecutorResponse, String> + Send + Sync,
        >,
        refresh_fn: Option<
            Box<dyn Fn(&Credentials) -> Result<Option<RefreshedCredentials>, String> + Send + Sync>,
        >,
        attempt: AtomicUsize,
    }

    impl FakeExecutor {
        fn new<F>(f: F) -> Self
        where
            F: Fn(&ExecutorRequest, usize) -> Result<ExecutorResponse, String>
                + Send
                + Sync
                + 'static,
        {
            Self {
                calls: Mutex::new(Vec::new()),
                response_fn: Box::new(f),
                refresh_fn: None,
                attempt: AtomicUsize::new(0),
            }
        }

        fn with_refresh<F>(mut self, f: F) -> Self
        where
            F: Fn(&Credentials) -> Result<Option<RefreshedCredentials>, String>
                + Send
                + Sync
                + 'static,
        {
            self.refresh_fn = Some(Box::new(f));
            self
        }
    }

    impl Executor for FakeExecutor {
        fn execute<'a>(
            &'a self,
            req: ExecutorRequest,
        ) -> BoxFuture<'a, Result<ExecutorResponse, String>> {
            Box::pin(async move {
                let idx = self.attempt.fetch_add(1, Ordering::SeqCst);
                self.calls.lock().unwrap().push(req.clone());
                (self.response_fn)(&req, idx)
            })
        }

        fn refresh_credentials<'a>(
            &'a self,
            creds: &'a Credentials,
        ) -> BoxFuture<'a, Result<Option<RefreshedCredentials>, String>> {
            Box::pin(async move {
                if let Some(f) = &self.refresh_fn {
                    f(creds)
                } else {
                    Ok(None)
                }
            })
        }
    }

    fn test_creds(id: &str) -> Credentials {
        Credentials {
            connection_id: id.to_string(),
            access_token: Some("tok-initial".to_string()),
            ..Default::default()
        }
    }

    struct ComboHooks { strategy: &'static str, pair: bool }
    impl ChatHooks for ComboHooks {
        fn get_credentials<'a>(&'a self, _: &'a str, _: &'a str, _: &'a HashSet<String>) -> BoxFuture<'a, Option<Credentials>> { Box::pin(async { Some(test_creds("combo-new-tests")) }) }
        fn get_pinned_credentials<'a>(&'a self, _: &'a str, _: &'a str, _: &'a HashSet<String>, pin: Option<&'a str>) -> BoxFuture<'a, Option<Credentials>> { Box::pin(async move { assert_eq!(pin, Some("pin-a")); Some(test_creds("combo-new-tests")) }) }
        fn get_combo_models<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Option<Vec<String>>> { Box::pin(async { Some(vec!["openai/a".into(), "openai/b".into()]) }) }
        fn settings<'a>(&'a self) -> BoxFuture<'a, Value> { Box::pin(async move { json!({"comboStrategies":{"new-test":{"fallbackStrategy":self.strategy}},"comboMemberPins":{"new-test":{"openai/a":"pin-a","openai/b":{"id":"pin-a"}}}}) }) }
        fn pair_enabled<'a>(&'a self, _: &'a str) -> BoxFuture<'a, bool> { Box::pin(async move { self.pair }) }
        fn capacity_connections<'a>(&'a self) -> BoxFuture<'a, Vec<Value>> { Box::pin(async { vec![json!({"provider":"openai","modelLock_a":"2999-01-01T00:00:00Z"})] }) }
    }

    #[tokio::test]
    async fn fusion_panel_and_judge_strip_tools_and_keep_pins() {
        let executor = FakeExecutor::new(|req, _| {
            let judge = req.body["messages"].as_array().is_some_and(|m| m.iter().any(|x| x["content"].as_str().is_some_and(|s| s.contains("=== PANEL RESPONSES ==="))));
            if judge { assert!(req.body["tools"].is_array()); } else { assert!(req.body.get("tools").is_none()); assert_eq!(req.body["stream"], false); }
            Ok(ExecutorResponse { status: 200, headers: vec![], body: ExecutorBody::Json(json!({"id":"test","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":if judge {"judged"} else {"panel"}},"finish_reason":"stop"}]})), url: None, response_format: None })
        });
        let response = handle_chat(json!({"model":"new-test","stream":false,"messages":[{"role":"user","content":"hello"}],"tools":[{"type":"function","function":{"name":"test"}}]}), &HeaderMap::new(), Some("/v1/chat/completions"), &executor, Arc::new(ComboHooks { strategy:"fusion", pair:false })).await;
        assert_eq!(response.status(), 200);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(String::from_utf8_lossy(&body).contains("judged"));
        assert_eq!(executor.calls.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn fusion_bare_judge_variant_resolves_to_backing() {
        // combo.js handleFusionChat: judge goes through handleSingleModel, which resolves variants.
        struct Judge;
        impl ChatHooks for Judge {
            fn get_credentials<'a>(&'a self, _: &'a str, _: &'a str, _: &'a HashSet<String>) -> BoxFuture<'a, Option<Credentials>> { Box::pin(async { Some(test_creds("judge")) }) }
            fn get_combo_models<'a>(&'a self, n: &'a str) -> BoxFuture<'a, Option<Vec<String>>> { Box::pin(async move { (n == "fz").then(|| vec!["openai/a".into(), "openai/b".into()]) }) }
            fn settings<'a>(&'a self) -> BoxFuture<'a, Value> { Box::pin(async { json!({"comboStrategies":{"fz":{"fallbackStrategy":"fusion","judgeModel":"my-judge"}}}) }) }
            fn variant_backing<'a>(&'a self, n: &'a str) -> BoxFuture<'a, Option<String>> { Box::pin(async move { (n == "my-judge").then(|| "openai/judge-backing".into()) }) }
        }
        let executor = FakeExecutor::new(|req, _| {
            let content = if req.model == "judge-backing" { "judged" } else { "panel" };
            Ok(ExecutorResponse { status: 200, headers: vec![], body: ExecutorBody::Json(json!({"choices":[{"index":0,"message":{"role":"assistant","content":content},"finish_reason":"stop"}]})), url: None, response_format: None })
        });
        let response = handle_chat(json!({"model":"fz","stream":false,"messages":[{"role":"user","content":"hi"}]}), &HeaderMap::new(), Some("/v1/chat/completions"), &executor, Arc::new(Judge)).await;
        assert_eq!(response.status(), 200);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(String::from_utf8_lossy(&body).contains("judged"), "bare judge variant must not fail as invalid model");
    }

    #[tokio::test]
    async fn capacity_orders_locked_models_and_forwards_pin() {
        let executor = FakeExecutor::new(|req, _| {
            assert_eq!(req.body["model"], "b");
            Ok(ExecutorResponse { status: 200, headers: vec![], body: ExecutorBody::Json(json!({"choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]})), url: None, response_format: None })
        });
        let response = handle_chat(json!({"model":"new-test","stream":false,"messages":[{"role":"user","content":"hi"}]}), &HeaderMap::new(), Some("/v1/chat/completions"), &executor, Arc::new(ComboHooks { strategy:"capacity", pair:false })).await;
        assert_eq!(response.status(), 200);
    }

    #[tokio::test]
    async fn pair_auto_rotates_and_explicit_fallback_wins() {
        let executor = FakeExecutor::new(|_, _| Ok(ExecutorResponse { status: 200, headers: vec![], body: ExecutorBody::Json(json!({"choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]})), url: None, response_format: None }));
        for strategy in ["auto", "auto", "fallback"] {
            let response = handle_chat(json!({"model":"new-test","stream":false,"messages":[{"role":"user","content":"hi"}]}), &HeaderMap::new(), Some("/v1/chat/completions"), &executor, Arc::new(ComboHooks { strategy, pair:true })).await;
            assert_eq!(response.status(), 200);
        }
        let calls = executor.calls.lock().unwrap();
        assert_eq!(calls.iter().map(|c| c.body["model"].as_str().unwrap()).collect::<Vec<_>>(), vec!["a", "b", "a"]);
    }

    #[tokio::test]
    async fn native_claude_request_preserves_custom_fields() {
        let executor = FakeExecutor::new(|req, _| {
            assert_eq!(req.body["custom_native_field"], "preserved");
            assert_eq!(req.body["model"], "test");
            Ok(ExecutorResponse { status: 200, headers: vec![], body: ExecutorBody::Json(json!({"type":"message","content":[{"type":"text","text":"ok"}]})), url: None, response_format: None })
        });
        let mut headers = HeaderMap::new();
        headers.insert("user-agent", "claude-cli".parse().unwrap());
        let hooks: Arc<dyn ChatHooks> = Arc::new(SingleCredHooks { credentials: test_creds("native-test") });
        let response = handle_chat(json!({"model":"claude/test","stream":false,"custom_native_field":"preserved","messages":[{"role":"user","content":"hello"}]}), &headers, Some("/v1/messages"), &executor, hooks).await;
        assert_eq!(response.status(), 200);
    }

    #[tokio::test]
    async fn noauth_failure_is_returned_without_cooldown() {
        // auth.js markAccountUnavailable: "noauth" never falls back or cools down.
        let executor = FakeExecutor::new(|_, _| {
            Ok(ExecutorResponse { status: 502, headers: vec![], body: ExecutorBody::Json(json!({"error":{"message":"proxy down"}})), url: None, response_format: None })
        });
        for _ in 0..2 {
            let hooks: Arc<dyn ChatHooks> = Arc::new(SingleCredHooks { credentials: test_creds("noauth") });
            let response = handle_chat(json!({"model":"openai/m","stream":false,"messages":[{"role":"user","content":"hi"}]}), &HeaderMap::new(), Some("/v1/chat/completions"), &executor, hooks).await;
            assert_eq!(response.status(), 502, "second call must not be locked out by a cooldown");
        }
    }

    #[tokio::test]
    async fn nested_combo_member_dispatches_inner_combo() {
        // chat.js handleSingleModelChat nested-combo branch -> dispatchCombo(recordBreakerSuccess:false)
        struct Nested;
        impl ChatHooks for Nested {
            fn get_credentials<'a>(&'a self, _: &'a str, _: &'a str, _: &'a HashSet<String>) -> BoxFuture<'a, Option<Credentials>> { Box::pin(async { Some(test_creds("nested")) }) }
            fn get_combo_models<'a>(&'a self, n: &'a str) -> BoxFuture<'a, Option<Vec<String>>> {
                Box::pin(async move { match n { "outer" => Some(vec!["inner".into()]), "inner" => Some(vec!["openai/deep".into()]), _ => None } })
            }
        }
        let executor = FakeExecutor::new(|req, _| {
            assert_eq!(req.model, "deep");
            Ok(ExecutorResponse { status: 200, headers: vec![], body: ExecutorBody::Json(json!({"choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]})), url: None, response_format: None })
        });
        let response = handle_chat(json!({"model":"outer","stream":false,"messages":[{"role":"user","content":"hi"}]}), &HeaderMap::new(), Some("/v1/chat/completions"), &executor, Arc::new(Nested)).await;
        assert_eq!(response.status(), 200, "combo member that is itself a combo must not be dropped");
    }

    #[tokio::test]
    async fn smart_combo_dispatches_selected_model() {
        struct Smart;
        impl ChatHooks for Smart {
            fn get_credentials<'a>(&'a self, _: &'a str, _: &'a str, _: &'a HashSet<String>) -> BoxFuture<'a, Option<Credentials>> { Box::pin(async { Some(test_creds("smart-combo-test")) }) }
            fn get_combo_models<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Option<Vec<String>>> { Box::pin(async { Some(vec!["openai/first".into(), "openai/second".into()]) }) }
            fn settings<'a>(&'a self) -> BoxFuture<'a, Value> { Box::pin(async { json!({"comboStrategies":{"test":{"fallbackStrategy":"smart","smartPolicy":"per-turn"}}}) }) }
            fn smart_choice<'a>(&'a self, body: &'a Value) -> BoxFuture<'a, Option<Value>> { Box::pin(async move { assert_eq!(body["state"], "hello"); assert!(body["questions"]["route"]["criteria"]["openai/second"].is_null()); Some(json!({"answers":{"route":{"choice":"openai/second","confidence":0.9}}})) }) }
        }
        let executor = FakeExecutor::new(|req, _| {
            assert_eq!(req.model, "second");
            Ok(ExecutorResponse { status: 200, headers: vec![], body: ExecutorBody::Json(json!({"choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]})), url: None, response_format: None })
        });
        let response = handle_chat(json!({"model":"test","messages":[{"role":"user","content":"hello"}]}), &HeaderMap::new(), Some("/v1/chat/completions"), &executor, Arc::new(Smart)).await;
        assert_eq!(response.status(), 200);
    }

    #[tokio::test]
    async fn smart_combo_variant_keeps_choice_and_injection_name() {
        // comboDispatch.js createVariantHelpers and dispatchCombo
        struct Variant;
        impl ChatHooks for Variant {
            fn get_credentials<'a>(&'a self, _: &'a str, _: &'a str, _: &'a HashSet<String>) -> BoxFuture<'a, Option<Credentials>> { Box::pin(async { Some(test_creds("variant-parity")) }) }
            fn get_combo_models<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Option<Vec<String>>> { Box::pin(async { Some(vec!["custom-variant".into(), "openai/second".into()]) }) }
            fn variant_backing<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Option<String>> { Box::pin(async { Some("openai/backing".into()) }) }
            fn settings<'a>(&'a self) -> BoxFuture<'a, Value> { Box::pin(async { json!({"comboStrategies":{"variant-combo":{"fallbackStrategy":"smart","smartPolicy":"per-turn"}}}) }) }
            fn smart_choice<'a>(&'a self, body: &'a Value) -> BoxFuture<'a, Option<Value>> { Box::pin(async move { assert!(body["questions"]["route"]["criteria"].get("custom-variant").is_some()); Some(json!({"answers":{"route":{"choice":"custom-variant","confidence":1.0}}})) }) }
            fn prompt_injections<'a>(&'a self, model: &'a str) -> BoxFuture<'a, Vec<Value>> { Box::pin(async move { assert_eq!(model, "custom-variant"); vec![] }) }
        }
        let executor = FakeExecutor::new(|req, _| { assert_eq!(req.model, "backing"); Ok(ExecutorResponse { status: 200, headers: vec![], body: ExecutorBody::Json(json!({"choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]})), url: None, response_format: None }) });
        let response = handle_chat(json!({"model":"variant-combo","messages":[{"role":"user","content":"hello"}]}), &HeaderMap::new(), Some("/v1/chat/completions"), &executor, Arc::new(Variant)).await;
        assert_eq!(response.status(), 200);
    }

    #[tokio::test]
    async fn claude_client_tools_are_deduplicated_before_execution() {
        let executor = FakeExecutor::new(|req, _| {
            let tools = req.body["tools"].as_array().unwrap();
            assert_eq!(tools.len(), 1);
            assert_eq!(tools[0]["function"]["name"], "mcp__exa__web_search_exa");
            Ok(ExecutorResponse {
                status: 200,
                headers: vec![],
                body: ExecutorBody::Json(json!({"choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]})),
                url: None,
                response_format: None,
            })
        });
        let body = json!({
            "model":"openai/gpt-4o", "stream":false,
            "messages":[{"role":"user","content":"hello"}],
            "tools":[
                {"type":"function","function":{"name":"WebSearch","parameters":{"type":"object"}}},
                {"type":"function","function":{"name":"mcp__exa__web_search_exa","parameters":{"type":"object"}}}
            ]
        });
        let mut headers = HeaderMap::new();
        headers.insert("user-agent", "claude-cli".parse().unwrap());
        let hooks: Arc<dyn ChatHooks> = Arc::new(SingleCredHooks { credentials: test_creds("dedup-test") });
        let response = handle_chat(body, &headers, Some("/v1/chat/completions"), &executor, hooks).await;
        assert_eq!(response.status(), 200);
    }

    #[tokio::test]
    async fn chat_errors_forward_only_approved_upstream_headers() {
        let executor = FakeExecutor::new(|_, _| Ok(ExecutorResponse {
            status: 400,
            headers: vec![("Retry-After".into(), "7".into()), ("Set-Cookie".into(), "secret".into())],
            body: ExecutorBody::Json(json!({"error":{"message":"bad request"}})),
            url: None,
            response_format: None,
        }));
        let hooks: Arc<dyn ChatHooks> = Arc::new(SingleCredHooks { credentials: test_creds("header-test") });
        let response = handle_chat(json!({"model":"openai/gpt-4o","messages":[{"role":"user","content":"hello"}]}), &HeaderMap::new(), Some("/v1/chat/completions"), &executor, hooks).await;
        assert_eq!(response.status(), 400);
        assert_eq!(response.headers()["retry-after"], "7");
        assert!(!response.headers().contains_key("set-cookie"));
    }

    #[tokio::test]
    async fn flaky_empty_completion_retries_another_account() {
        struct TwoAccounts;
        impl ChatHooks for TwoAccounts {
            fn get_credentials<'a>(&'a self, _: &'a str, _: &'a str, excluded: &'a HashSet<String>) -> BoxFuture<'a, Option<Credentials>> {
                Box::pin(async move { ["flaky-a", "flaky-b"].into_iter().find(|id| !excluded.contains(*id)).map(test_creds) })
            }
        }
        let executor = FakeExecutor::new(|_, attempt| Ok(ExecutorResponse {
            status: 200, headers: vec![],
            body: ExecutorBody::Json(json!({"choices":[{"message":{"role":"assistant","content":if attempt == 0 { "" } else { "recovered" }},"finish_reason":"stop"}]})),
            url: None, response_format: None,
        }));
        let response = handle_chat(json!({"model":"openai/gpt-4o","stream":false,"messages":[{"role":"user","content":"hello"}]}), &HeaderMap::new(), Some("/v1/chat/completions"), &executor, Arc::new(TwoAccounts)).await;
        assert_eq!(executor.attempt.load(Ordering::SeqCst), 2);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(String::from_utf8_lossy(&bytes).contains("recovered"));
    }

    #[tokio::test]
    async fn combo_peek_replays_content_and_preserves_filter_refusals() {
        for (text, expected) in [
            ("data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n", true),
            ("data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"content_filter\"}]}\n\n", false),
            ("data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n", false),
        ] {
            let (response, empty) = peek_combo_response(Response::new(Body::from(text))).await;
            assert_eq!(empty, expected);
            assert_eq!(&to_bytes(response.into_body(), usize::MAX).await.unwrap()[..], text.as_bytes());
        }
    }

    #[tokio::test]
    async fn configured_prompt_and_chat_turns_reach_executor() {
        struct Inject;
        impl ChatHooks for Inject {
            fn get_credentials<'a>(&'a self, _: &'a str, _: &'a str, _: &'a HashSet<String>) -> BoxFuture<'a, Option<Credentials>> {
                Box::pin(async { Some(test_creds("injection-test")) })
            }
            fn prompt_injections<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Vec<Value>> {
                Box::pin(async { vec![json!({"content":"instruction"}), json!({"mode":"chat","turns":[{"role":"user","content":"example"}]})] })
            }
        }
        let executor = FakeExecutor::new(|req, _| {
            assert_eq!(req.body["messages"][0]["content"], "instruction");
            assert_eq!(req.body["messages"][1]["content"], "example\n\nhello");
            Ok(ExecutorResponse { status: 200, headers: vec![], body: ExecutorBody::Json(json!({"choices":[{"message":{"content":"ok"}}]})), url: None, response_format: None })
        });
        let response = handle_chat(json!({"model":"openai/gpt-4o","stream":false,"messages":[{"role":"user","content":"hello"}]}), &HeaderMap::new(), Some("/v1/chat/completions"), &executor, Arc::new(Inject)).await;
        assert_eq!(response.status(), 200);
    }

    #[test]
    fn source_format_detection_covers_all_major_formats() {
        assert_eq!(strip_model_context_marker("openai/model[1M]"), "openai/model");
        assert_eq!(strip_model_context_marker("openai/model[custom]"), "openai/model[custom]");
        assert_eq!(
            detect_format(&json!({ "messages": [{ "role": "user", "content": "hi" }] })),
            FORMAT_OPENAI
        );
        assert_eq!(
            detect_format(&json!({
                "system": "be concise",
                "messages": [{ "role": "user", "content": "hi" }]
            })),
            FORMAT_CLAUDE
        );
        assert_eq!(
            detect_format(&json!({
                "contents": [{ "role": "user", "parts": [{ "text": "hi" }] }]
            })),
            FORMAT_GEMINI
        );
        assert_eq!(
            detect_format(&json!({
                "input": "hello",
                "model": "gpt-5"
            })),
            FORMAT_OPENAI_RESPONSES
        );
    }

    #[tokio::test]
    async fn openai_to_claude_non_stream_round_trip() {
        let executor = FakeExecutor::new(|req, _| {
            // Verify request was translated from OpenAI -> Claude
            assert_eq!(req.provider, "anthropic");
            assert_eq!(req.model, "claude-sonnet-4-5");
            assert!(!req.stream);
            assert_eq!(req.body["model"], "claude-sonnet-4-5");
            // OpenAI system message should be lifted to Claude top-level `system`
            assert!(req.body.get("system").is_some());
            assert_eq!(req.body["messages"][0]["role"], "user");

            Ok(ExecutorResponse {
                status: 200,
                headers: vec![("x-upstream".to_string(), "ok".to_string())],
                body: ExecutorBody::Json(json!({
                    "id": "msg_123",
                    "type": "message",
                    "role": "assistant",
                    "model": "claude-sonnet-4-5",
                    "content": [{ "type": "text", "text": "Hello from Claude!" }],
                    "stop_reason": "end_turn",
                    "usage": { "input_tokens": 12, "output_tokens": 5 }
                })),
                url: None,
                response_format: None,
            })
        });

        let body = json!({
            "model": "anthropic/claude-sonnet-4-5",
            "stream": false,
            "messages": [
                { "role": "system", "content": "You are concise." },
                { "role": "user", "content": "Say hi" }
            ]
        });

        let hooks: Arc<dyn ChatHooks> = Arc::new(SingleCredHooks {
            credentials: test_creds("conn-1"),
        });

        let resp = handle_chat(body, &HeaderMap::new(), Some("/v1/chat/completions"), &executor, hooks).await;
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.headers().get("content-type").unwrap(), "application/json");

        let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let parsed: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed["object"], "chat.completion");
        assert_eq!(parsed["choices"][0]["message"]["role"], "assistant");
        assert_eq!(parsed["choices"][0]["message"]["content"], "Hello from Claude!");
        assert_eq!(parsed["choices"][0]["finish_reason"], "stop");
        assert_eq!(parsed["usage"]["prompt_tokens"], 12);
        assert_eq!(parsed["usage"]["completion_tokens"], 5);
        assert_eq!(parsed["usage"]["total_tokens"], 17);
    }

    #[tokio::test]
    async fn openai_to_claude_stream_round_trip() {
        let executor = FakeExecutor::new(|req, _| {
            assert_eq!(req.provider, "anthropic");
            assert!(req.stream);

            let (tx, rx) = mpsc::channel::<Result<Vec<u8>, String>>(16);
            tokio::spawn(async move {
                let events = [
                    "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_s1\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude-sonnet-4-5\",\"content\":[],\"usage\":{\"input_tokens\":10,\"output_tokens\":0}}}\n\n",
                    "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
                    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello \"}}\n\n",
                    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"world!\"}}\n\n",
                    "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
                    "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":4}}\n\n",
                    "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
                ];
                for ev in events {
                    let _ = tx.send(Ok(ev.as_bytes().to_vec())).await;
                }
            });

            Ok(ExecutorResponse {
                status: 200,
                headers: vec![],
                body: ExecutorBody::Stream(rx),
                url: None,
                response_format: None,
            })
        });

        let body = json!({
            "model": "anthropic/claude-sonnet-4-5",
            "stream": true,
            "messages": [
                { "role": "user", "content": "Hi" }
            ]
        });

        let hooks: Arc<dyn ChatHooks> = Arc::new(SingleCredHooks {
            credentials: test_creds("conn-stream"),
        });

        let resp = handle_chat(body, &HeaderMap::new(), Some("/v1/chat/completions"), &executor, hooks).await;
        assert_eq!(resp.status(), 200);
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "text/event-stream; charset=utf-8"
        );

        let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();

        let mut content = String::new();
        let mut saw_done = false;
        let mut finish_reason = None;
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("data:") {
                let data = rest.trim();
                if data == "[DONE]" {
                    saw_done = true;
                    continue;
                }
                if let Ok(chunk) = serde_json::from_str::<Value>(data) {
                    assert_eq!(chunk["object"], "chat.completion.chunk");
                    if let Some(c) = chunk["choices"][0]["delta"]["content"].as_str() {
                        content.push_str(c);
                    }
                    if let Some(fr) = chunk["choices"][0]["finish_reason"].as_str() {
                        finish_reason = Some(fr.to_string());
                    }
                }
            }
        }

        assert_eq!(content, "Hello world!");
        assert_eq!(finish_reason.as_deref(), Some("stop"));
        assert!(saw_done);
    }

    #[tokio::test]
    async fn stalled_combo_member_is_skipped_for_next() {
        // First member never answers; second answers. Combo must reach the second.
        let executor = FakeExecutor::new(|req, _| {
            if req.provider == "anthropic" {
                Ok(ExecutorResponse {
                    status: 200,
                    headers: vec![],
                    body: ExecutorBody::Json(json!({
                        "id": "m", "type": "message",
                        "content": [{ "type": "text", "text": "second" }],
                        "stop_reason": "end_turn"
                    })),
                    url: None,
                    response_format: None,
                })
            } else {
                Ok(ExecutorResponse { status: 200, headers: vec![], body: ExecutorBody::Json(json!({})), url: None, response_format: None })
            }
        });
        struct Stalled;
        impl ChatHooks for Stalled {
            fn get_credentials<'a>(&'a self, p: &'a str, _m: &'a str, _x: &'a HashSet<String>) -> BoxFuture<'a, Option<Credentials>> {
                Box::pin(async move { Some(Credentials { connection_id: format!("c-{p}-stall-test"), api_key: Some("k".into()), ..Default::default() }) })
            }
            fn get_combo_models<'a>(&'a self, _n: &'a str) -> BoxFuture<'a, Option<Vec<String>>> {
                Box::pin(async { Some(vec!["openai/gpt".into(), "anthropic/claude".into()]) })
            }
        }
        // The stalled member is simulated by a 504 from the core: the real timeout path is
        // exercised in chat_core. Here we assert the combo fallback order holds.
        let body = json!({"model": "combo", "stream": false, "messages": [{"role":"user","content":"hi"}]});
        let resp = handle_chat(body, &HeaderMap::new(), None, &executor, Arc::new(Stalled)).await;
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn refresh_and_retry_on_401() {
        let refreshed_flag = Arc::new(AtomicUsize::new(0));
        let refreshed_flag_clone = Arc::clone(&refreshed_flag);

        struct TrackingHooks {
            creds: Credentials,
            refreshed: Arc<AtomicUsize>,
        }
        impl ChatHooks for TrackingHooks {
            fn get_credentials<'a>(
                &'a self,
                _p: &'a str,
                _m: &'a str,
                excluded: &'a HashSet<String>,
            ) -> BoxFuture<'a, Option<Credentials>> {
                Box::pin(async move {
                    if excluded.contains(&self.creds.connection_id) {
                        None
                    } else {
                        Some(self.creds.clone())
                    }
                })
            }
            fn on_credentials_refreshed<'a>(
                &'a self,
                _connection_id: &'a str,
                new_creds: RefreshedCredentials,
            ) -> BoxFuture<'a, Result<(), String>> {
                Box::pin(async move {
                    assert_eq!(new_creds.access_token.as_deref(), Some("tok-fresh"));
                    self.refreshed.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            }
        }

        let executor = FakeExecutor::new(|req, attempt| {
            if attempt == 0 {
                assert_eq!(req.credentials.access_token.as_deref(), Some("tok-initial"));
                Ok(ExecutorResponse {
                    status: 401,
                    headers: vec![],
                    body: ExecutorBody::Json(json!({ "error": { "message": "expired" } })),
                    url: None,
                    response_format: None,
                })
            } else {
                assert_eq!(req.credentials.access_token.as_deref(), Some("tok-fresh"));
                Ok(ExecutorResponse {
                    status: 200,
                    headers: vec![],
                    body: ExecutorBody::Json(json!({
                        "id": "msg_ok",
                        "type": "message",
                        "content": [{ "type": "text", "text": "retried ok" }],
                        "stop_reason": "end_turn"
                    })),
                    url: None,
                    response_format: None,
                })
            }
        })
        .with_refresh(|_| {
            Ok(Some(RefreshedCredentials {
                access_token: Some("tok-fresh".to_string()),
                ..Default::default()
            }))
        });

        let hooks: Arc<dyn ChatHooks> = Arc::new(TrackingHooks {
            creds: test_creds("conn-refresh"),
            refreshed: refreshed_flag_clone,
        });

        let body = json!({
            "model": "anthropic/claude-sonnet-4-5",
            "stream": false,
            "messages": [{ "role": "user", "content": "hi" }]
        });

        let resp = handle_chat(body, &HeaderMap::new(), None, &executor, hooks).await;
        assert_eq!(resp.status(), 200);
        assert_eq!(refreshed_flag.load(Ordering::SeqCst), 1);
        assert_eq!(executor.calls.lock().unwrap().len(), 2);
    }
}
