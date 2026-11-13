use axum::body::Body;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::Arc;

use crate::handlers::chat_core::{
    ChatCoreHooks, ChatCoreOptions, ChatCoreResult, handle_chat_core,
};
use crate::handlers::{BoxFuture, Credentials, Executor, RefreshedCredentials};
use crate::providers;
use crate::routing::classify::{
    MAX_RATE_LIMIT_COOLDOWN, OVERLOAD_MAX_ATTEMPTS, TRANSIENT_COOLDOWN, Verdict, classify, now_ms,
    overload_delay,
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
    if let Some(idx) = model.rfind('[')
        && model.ends_with(']')
    {
        return &model[..idx];
    }
    model
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

    let mut targets = if let Some(t) = parse_model_target(&model_str) {
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

    let started = Instant::now();
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

            let options = ChatCoreOptions {
                body: body.clone(),
                provider: target.provider.clone(),
                model: target.model.clone(),
                credentials: creds,
                source_format_override: None,
                endpoint_path: endpoint_path.map(str::to_string),
                accept_header: accept_header.clone(),
                user_agent: user_agent.clone(),
                client_tool: None,
                provider_session_id: None,
            };

            match handle_chat_core(options, executor, &core_hooks).await {
                ChatCoreResult::Success(resp) => {
                    cooldowns.clear(&conn_id);
                    return resp;
                }
                ChatCoreResult::Error {
                    status,
                    message,
                    headers: _,
                } => {
                    last_status = status;
                    last_error = message.clone();
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
                    let verdict = classify(&target.provider, status, &message, now_ms());
                    if verdict == Verdict::RequestScoped {
                        return error_response(status, &message);
                    }
                    if verdict == Verdict::Overload && overload_attempts < OVERLOAD_MAX_ATTEMPTS {
                        overload_attempts += 1;
                        tokio::time::sleep(overload_delay(overload_attempts)).await;
                        continue;
                    }
                    if !hooks.on_account_error(&conn_id, status, &message).await {
                        return error_response(status, &message);
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

    error_response(last_status, &last_error)
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

    #[test]
    fn source_format_detection_covers_all_major_formats() {
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
