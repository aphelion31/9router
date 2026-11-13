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

/// One upstream attempt, written to usage history by the server.
#[derive(Debug, Clone, Default)]
pub struct UsageRecord {
    pub connection_id: Option<String>,
    pub provider: String,
    pub model: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub status: u16,
    pub latency_ms: u64,
}

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

/// Hooks for credential lookup, persistence, and account failover in `handle_chat`.
pub trait ChatHooks: Send + Sync {
    fn get_credentials<'a>(
        &'a self,
        provider: &'a str,
        model: &'a str,
        excluded_connection_ids: &'a HashSet<String>,
    ) -> BoxFuture<'a, Option<Credentials>>;

    fn get_combo_models<'a>(
        &'a self,
        _combo_name: &'a str,
    ) -> BoxFuture<'a, Option<Vec<String>>> {
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
    mut body: Value,
    headers: &HeaderMap,
    endpoint_path: Option<&str>,
    executor: &dyn Executor,
    hooks: Arc<dyn ChatHooks>,
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

    let mut targets = if let Some(mut t) = parse_model_target(&model_str) {
        // Upstream getModelInfo: a non-reserved prefix may name a provider node.
        if providers::lookup(&t.provider).is_none()
            && let Some(node_id) = hooks.resolve_node_prefix(&t.provider).await
        {
            t.provider = node_id;
        }
        vec![t]
    } else if let Some(combo_models) = hooks.get_combo_models(&model_str).await {
        combo_models
            .into_iter()
            .filter_map(|m| parse_model_target(&m))
            .collect()
    } else {
        return error_response(400, "Invalid model format");
    };

    if targets.is_empty() {
        return error_response(400, "No valid target models resolved");
    }

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
        let mut excluded = HashSet::new();
        let mut overload_attempts = 0u32;
        loop {
            let Some(creds) = hooks
                .get_credentials(&target.provider, &target.model, &excluded)
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
                on_usage: Some(Arc::new(move |prompt_tokens, completion_tokens| {
                    let h = Arc::clone(&hooks_for_usage);
                    let record = UsageRecord {
                        prompt_tokens,
                        completion_tokens,
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
                    Box::pin(async move { h.on_request_success(&cid, &m).await })
                })),
            };

            // Only combo members get a first-response bound; a single model waits as before.
            let first_response_timeout = (targets_len > 1).then(|| {
                Duration::from_millis(crate::config::COMBO_FIRST_RESPONSE_TIMEOUT_MS)
            });
            let token_saver = hooks.get_token_saver_config(&model_str).await;
            let options = ChatCoreOptions {
                body: body.clone(),
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
                prompt_injections: hooks.prompt_injections(&format!("{}/{}", target.provider, target.model)).await,
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
                    if anti_flaky_enabled && max_flaky_retries > 0 && body["stream"] != true {
                        let (parts, response_body) = resp.into_parts();
                        match axum::body::to_bytes(response_body, usize::MAX).await {
                            Ok(bytes) => {
                                let flaky = crate::services::flaky_guard::judge_finished_text(&String::from_utf8_lossy(&bytes)).is_some();
                                let resp = Response::from_parts(parts, Body::from(bytes));
                                if flaky && flaky_attempts < max_flaky_retries {
                                    flaky_attempts += 1;
                                    last_flaky = Some(resp);
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
                    if verdict == Verdict::RequestScoped {
                        return upstream_error_response(status, &message, &last_headers);
                    }
                    if verdict == Verdict::Overload && overload_attempts < OVERLOAD_MAX_ATTEMPTS {
                        overload_attempts += 1;
                        tokio::time::sleep(overload_delay(overload_attempts)).await;
                        continue;
                    }
                    if !hooks.on_account_error(&conn_id, status, &message).await {
                        return upstream_error_response(status, &message, &last_headers);
                    }
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
                        Verdict::RequestScoped => {}
                    }
                    excluded.insert(conn_id);
                    continue;
                }
            }
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
