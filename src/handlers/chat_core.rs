#![allow(dead_code)]

use axum::body::Body;
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::response::Response;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::handlers::sse::{SseLine, parse_sse_line, translate_sse_to_body};
use crate::handlers::{
    BoxFuture, Credentials, Executor, ExecutorBody, ExecutorRequest, ExecutorResponse,
    RefreshedCredentials,
};
use crate::providers;
use crate::translator::formats::{
    FORMAT_ANTIGRAVITY, FORMAT_CLAUDE, FORMAT_GEMINI, FORMAT_GEMINI_CLI, FORMAT_OPENAI,
    FORMAT_OPENAI_RESPONSES, FORMAT_VERTEX, detect_format_by_endpoint,
};
use crate::translator::{needs_translation, normalize_format, translate_request};

pub type RefreshHook =
    Arc<dyn Fn(RefreshedCredentials) -> BoxFuture<'static, Result<(), String>> + Send + Sync>;
pub type SuccessHook = Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>;
/// Called once per completed upstream call with `(prompt_tokens, completion_tokens)`.
/// Streaming reports the last usage seen in the SSE stream, sent when the stream ends.
pub type UsageHook = Arc<dyn Fn(u64, u64) -> BoxFuture<'static, ()> + Send + Sync>;

#[derive(Default, Clone)]
pub struct ChatCoreHooks {
    pub on_credentials_refreshed: Option<RefreshHook>,
    pub on_request_success: Option<SuccessHook>,
    pub on_usage: Option<UsageHook>,
}

/// Token counts from a raw upstream body (OpenAI, Claude, Gemini, Responses shapes).
pub fn usage_from_body(body: &Value) -> Option<(u64, u64)> {
    let u = body.get("usage").or_else(|| body.get("usageMetadata"))?;
    let pick = |keys: &[&str]| keys.iter().find_map(|k| u.get(*k).and_then(Value::as_u64));
    let input = pick(&["prompt_tokens", "input_tokens", "promptTokenCount"]);
    let output = pick(&["completion_tokens", "output_tokens", "candidatesTokenCount"]);
    if input.is_none() && output.is_none() {
        return None;
    }
    Some((input.unwrap_or(0), output.unwrap_or(0)))
}

#[derive(Debug, Clone, Default)]
pub struct TokenSaverConfig {
    pub token_saver_enabled: bool,
    pub rtk_enabled: bool,
    pub caveman_enabled: bool,
    pub caveman_level: Option<String>,
    pub brief_level: Option<String>,
    pub ponytail_enabled: bool,
    pub ponytail_level: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ChatCoreOptions {
    pub body: Value,
    pub provider: String,
    pub model: String,
    pub credentials: Credentials,
    pub source_format_override: Option<String>,
    pub endpoint_path: Option<String>,
    pub accept_header: Option<String>,
    pub user_agent: Option<String>,
    pub client_tool: Option<String>,
    pub provider_session_id: Option<String>,
    /// Upper bound for upstream response headers. Exceeding it returns status 504.
    pub first_response_timeout: Option<std::time::Duration>,
    pub token_saver: Option<TokenSaverConfig>,
}

pub enum ChatCoreResult {
    Success(Response),
    Error {
        status: u16,
        message: String,
        headers: Vec<(String, String)>,
        /// Upstream `retry-after` header, in seconds.
        retry_after: Option<u64>,
    },
}

/// Detect request format from body structure (port of `detectFormat` in `open-sse/services/provider.js`).
pub fn detect_format(body: &Value) -> &'static str {
    if let Some(input) = body.get("input")
        && (input.is_array() || input.is_string())
        && body.get("messages").is_none()
    {
        return FORMAT_OPENAI_RESPONSES;
    }

    if body.get("request").and_then(|r| r.get("contents")).is_some()
        && body.get("userAgent").and_then(|v| v.as_str()) == Some("antigravity")
    {
        return FORMAT_ANTIGRAVITY;
    }

    if body.get("contents").is_some_and(|v| v.is_array()) {
        return FORMAT_GEMINI;
    }

    // OpenAI-specific indicators (checked BEFORE Claude)
    let has_openai_field = body.get("stream_options").is_some_and(|v| !v.is_null())
        || body.get("response_format").is_some_and(|v| !v.is_null())
        || body.get("logprobs").is_some()
        || body.get("top_logprobs").is_some()
        || body.get("n").is_some()
        || body.get("presence_penalty").is_some()
        || body.get("frequency_penalty").is_some()
        || body.get("logit_bias").is_some_and(|v| !v.is_null())
        || body.get("user").is_some_and(|v| !v.is_null());

    if has_openai_field {
        return FORMAT_OPENAI;
    }

    if let Some(messages) = body.get("messages").and_then(|v| v.as_array()) {
        if let Some(first_content) = messages
            .first()
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array())
        {
            let first_type = first_content
                .first()
                .and_then(|c| c.get("type"))
                .and_then(|t| t.as_str());
            let model_has_slash = body
                .get("model")
                .and_then(|m| m.as_str())
                .is_some_and(|m| m.contains('/'));

            if first_type == Some("text") && !model_has_slash {
                if body.get("system").is_some_and(|v| !v.is_null())
                    || body.get("anthropic_version").is_some_and(|v| !v.is_null())
                {
                    return FORMAT_CLAUDE;
                }

                let has_claude_image = first_content.iter().any(|c| {
                    c.get("type").and_then(|t| t.as_str()) == Some("image")
                        && c.get("source")
                            .and_then(|s| s.get("type"))
                            .and_then(|t| t.as_str())
                            == Some("base64")
                });
                if has_claude_image {
                    return FORMAT_CLAUDE;
                }

                let has_openai_image = first_content.iter().any(|c| {
                    c.get("type").and_then(|t| t.as_str()) == Some("image_url")
                        && c.get("image_url")
                            .and_then(|u| u.get("url"))
                            .is_some_and(|v| !v.is_null())
                });
                if has_openai_image {
                    return FORMAT_OPENAI;
                }

                let has_claude_tool = first_content.iter().any(|c| {
                    matches!(
                        c.get("type").and_then(|t| t.as_str()),
                        Some("tool_use") | Some("tool_result")
                    )
                });
                if has_claude_tool {
                    return FORMAT_CLAUDE;
                }
            }
        }

        if body.get("system").is_some()
            || body
                .get("anthropic_version")
                .is_some_and(|v| !v.is_null() && v.as_str() != Some(""))
        {
            return FORMAT_CLAUDE;
        }
    }

    FORMAT_OPENAI
}

/// Detect source format from endpoint path first, falling back to body inspection.
pub fn detect_source_format(endpoint_path: Option<&str>, body: &Value) -> &'static str {
    if let Some(path) = endpoint_path {
        let by_ep = detect_format_by_endpoint(path);
        if by_ep != FORMAT_OPENAI {
            return by_ep;
        }
    }
    detect_format(body)
}

/// Resolve target wire format for a provider/model pair.
pub fn get_target_format(
    provider: &str,
    model: &str,
    source_format: &str,
    credentials: Option<&Credentials>,
) -> String {
    if provider.starts_with("openai-compatible-") {
        let api_type = credentials
            .and_then(|c| c.provider_specific_data.get("apiType"))
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| {
                if provider.contains("responses") {
                    "responses"
                } else {
                    "chat"
                }
            });
        return if api_type == "responses" {
            FORMAT_OPENAI_RESPONSES.to_string()
        } else {
            FORMAT_OPENAI.to_string()
        };
    }

    if provider.starts_with("anthropic-compatible-") {
        return FORMAT_CLAUDE.to_string();
    }

    if let Some(p) = providers::lookup(provider) {
        if let Some(m) = p.models.iter().find(|m| m.id == model) {
            if let Some(supported) = m
                .metadata
                .get("supportedFormats")
                .and_then(|v| v.as_array())
                && supported
                    .iter()
                    .any(|f| f.as_str() == Some(source_format))
            {
                return source_format.to_string();
            }
            if let Some(tf) = m.metadata.get("targetFormat").and_then(|v| v.as_str()) {
                return tf.to_string();
            }
        }
        if let Some(fmt) = &p.format {
            return fmt.clone();
        }
    }

    FORMAT_OPENAI.to_string()
}

/// Remove translator-internal continuity fields from outbound `messages`.
pub fn strip_continuity_fields(body: &mut Value) {
    if let Some(messages) = body.get_mut("messages").and_then(|m| m.as_array_mut()) {
        for msg in messages {
            if let Some(obj) = msg.as_object_mut() {
                obj.remove("encrypted_content");
                obj.remove("reasoning_encrypted_content");
            }
        }
    }
}

fn strip_thinking_suffix(model: &str) -> &str {
    // Strip trailing "(high)", "(medium)", "(low)", "(none)" thinking suffix if present.
    if let Some(idx) = model.rfind('(')
        && model.ends_with(')')
    {
        return model[..idx].trim_end();
    }
    model
}

fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Translate a non-streaming response body from `target_format` (provider) -> `source_format` (client).
pub fn translate_non_streaming_response(
    response_body: &Value,
    target_format: &str,
    source_format: &str,
) -> Value {
    let tgt = normalize_format(target_format);
    let src = normalize_format(source_format);
    if tgt == src {
        return response_body.clone();
    }

    // Step 1: Convert provider format -> OpenAI chat.completion
    let openai_body = match tgt {
        FORMAT_OPENAI => response_body.clone(),
        FORMAT_CLAUDE => claude_body_to_openai(response_body),
        FORMAT_GEMINI | FORMAT_ANTIGRAVITY | FORMAT_GEMINI_CLI | FORMAT_VERTEX => {
            gemini_body_to_openai(response_body)
        }
        FORMAT_OPENAI_RESPONSES => responses_body_to_openai(response_body),
        _ => response_body.clone(),
    };

    // Step 2: Convert OpenAI chat.completion -> client source_format
    match src {
        FORMAT_OPENAI => openai_body,
        FORMAT_CLAUDE => openai_body_to_claude(&openai_body),
        FORMAT_OPENAI_RESPONSES => openai_body_to_responses(&openai_body),
        _ => openai_body,
    }
}

fn claude_body_to_openai(body: &Value) -> Value {
    if body.get("choices").is_some() {
        return body.clone();
    }
    let mut text_content = String::new();
    let mut thinking_content = String::new();
    let mut tool_calls = Vec::new();

    if let Some(blocks) = body.get("content").and_then(|v| v.as_array()) {
        for block in blocks {
            match block.get("type").and_then(|v| v.as_str()) {
                Some("text") => {
                    if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                        text_content.push_str(t);
                    }
                }
                Some("thinking") => {
                    if let Some(t) = block.get("thinking").and_then(|v| v.as_str()) {
                        thinking_content.push_str(t);
                    }
                }
                Some("tool_use") => {
                    let id = block.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    let name = block.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let input = block.get("input").cloned().unwrap_or_else(|| json!({}));
                    tool_calls.push(json!({
                        "id": id,
                        "type": "function",
                        "function": {
                            "name": name,
                            "arguments": serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_string()),
                        }
                    }));
                }
                _ => {}
            }
        }
    }

    let mut message = json!({ "role": "assistant" });
    if !text_content.is_empty() {
        message["content"] = json!(text_content);
    }
    if !thinking_content.is_empty() {
        message["reasoning_content"] = json!(thinking_content);
    }
    if !tool_calls.is_empty() {
        message["tool_calls"] = Value::Array(tool_calls);
    }
    if message.get("content").is_none() && message.get("tool_calls").is_none() {
        message["content"] = json!("");
    }

    let finish_reason = match body.get("stop_reason").and_then(|v| v.as_str()) {
        Some("end_turn") | None => "stop",
        Some("tool_use") => "tool_calls",
        Some("max_tokens") => "length",
        Some(other) => other,
    };

    let id = body
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| format!("chatcmpl-{s}"))
        .unwrap_or_else(|| format!("chatcmpl-{}", now_unix_secs()));

    let mut out = json!({
        "id": id,
        "object": "chat.completion",
        "created": now_unix_secs(),
        "model": body.get("model").and_then(|v| v.as_str()).unwrap_or("claude"),
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": finish_reason,
        }]
    });

    if let Some(u) = body.get("usage") {
        let input_tokens = u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
        let output_tokens = u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
        out["usage"] = json!({
            "prompt_tokens": input_tokens,
            "completion_tokens": output_tokens,
            "total_tokens": input_tokens + output_tokens,
        });
    }
    out
}

fn openai_body_to_claude(body: &Value) -> Value {
    if body.get("type").and_then(|v| v.as_str()) == Some("message") {
        return body.clone();
    }
    let choice = body
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first());
    let message = choice.and_then(|c| c.get("message"));
    let mut content = Vec::new();

    if let Some(msg) = message {
        if let Some(reasoning) = msg
            .get("reasoning_content")
            .or_else(|| msg.get("reasoning"))
            .and_then(|v| v.as_str())
            && !reasoning.is_empty()
        {
            content.push(json!({ "type": "thinking", "thinking": reasoning }));
        }
        if let Some(text) = msg.get("content").and_then(|v| v.as_str())
            && !text.is_empty()
        {
            content.push(json!({ "type": "text", "text": text }));
        }
        if let Some(tcs) = msg.get("tool_calls").and_then(|v| v.as_array()) {
            for tc in tcs {
                let id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let func = tc.get("function");
                let name = func
                    .and_then(|f| f.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let args_val = func.and_then(|f| f.get("arguments"));
                let input = match args_val {
                    Some(Value::String(s)) => {
                        serde_json::from_str::<Value>(s).unwrap_or_else(|_| json!({}))
                    }
                    Some(v) if v.is_object() => v.clone(),
                    _ => json!({}),
                };
                content.push(json!({
                    "type": "tool_use",
                    "id": id,
                    "name": name,
                    "input": input,
                }));
            }
        }
    }

    let stop_reason = match choice
        .and_then(|c| c.get("finish_reason"))
        .and_then(|v| v.as_str())
    {
        Some("tool_calls") => "tool_use",
        Some("length") => "max_tokens",
        _ => "end_turn",
    };

    let usage = body.get("usage");
    json!({
        "id": body.get("id").and_then(|v| v.as_str()).unwrap_or("msg_0"),
        "type": "message",
        "role": "assistant",
        "model": body.get("model").and_then(|v| v.as_str()).unwrap_or("unknown"),
        "content": content,
        "stop_reason": stop_reason,
        "stop_sequence": null,
        "usage": {
            "input_tokens": usage.and_then(|u| u.get("prompt_tokens")).and_then(|v| v.as_u64()).unwrap_or(0),
            "output_tokens": usage.and_then(|u| u.get("completion_tokens")).and_then(|v| v.as_u64()).unwrap_or(0),
        }
    })
}

fn gemini_body_to_openai(body: &Value) -> Value {
    let response = body.get("response").unwrap_or(body);
    let Some(candidate) = response
        .get("candidates")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
    else {
        return body.clone();
    };

    let mut text_content = String::new();
    let mut reasoning_content = String::new();
    let mut tool_calls = Vec::new();

    if let Some(parts) = candidate
        .get("content")
        .and_then(|c| c.get("parts"))
        .and_then(|p| p.as_array())
    {
        for (i, part) in parts.iter().enumerate() {
            if part.get("thought").and_then(|v| v.as_bool()) == Some(true)
                && let Some(t) = part.get("text").and_then(|v| v.as_str())
            {
                reasoning_content.push_str(t);
            } else if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                text_content.push_str(t);
            }
            if let Some(fc) = part.get("functionCall") {
                let name = fc.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let args = fc.get("args").cloned().unwrap_or_else(|| json!({}));
                tool_calls.push(json!({
                    "id": format!("call_{name}_{i}"),
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": serde_json::to_string(&args).unwrap_or_else(|_| "{}".to_string()),
                    }
                }));
            }
        }
    }

    let mut message = json!({ "role": "assistant" });
    if !text_content.is_empty() {
        message["content"] = json!(text_content);
    }
    if !reasoning_content.is_empty() {
        message["reasoning_content"] = json!(reasoning_content);
    }
    if !tool_calls.is_empty() {
        message["tool_calls"] = Value::Array(tool_calls);
    }
    if message.get("content").is_none() && message.get("tool_calls").is_none() {
        message["content"] = json!("");
    }

    let mut finish_reason = candidate
        .get("finishReason")
        .and_then(|v| v.as_str())
        .unwrap_or("stop")
        .to_lowercase();
    if finish_reason == "stop" && message.get("tool_calls").is_some() {
        finish_reason = "tool_calls".to_string();
    }

    let mut out = json!({
        "id": format!("chatcmpl-{}", response.get("responseId").and_then(|v| v.as_str()).unwrap_or("0")),
        "object": "chat.completion",
        "created": now_unix_secs(),
        "model": response.get("modelVersion").and_then(|v| v.as_str()).unwrap_or("gemini"),
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": finish_reason,
        }]
    });

    if let Some(u) = response
        .get("usageMetadata")
        .or_else(|| body.get("usageMetadata"))
    {
        let prompt = u.get("promptTokenCount").and_then(|v| v.as_u64()).unwrap_or(0)
            + u.get("thoughtsTokenCount").and_then(|v| v.as_u64()).unwrap_or(0);
        let comp = u
            .get("candidatesTokenCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let total = u
            .get("totalTokenCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(prompt + comp);
        out["usage"] = json!({
            "prompt_tokens": prompt,
            "completion_tokens": comp,
            "total_tokens": total,
        });
    }
    out
}

fn responses_body_to_openai(body: &Value) -> Value {
    if body.get("choices").is_some() {
        return body.clone();
    }
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    if let Some(output) = body.get("output").and_then(|v| v.as_array()) {
        for item in output {
            match item.get("type").and_then(|v| v.as_str()) {
                Some("message") => {
                    if let Some(parts) = item.get("content").and_then(|v| v.as_array()) {
                        for p in parts {
                            if let Some(t) = p.get("text").and_then(|v| v.as_str()) {
                                text.push_str(t);
                            }
                        }
                    }
                }
                Some("function_call") => {
                    let call_id = item.get("call_id").and_then(|v| v.as_str()).unwrap_or("");
                    let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let args = item
                        .get("arguments")
                        .and_then(|v| v.as_str())
                        .unwrap_or("{}");
                    tool_calls.push(json!({
                        "id": call_id,
                        "type": "function",
                        "function": { "name": name, "arguments": args }
                    }));
                }
                _ => {}
            }
        }
    }
    let mut message = json!({ "role": "assistant", "content": text });
    if !tool_calls.is_empty() {
        message["tool_calls"] = Value::Array(tool_calls);
    }
    json!({
        "id": body.get("id").and_then(|v| v.as_str()).unwrap_or("chatcmpl-0"),
        "object": "chat.completion",
        "created": body.get("created_at").and_then(|v| v.as_u64()).unwrap_or_else(now_unix_secs),
        "model": body.get("model").and_then(|v| v.as_str()).unwrap_or("unknown"),
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": if message.get("tool_calls").is_some() { "tool_calls" } else { "stop" }
        }]
    })
}

fn openai_body_to_responses(body: &Value) -> Value {
    let Some(choice) = body
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
    else {
        return body.clone();
    };
    let msg = choice.get("message").cloned().unwrap_or_else(|| json!({}));
    let mut output = Vec::new();
    if let Some(text) = msg.get("content").and_then(|v| v.as_str())
        && !text.is_empty()
    {
        output.push(json!({
            "type": "message",
            "role": "assistant",
            "content": [{ "type": "output_text", "text": text, "annotations": [] }]
        }));
    }
    if let Some(tcs) = msg.get("tool_calls").and_then(|v| v.as_array()) {
        for tc in tcs {
            let id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let fn_obj = tc.get("function");
            let name = fn_obj
                .and_then(|f| f.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let args = fn_obj
                .and_then(|f| f.get("arguments"))
                .and_then(|v| v.as_str())
                .unwrap_or("{}");
            output.push(json!({
                "type": "function_call",
                "id": format!("fc_{id}"),
                "call_id": id,
                "name": name,
                "arguments": args,
            }));
        }
    }
    json!({
        "id": format!("resp_{}", body.get("id").and_then(|v| v.as_str()).unwrap_or("0")),
        "object": "response",
        "created_at": body.get("created").and_then(|v| v.as_u64()).unwrap_or_else(now_unix_secs),
        "model": body.get("model").and_then(|v| v.as_str()).unwrap_or("unknown"),
        "status": "completed",
        "output": output,
    })
}

async fn read_body_bytes(body: ExecutorBody) -> Result<Vec<u8>, String> {
    match body {
        ExecutorBody::Json(v) => serde_json::to_vec(&v).map_err(|e| e.to_string()),
        ExecutorBody::Bytes(b) => Ok(b),
        ExecutorBody::Stream(mut rx) => {
            let mut out = Vec::new();
            while let Some(chunk) = rx.recv().await {
                out.extend_from_slice(&chunk?);
            }
            Ok(out)
        }
    }
}

/// Forwards upstream chunks unchanged and reports the last usage found in the SSE JSON lines
/// once the stream ends. Lines are reassembled across chunk boundaries.
fn tap_usage(
    mut upstream: mpsc::Receiver<Result<Vec<u8>, String>>,
    hook: UsageHook,
) -> mpsc::Receiver<Result<Vec<u8>, String>> {
    let (tx, rx) = mpsc::channel(64);
    tokio::spawn(async move {
        let mut buf: Vec<u8> = Vec::new();
        let mut usage: Option<(u64, u64)> = None;
        let scan = |line: &[u8], usage: &mut Option<(u64, u64)>| {
            if let Some(SseLine::Json(v)) = parse_sse_line(&String::from_utf8_lossy(line))
                && let Some(found) = usage_from_body(&v)
            {
                *usage = Some(found);
            }
        };
        while let Some(chunk) = upstream.recv().await {
            if let Ok(bytes) = &chunk {
                buf.extend_from_slice(bytes);
                while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                    let line: Vec<u8> = buf.drain(..=pos).collect();
                    scan(&line, &mut usage);
                }
            }
            if tx.send(chunk.clone()).await.is_err() {
                break;
            }
            if chunk.is_err() {
                break;
            }
        }
        if !buf.is_empty() {
            scan(&buf, &mut usage);
        }
        let (p, c) = usage.unwrap_or((0, 0));
        hook(p, c).await;
    });
    rx
}

fn parse_sse_text_to_last_json(text: &str) -> Option<Value> {
    let mut last = None;
    for line in text.lines() {
        if let Some(SseLine::Json(v)) = parse_sse_line(line) {
            last = Some(v);
        }
    }
    last
}

fn build_json_response(status: u16, headers: &[(String, String)], body: &Value) -> Response {
    let bytes = serde_json::to_vec(body).unwrap_or_else(|_| b"{}".to_vec());
    let mut builder = Response::builder()
        .status(StatusCode::from_u16(status).unwrap_or(StatusCode::OK))
        .header("content-type", "application/json")
        .header("access-control-allow-origin", "*");
    for (k, v) in headers {
        if !k.eq_ignore_ascii_case("content-type")
            && !k.eq_ignore_ascii_case("content-length")
            && let (Ok(name), Ok(val)) = (
                HeaderName::from_bytes(k.as_bytes()),
                HeaderValue::from_str(v),
            )
        {
            builder = builder.header(name, val);
        }
    }
    builder
        .body(Body::from(bytes))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

fn build_sse_response(headers: &[(String, String)], body: Body) -> Response {
    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream; charset=utf-8")
        .header("cache-control", "no-cache")
        .header("connection", "keep-alive")
        .header("access-control-allow-origin", "*");
    for (k, v) in headers {
        if !k.eq_ignore_ascii_case("content-type")
            && !k.eq_ignore_ascii_case("content-length")
            && let (Ok(name), Ok(val)) = (
                HeaderName::from_bytes(k.as_bytes()),
                HeaderValue::from_str(v),
            )
        {
            builder = builder.header(name, val);
        }
    }
    builder
        .body(body)
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

async fn try_refresh_and_retry(
    executor: &dyn Executor,
    exec_req: &ExecutorRequest,
    credentials: &mut Credentials,
    hooks: &ChatCoreHooks,
) -> Option<ExecutorResponse> {
    let mut refreshed = None;
    for _ in 0..3 {
        match executor.refresh_credentials(credentials).await {
            Ok(Some(new_creds))
                if new_creds.access_token.is_some() || new_creds.copilot_token.is_some() =>
            {
                if let Some(at) = &new_creds.access_token {
                    credentials.access_token = Some(at.clone());
                }
                if let Some(rt) = &new_creds.refresh_token {
                    credentials.refresh_token = Some(rt.clone());
                }
                if let Some(exp) = &new_creds.expires_at {
                    credentials.expires_at = Some(exp.clone());
                }
                refreshed = Some(new_creds);
                break;
            }
            Ok(_) => continue,
            Err(_) => continue,
        }
    }

    let new_creds = refreshed?;
    if let Some(hook) = &hooks.on_credentials_refreshed {
        let _ = hook(new_creds).await;
    }

    let mut retry_req = exec_req.clone();
    retry_req.credentials = credentials.clone();
    match executor.execute(retry_req).await {
        Ok(resp) if (200..300).contains(&resp.status) => Some(resp),
        _ => None,
    }
}

/// Core chat handler: source-format detection, request translation, executor dispatch,
/// 401/403 token refresh + retry, and streaming/non-streaming response translation.
pub async fn handle_chat_core(
    mut options: ChatCoreOptions,
    executor: &dyn Executor,
    hooks: &ChatCoreHooks,
) -> ChatCoreResult {
    let source_format = options
        .source_format_override
        .clone()
        .unwrap_or_else(|| {
            detect_source_format(options.endpoint_path.as_deref(), &options.body).to_string()
        });

    let target_format = get_target_format(
        &options.provider,
        &options.model,
        &source_format,
        Some(&options.credentials),
    );

    let body_stream_flag = options.body.get("stream").and_then(|v| v.as_bool());
    let mut stream = body_stream_flag != Some(false);
    if let Some(accept) = &options.accept_header {
        let wants_json = accept.contains("application/json");
        let wants_sse = accept.contains("text/event-stream");
        if wants_json && !wants_sse && body_stream_flag != Some(true) {
            stream = false;
        }
    }

    let ts_cfg = options.token_saver.clone().unwrap_or_default();
    let rtk_active = ts_cfg.token_saver_enabled && ts_cfg.rtk_enabled;

    // Cursor translator rewrites tool_result shapes, so RTK runs pre-translate for cursor.
    let pre_translate_rtk = if options.provider == "cursor" {
        crate::services::rtk_index::compress_messages(&mut options.body, rtk_active)
    } else {
        None
    };

    let creds_val = options.credentials.to_translator_value();
    let upstream_model = strip_thinking_suffix(&options.model).to_string();
    let mut translated_body = translate_request(
        &source_format,
        &target_format,
        &upstream_model,
        &options.body,
        stream,
        Some(&creds_val),
        Some(&options.provider),
    );
    if let Some(obj) = translated_body.as_object_mut() {
        obj.insert("model".to_string(), json!(upstream_model));
    }
    strip_continuity_fields(&mut translated_body);

    let rtk_stats = pre_translate_rtk.or_else(|| {
        crate::services::rtk_index::compress_messages(&mut translated_body, rtk_active)
    });
    if let Some(stats) = &rtk_stats
        && let Some(line) = crate::services::rtk_index::format_rtk_log(stats)
    {
        tracing::debug!("{line}");
    }

    let effective_caveman = ts_cfg.brief_level.as_deref().or_else(|| {
        if ts_cfg.caveman_enabled {
            ts_cfg.caveman_level.as_deref()
        } else {
            None
        }
    });
    if ts_cfg.token_saver_enabled
        && let Some(level) = effective_caveman
    {
        crate::services::rtk_caveman::inject_caveman(&mut translated_body, &target_format, level);
    }
    if ts_cfg.token_saver_enabled
        && ts_cfg.ponytail_enabled
        && let Some(level) = ts_cfg.ponytail_level.as_deref()
    {
        crate::services::rtk_caveman::inject_ponytail(&mut translated_body, &target_format, level);
    }

    let exec_req = ExecutorRequest {
        provider: options.provider.clone(),
        model: options.model.clone(),
        body: translated_body,
        stream,
        credentials: options.credentials.clone(),
        provider_session_id: options.provider_session_id.clone(),
        client_tool: options.client_tool.clone(),
    };

    let first = executor.execute(exec_req.clone());
    let first = match options.first_response_timeout {
        Some(limit) => match tokio::time::timeout(limit, first).await {
            Ok(r) => r,
            Err(_) => {
                return ChatCoreResult::Error {
                    status: 504,
                    message: format!("No first response within {}s", limit.as_secs()),
                    headers: vec![],
                    retry_after: None,
                };
            }
        },
        None => first.await,
    };
    let mut provider_resp = match first {
        Ok(r) => r,
        Err(err) => {
            return ChatCoreResult::Error {
                status: 502,
                message: err,
                headers: vec![],
                retry_after: None,
            };
        }
    };

    if !executor.no_auth() && (provider_resp.status == 401 || provider_resp.status == 403)
        && let Some(retried) =
            try_refresh_and_retry(executor, &exec_req, &mut options.credentials, hooks).await
    {
        provider_resp = retried;
    }

    let resp_format = provider_resp
        .response_format
        .clone()
        .unwrap_or_else(|| target_format.clone());

    if !(200..300).contains(&provider_resp.status) {
        let status = provider_resp.status;
        let headers = provider_resp.headers;
        let retry_after = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("retry-after"))
            .and_then(|(_, v)| v.trim().parse::<u64>().ok());
        let bytes = read_body_bytes(provider_resp.body).await.unwrap_or_default();
        let message = serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|v| {
                v.get("error")
                    .and_then(|e| e.get("message").or(Some(e)))
                    .and_then(|m| m.as_str().map(str::to_string))
                    .or_else(|| v.get("message").and_then(|m| m.as_str()).map(str::to_string))
            })
            .unwrap_or_else(|| {
                let txt = String::from_utf8_lossy(&bytes).trim().to_string();
                if txt.is_empty() {
                    format!("Upstream HTTP {status}")
                } else {
                    txt
                }
            });
        return ChatCoreResult::Error {
            status,
            message,
            headers,
            retry_after,
        };
    }

    if let Some(hook) = &hooks.on_request_success {
        hook().await;
    }

    if !stream {
        let raw_json = match provider_resp.body {
            ExecutorBody::Json(v) => v,
            ExecutorBody::Bytes(b) => {
                if let Ok(v) = serde_json::from_slice::<Value>(&b) {
                    v
                } else if let Some(v) = parse_sse_text_to_last_json(&String::from_utf8_lossy(&b)) {
                    v
                } else {
                    return ChatCoreResult::Error {
                        status: 502,
                        message: "Invalid JSON response from upstream".to_string(),
                        headers: provider_resp.headers,
                        retry_after: None,
                    };
                }
            }
            ExecutorBody::Stream(rx) => {
                let b = read_body_bytes(ExecutorBody::Stream(rx))
                    .await
                    .unwrap_or_default();
                if let Ok(v) = serde_json::from_slice::<Value>(&b) {
                    v
                } else if let Some(v) = parse_sse_text_to_last_json(&String::from_utf8_lossy(&b)) {
                    v
                } else {
                    return ChatCoreResult::Error {
                        status: 502,
                        message: "Invalid streaming body for non-stream request".to_string(),
                        headers: provider_resp.headers,
                        retry_after: None,
                    };
                }
            }
        };

        let usage = hooks.on_usage.as_ref().map(|_| usage_from_body(&raw_json).unwrap_or((0, 0)));
        let mut translated = if needs_translation(&resp_format, &source_format) {
            translate_non_streaming_response(&raw_json, &resp_format, &source_format)
        } else {
            raw_json
        };

        if source_format == FORMAT_OPENAI
            && let Some(obj) = translated.as_object_mut()
        {
            obj.entry("object").or_insert_with(|| json!("chat.completion"));
            obj.entry("created")
                .or_insert_with(|| json!(now_unix_secs()));
        }

        if let (Some(hook), Some((p, c))) = (&hooks.on_usage, usage) {
            hook(p, c).await;
        }
        return ChatCoreResult::Success(build_json_response(
            200,
            &provider_resp.headers,
            &translated,
        ));
    }

    // Streaming path
    let rx = match provider_resp.body {
        ExecutorBody::Stream(rx) => rx,
        ExecutorBody::Bytes(b) => {
            let (tx, rx) = mpsc::channel(1);
            let _ = tx.send(Ok(b)).await;
            rx
        }
        ExecutorBody::Json(v) => {
            let (tx, rx) = mpsc::channel(1);
            let line = format!("data: {}\n\n", serde_json::to_string(&v).unwrap_or_default());
            let _ = tx.send(Ok(line.into_bytes())).await;
            rx
        }
    };

    let rx = match &hooks.on_usage {
        Some(hook) => tap_usage(rx, hook.clone()),
        None => rx,
    };
    let body = translate_sse_to_body(rx, resp_format, source_format);
    ChatCoreResult::Success(build_sse_response(&provider_resp.headers, body))
}
