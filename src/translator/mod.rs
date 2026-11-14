pub mod antigravity;
pub mod claude;
pub mod commandcode;
pub mod concerns;
pub mod cursor;
pub mod deepseek_web_tools;
pub mod formats;
pub mod gemini;
pub mod kiro;
pub mod modality;
pub mod ollama;
pub mod openai_responses;
pub mod schema;
pub mod thinking;
pub mod vertex;
pub mod web_tools;

use serde_json::Value;
use std::collections::HashMap;
use std::sync::OnceLock;

use crate::translator::antigravity::antigravity_to_openai_request;
use crate::translator::claude::{
    claude_to_openai_request, claude_to_openai_response, openai_to_claude_request,
    openai_to_claude_request_for_antigravity, openai_to_claude_response,
};
use crate::translator::commandcode::{commandcode_to_openai_response, openai_to_commandcode_request};
use crate::translator::concerns::{filter_to_openai_format, lift_reasoning_aliases};
use crate::translator::cursor::{cursor_to_openai_response, openai_to_cursor_request};
use crate::translator::formats::{
    FORMAT_ANTIGRAVITY, FORMAT_CLAUDE, FORMAT_COMMANDCODE, FORMAT_CURSOR, FORMAT_GEMINI,
    FORMAT_GEMINI_CLI, FORMAT_KIRO, FORMAT_OLLAMA, FORMAT_OPENAI, FORMAT_OPENAI_RESPONSE,
    FORMAT_OPENAI_RESPONSES, FORMAT_VERTEX,
};
use crate::translator::gemini::{
    gemini_to_openai_request, gemini_to_openai_response, openai_to_antigravity_request,
    openai_to_antigravity_response, openai_to_gemini_cli_request, openai_to_gemini_request,
};
use crate::translator::kiro::{
    claude_to_kiro_request, kiro_to_claude_response, kiro_to_openai_response,
    openai_to_kiro_request,
};
use crate::translator::ollama::{ollama_to_openai_response, openai_to_ollama_request};
use crate::translator::openai_responses::{
    openai_responses_to_openai_request, openai_responses_to_openai_response,
    openai_to_openai_responses_request, openai_to_openai_responses_response,
};
use crate::translator::vertex::openai_to_vertex_request;

pub type RequestFn = fn(&str, &Value, bool, Option<&Value>) -> Value;
pub type ResponseFn = fn(Option<&Value>, &mut TranslateState) -> Vec<Value>;

#[derive(Debug, Clone, Default)]
pub struct ClaudeBlockState {
    pub tool_call_index: Option<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct ResponsesToolCallState {
    pub item_id: String,
    pub call_id: String,
    pub name: String,
    pub arguments: String,
    pub output_index: usize,
}

#[derive(Debug, Clone, Default)]
pub struct TranslateState {
    pub source_format: String,
    pub message_id: Option<String>,
    pub model: Option<String>,
    pub created: Option<u64>,
    pub started: bool,
    pub finished: bool,
    pub finish_reason: Option<String>,
    pub finish_reason_sent: bool,
    pub usage: Option<Value>,
    pub has_tool_calls: bool,
    pub tool_call_index: usize,
    pub current_tool_call_id: Option<String>,
    pub current_tool_call_name: Option<String>,
    pub tool_calls_map: HashMap<usize, Value>,
    pub claude_blocks: HashMap<usize, ClaudeBlockState>,
    pub claude_next_block_index: usize,
    pub text_block_started: bool,
    pub text_block_closed: bool,
    pub text_block_index: Option<usize>,
    pub thinking_block_started: bool,
    pub thinking_block_closed: bool,
    pub thinking_block_index: Option<usize>,
    pub responses_seq: usize,
    pub responses_response_id: Option<String>,
    pub responses_started: bool,
    pub responses_text_started: bool,
    pub responses_text_done: bool,
    pub responses_text_buffer: String,
    pub responses_item_id: Option<String>,
    pub responses_output_index: usize,
    pub responses_reasoning_buffer: String,
    pub responses_tool_calls: HashMap<usize, ResponsesToolCallState>,
    pub responses_completed_sent: bool,
    pub responses_usage: Option<Value>,
}

pub fn init_state(source_format: &str) -> TranslateState {
    TranslateState {
        source_format: source_format.to_string(),
        ..Default::default()
    }
}

pub fn normalize_format(fmt: &str) -> &str {
    if fmt == FORMAT_OPENAI_RESPONSE {
        FORMAT_OPENAI_RESPONSES
    } else {
        fmt
    }
}

pub fn needs_translation(source_format: &str, target_format: &str) -> bool {
    normalize_format(source_format) != normalize_format(target_format)
}

fn route_key(source: &str, target: &str) -> String {
    format!("{}:{}", normalize_format(source), normalize_format(target))
}

struct Registry {
    requests: HashMap<String, RequestFn>,
    responses: HashMap<String, ResponseFn>,
}

fn get_registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut requests: HashMap<String, RequestFn> = HashMap::new();
        let mut responses: HashMap<String, ResponseFn> = HashMap::new();

        // Direct routes keyed "source:target"
        requests.insert(route_key(FORMAT_CLAUDE, FORMAT_OPENAI), claude_to_openai_request);
        requests.insert(route_key(FORMAT_OPENAI, FORMAT_CLAUDE), openai_to_claude_request);

        requests.insert(route_key(FORMAT_GEMINI, FORMAT_OPENAI), gemini_to_openai_request);
        requests.insert(route_key(FORMAT_GEMINI_CLI, FORMAT_OPENAI), gemini_to_openai_request);
        requests.insert(route_key(FORMAT_VERTEX, FORMAT_OPENAI), gemini_to_openai_request);
        requests.insert(route_key(FORMAT_ANTIGRAVITY, FORMAT_OPENAI), antigravity_to_openai_request);

        requests.insert(route_key(FORMAT_OPENAI, FORMAT_GEMINI), openai_to_gemini_request);
        requests.insert(route_key(FORMAT_OPENAI, FORMAT_GEMINI_CLI), openai_to_gemini_cli_request);
        requests.insert(route_key(FORMAT_OPENAI, FORMAT_VERTEX), openai_to_vertex_request);
        requests.insert(route_key(FORMAT_OPENAI, FORMAT_ANTIGRAVITY), openai_to_antigravity_request);

        requests.insert(route_key(FORMAT_CLAUDE, FORMAT_KIRO), claude_to_kiro_request);
        requests.insert(route_key(FORMAT_OPENAI, FORMAT_KIRO), openai_to_kiro_request);
        requests.insert(route_key(FORMAT_OPENAI, FORMAT_CURSOR), openai_to_cursor_request);
        requests.insert(route_key(FORMAT_OPENAI, FORMAT_OLLAMA), openai_to_ollama_request);
        requests.insert(route_key(FORMAT_OPENAI, FORMAT_COMMANDCODE), openai_to_commandcode_request);

        requests.insert(
            route_key(FORMAT_OPENAI_RESPONSES, FORMAT_OPENAI),
            openai_responses_to_openai_request,
        );
        requests.insert(
            route_key(FORMAT_OPENAI, FORMAT_OPENAI_RESPONSES),
            openai_to_openai_responses_request,
        );

        // Response translators keyed "from_format:to_format"
        responses.insert(route_key(FORMAT_CLAUDE, FORMAT_OPENAI), claude_to_openai_response);
        responses.insert(route_key(FORMAT_OPENAI, FORMAT_CLAUDE), openai_to_claude_response);

        responses.insert(route_key(FORMAT_GEMINI, FORMAT_OPENAI), gemini_to_openai_response);
        responses.insert(route_key(FORMAT_GEMINI_CLI, FORMAT_OPENAI), gemini_to_openai_response);
        responses.insert(route_key(FORMAT_VERTEX, FORMAT_OPENAI), gemini_to_openai_response);
        responses.insert(route_key(FORMAT_ANTIGRAVITY, FORMAT_OPENAI), gemini_to_openai_response);

        responses.insert(route_key(FORMAT_OPENAI, FORMAT_GEMINI), openai_to_antigravity_response);
        responses.insert(route_key(FORMAT_OPENAI, FORMAT_GEMINI_CLI), openai_to_antigravity_response);
        responses.insert(route_key(FORMAT_OPENAI, FORMAT_VERTEX), openai_to_antigravity_response);
        responses.insert(route_key(FORMAT_OPENAI, FORMAT_ANTIGRAVITY), openai_to_antigravity_response);

        responses.insert(
            route_key(FORMAT_OPENAI_RESPONSES, FORMAT_OPENAI),
            openai_responses_to_openai_response,
        );
        responses.insert(
            route_key(FORMAT_OPENAI, FORMAT_OPENAI_RESPONSES),
            openai_to_openai_responses_response,
        );

        responses.insert(route_key(FORMAT_KIRO, FORMAT_CLAUDE), kiro_to_claude_response);
        responses.insert(route_key(FORMAT_KIRO, FORMAT_OPENAI), kiro_to_openai_response);
        responses.insert(route_key(FORMAT_CURSOR, FORMAT_OPENAI), cursor_to_openai_response);
        responses.insert(route_key(FORMAT_OLLAMA, FORMAT_OPENAI), ollama_to_openai_response);
        responses.insert(route_key(FORMAT_COMMANDCODE, FORMAT_OPENAI), commandcode_to_openai_response);

        Registry { requests, responses }
    })
}

fn identity_request(
    _model: &str,
    body: &Value,
    _stream: bool,
    _credentials: Option<&Value>,
) -> Value {
    body.clone()
}

pub fn translate_request(
    source_format: &str,
    target_format: &str,
    model: &str,
    body: &Value,
    stream: bool,
    credentials: Option<&Value>,
    provider: Option<&str>,
) -> Value {
    let src = normalize_format(source_format);
    let tgt = normalize_format(target_format);

    // Special case: Claude target under Antigravity provider needs AG thinking signature
    let is_ag_provider = provider == Some(FORMAT_ANTIGRAVITY);

    if src == tgt {
        let mut result = body.clone();
        if tgt == FORMAT_CLAUDE {
            if let Some(messages) = result.get_mut("messages").and_then(Value::as_array_mut) {
                for message in messages {
                    if message["role"] == "assistant" {
                        if let Some(blocks) = message.get_mut("content").and_then(Value::as_array_mut) {
                            blocks.retain(|block| {
                                !matches!(block["type"].as_str(), Some("thinking" | "redacted_thinking"))
                                    || block["signature"].as_str().is_some_and(crate::services::claude_signature::is_valid_claude_signature)
                            });
                        }
                    }
                }
            }
        }
        return result;
    }

    let reg = get_registry();
    let direct_key = route_key(src, tgt);
    if let Some(direct_fn) = reg.requests.get(&direct_key) {
        if tgt == FORMAT_CLAUDE && is_ag_provider {
            return openai_to_claude_request_for_antigravity(model, body, stream, credentials);
        }
        return direct_fn(model, body, stream, credentials);
    }

    // Pivot through OpenAI: source -> openai -> target
    let to_openai = reg
        .requests
        .get(&route_key(src, FORMAT_OPENAI))
        .copied()
        .unwrap_or(identity_request);
    let mut openai_body = to_openai(model, body, stream, credentials);

    if src != FORMAT_OPENAI {
        openai_body = filter_to_openai_format(&openai_body);
    }

    if tgt == FORMAT_OPENAI {
        return openai_body;
    }

    if tgt == FORMAT_CLAUDE && is_ag_provider {
        return openai_to_claude_request_for_antigravity(model, &openai_body, stream, credentials);
    }

    let from_openai = reg
        .requests
        .get(&route_key(FORMAT_OPENAI, tgt))
        .copied()
        .unwrap_or(identity_request);

    from_openai(model, &openai_body, stream, credentials)
}

pub fn translate_response(
    target_format: &str,
    source_format: &str,
    chunk: Option<&Value>,
    state: &mut TranslateState,
) -> Vec<Value> {
    let tgt = normalize_format(target_format);
    let src = normalize_format(source_format);

    if tgt == src {
        if let Some(c) = chunk {
            let mut cloned = c.clone();
            if tgt == FORMAT_OPENAI {
                if let Some(delta) = cloned
                    .get_mut("choices")
                    .and_then(|v| v.as_array_mut())
                    .and_then(|a| a.first_mut())
                    .and_then(|c0| c0.get_mut("delta"))
                {
                    lift_reasoning_aliases(delta);
                }
            }
            return vec![cloned];
        }
        return vec![];
    }

    let reg = get_registry();
    let direct_key = route_key(tgt, src);
    if let Some(direct_fn) = reg.responses.get(&direct_key) {
        return direct_fn(chunk, state);
    }

    // Pivot through OpenAI: target_format -> openai -> source_format
    let openai_chunks = if tgt == FORMAT_OPENAI {
        match chunk {
            Some(c) => vec![c.clone()],
            None => vec![],
        }
    } else if let Some(to_openai) = reg.responses.get(&route_key(tgt, FORMAT_OPENAI)) {
        to_openai(chunk, state)
    } else {
        vec![]
    };

    if src == FORMAT_OPENAI {
        return openai_chunks;
    }

    let Some(from_openai) = reg.responses.get(&route_key(FORMAT_OPENAI, src)) else {
        return openai_chunks;
    };

    if chunk.is_none() && openai_chunks.is_empty() {
        return from_openai(None, state);
    }

    let mut out = Vec::new();
    for oc in &openai_chunks {
        out.extend(from_openai(Some(oc), state));
    }
    out
}

#[cfg(test)]
mod tests_parity;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashSet;

    fn run_stream(target_fmt: &str, source_fmt: &str, events: Vec<Value>) -> Vec<Value> {
        let mut state = init_state(source_fmt);
        let mut all = Vec::new();
        for ev in events {
            all.extend(translate_response(target_fmt, source_fmt, Some(&ev), &mut state));
        }
        all.extend(translate_response(target_fmt, source_fmt, None, &mut state));
        all
    }

    #[test]
    fn roundtrip_claude_to_openai_preserves_core_fields() {
        let body = json!({
            "system": "sys",
            "max_tokens": 100,
            "messages": [
                { "role": "user", "content": "question" },
                { "role": "assistant", "content": [{ "type": "tool_use", "id": "call_1", "name": "search", "input": { "q": "x" } }] },
                { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": "call_1", "content": "result" }] }
            ]
        });
        let out = translate_request(FORMAT_CLAUDE, FORMAT_OPENAI, "m", &body, true, None, None);
        let msgs = out["messages"].as_array().unwrap();
        assert!(msgs.iter().any(|m| m["role"] == "system" && m["content"] == "sys"));
        let asst = msgs.iter().find(|m| m.get("tool_calls").is_some()).unwrap();
        assert_eq!(asst["tool_calls"][0]["id"], "call_1");
        let args_str = asst["tool_calls"][0]["function"]["arguments"].as_str().unwrap();
        assert!(serde_json::from_str::<Value>(args_str).is_ok());
        let tool = msgs.iter().find(|m| m["role"] == "tool").unwrap();
        assert_eq!(tool["tool_call_id"], "call_1");
        assert!(tool["content"].as_str().unwrap().contains("result"));
    }

    #[test]
    fn roundtrip_parallel_tool_calls_keep_distinct_ids() {
        let body = json!({
            "messages": [
                { "role": "assistant", "content": [
                    { "type": "tool_use", "id": "call_a", "name": "f1", "input": {} },
                    { "type": "tool_use", "id": "call_b", "name": "f2", "input": {} }
                ] },
                { "role": "user", "content": [
                    { "type": "tool_result", "tool_use_id": "call_a", "content": "ra" },
                    { "type": "tool_result", "tool_use_id": "call_b", "content": "rb" }
                ] }
            ]
        });
        let out = translate_request(FORMAT_CLAUDE, FORMAT_OPENAI, "m", &body, true, None, None);
        let msgs = out["messages"].as_array().unwrap();
        let asst = msgs.iter().find(|m| m.get("tool_calls").is_some()).unwrap();
        let ids: HashSet<&str> = asst["tool_calls"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tc| tc["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids.len(), 2);
        let tool_count = msgs.iter().filter(|m| m["role"] == "tool").count();
        assert_eq!(tool_count, 2);
    }

    #[test]
    fn openai_to_claude_request_adaptive_thinking_and_tools() {
        let body = json!({
            "messages": [
                { "role": "system", "content": "You are helpful." },
                { "role": "user", "content": [
                    { "type": "text", "text": "What's in this image?" },
                    { "type": "image_url", "image_url": { "url": "data:image/png;base64,IMGDATA" } }
                ] },
                { "role": "assistant", "content": "", "tool_calls": [
                    { "id": "call_1", "type": "function", "function": { "name": "get_weather", "arguments": "{\"city\":\"NYC\"}" } }
                ] },
                { "role": "tool", "tool_call_id": "call_1", "content": "sunny" }
            ],
            "tools": [
                { "type": "function", "function": { "name": "get_weather", "description": "Get weather", "parameters": { "type": "object", "properties": { "city": { "type": "string" } }, "required": ["city"] } } }
            ],
            "reasoning_effort": "high"
        });
        let out = translate_request(FORMAT_OPENAI, FORMAT_CLAUDE, "claude-opus-4-6", &body, true, None, Some("anthropic"));
        assert_eq!(out["thinking"]["type"], "adaptive");
        assert_eq!(out["output_config"]["effort"], "high");
        assert_eq!(out["tools"][0]["name"], "get_weather");
        assert_eq!(out["system"][0]["text"], "You are helpful.");
        assert_eq!(out["messages"][0]["content"][1]["type"], "image");
        assert_eq!(out["messages"][1]["content"][0]["type"], "tool_use");
        assert_eq!(out["messages"][2]["content"][0]["type"], "tool_result");
    }

    #[test]
    fn openai_to_gemini_cli_request_validated_tools_and_thinking() {
        let body = json!({
            "messages": [{ "role": "user", "content": "Call add with 7 and 35." }],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "add",
                    "description": "Add two numbers",
                    "parameters": {
                        "type": "object",
                        "properties": { "a": { "type": "number" }, "b": { "type": "number" } },
                        "required": ["a", "b"]
                    }
                }
            }],
            "reasoning_effort": "high",
            "max_tokens": 128
        });
        let creds = json!({ "accessToken": "t", "projectId": "p" });
        let out = translate_request(
            FORMAT_OPENAI,
            FORMAT_GEMINI_CLI,
            "gemini-3.1-pro-preview",
            &body,
            true,
            Some(&creds),
            Some("gemini-cli"),
        );
        assert_eq!(out["project"], "p");
        assert_eq!(out["request"]["toolConfig"]["functionCallingConfig"]["mode"], "VALIDATED");
        assert!(out["request"]["safetySettings"].is_array());
        assert_eq!(
            out["request"]["generationConfig"]["thinkingConfig"],
            json!({ "thinkingLevel": "high", "includeThoughts": true })
        );
        assert_eq!(out["request"]["generationConfig"]["maxOutputTokens"], 65535);
    }

    #[test]
    fn antigravity_gemini4_high_reserves_thinking_output() {
        let body = json!({"max_tokens":200,"messages":[{"role":"user","content":"ping"}]});
        let out = translate_request("openai", "antigravity", "gemini-4-hydrogen-high", &body, false, None, Some("antigravity"));
        assert_eq!(out["request"]["generationConfig"]["maxOutputTokens"], 65535);
        assert_eq!(out["request"]["generationConfig"]["thinkingConfig"]["thinkingLevel"], "high");
    }

    #[test]
    fn claude_to_openai_stream_text_thinking_tool_usage_finish() {
        let events = vec![
            json!({ "type": "message_start", "message": { "id": "msg_1", "model": "claude-opus-4-6" } }),
            json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "thinking" } }),
            json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "thinking_delta", "thinking": "let me think" } }),
            json!({ "type": "content_block_stop", "index": 0 }),
            json!({ "type": "content_block_start", "index": 1, "content_block": { "type": "text" } }),
            json!({ "type": "content_block_delta", "index": 1, "delta": { "type": "text_delta", "text": "Hello" } }),
            json!({ "type": "content_block_stop", "index": 1 }),
            json!({ "type": "content_block_start", "index": 2, "content_block": { "type": "tool_use", "id": "tu_1", "name": "get_weather" } }),
            json!({ "type": "content_block_delta", "index": 2, "delta": { "type": "input_json_delta", "partial_json": "{\"city\":\"NYC\"}" } }),
            json!({ "type": "content_block_stop", "index": 2 }),
            json!({ "type": "message_delta", "delta": { "stop_reason": "tool_use" }, "usage": { "input_tokens": 10, "output_tokens": 5, "cache_read_input_tokens": 3 } }),
            json!({ "type": "message_stop" }),
        ];
        let chunks = run_stream(FORMAT_CLAUDE, FORMAT_OPENAI, events);
        assert!(chunks.iter().any(|c| c["choices"][0]["delta"]["reasoning_content"] == "let me think"));
        assert!(chunks.iter().any(|c| c["choices"][0]["delta"]["content"] == "Hello"));
        assert!(chunks.iter().any(|c| c["choices"][0]["delta"]["tool_calls"][0]["id"] == "tu_1"));
        let last = chunks.last().unwrap();
        assert_eq!(last["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(last["usage"]["prompt_tokens"], 13);
        assert_eq!(last["usage"]["completion_tokens"], 5);
        assert_eq!(last["usage"]["prompt_tokens_details"]["cached_tokens"], 3);
    }

    #[test]
    fn gemini_to_openai_stream_thought_text_tool_image_usage() {
        let events = vec![
            json!({ "candidates": [{ "content": { "parts": [{ "text": "thinking part", "thought": true }] } }], "responseId": "resp_1", "modelVersion": "gemini-3-pro" }),
            json!({ "candidates": [{ "content": { "parts": [{ "text": "Answer" }] } }] }),
            json!({ "candidates": [{ "content": { "parts": [{ "functionCall": { "name": "search", "args": { "q": "x" } } }] } }] }),
            json!({ "candidates": [{ "content": { "parts": [{ "inlineData": { "mimeType": "image/png", "data": "BASE64DATA" } }] } }] }),
            json!({ "candidates": [{ "finishReason": "STOP" }], "usageMetadata": { "promptTokenCount": 8, "candidatesTokenCount": 4, "thoughtsTokenCount": 2, "totalTokenCount": 14, "cachedContentTokenCount": 1 } }),
        ];
        let chunks = run_stream(FORMAT_GEMINI, FORMAT_OPENAI, events);
        assert!(chunks.iter().any(|c| c["choices"][0]["delta"]["reasoning_content"] == "thinking part"));
        assert!(chunks.iter().any(|c| c["choices"][0]["delta"]["content"] == "Answer"));
        assert!(chunks.iter().any(|c| c["choices"][0]["delta"]["tool_calls"][0]["function"]["name"] == "search"));
        assert!(chunks.iter().any(|c| c["choices"][0]["delta"]["images"][0]["image_url"]["url"] == "data:image/png;base64,BASE64DATA"));
        let last = chunks.last().unwrap();
        assert_eq!(last["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(last["usage"]["prompt_tokens"], 8);
        assert_eq!(last["usage"]["completion_tokens"], 6);
        assert_eq!(last["usage"]["completion_tokens_details"]["reasoning_tokens"], 2);
    }

    #[test]
    fn openai_responses_to_openai_stream_and_error() {
        let events = vec![
            json!({ "type": "response.output_text.delta", "delta": "Hello" }),
            json!({ "type": "response.reasoning_summary_text.delta", "delta": "thinking" }),
            json!({ "type": "response.output_item.added", "item": { "type": "function_call", "call_id": "call_1", "name": "get_weather" } }),
            json!({ "type": "response.function_call_arguments.delta", "delta": "{\"city\":\"NYC\"}" }),
            json!({ "type": "response.output_item.done", "item": { "type": "function_call" } }),
            json!({ "type": "response.completed", "response": { "usage": { "input_tokens": 10, "output_tokens": 5, "input_tokens_details": { "cached_tokens": 3 } } } }),
        ];
        let chunks = run_stream(FORMAT_OPENAI_RESPONSES, FORMAT_OPENAI, events);
        assert!(chunks.iter().any(|c| c["choices"][0]["delta"]["content"] == "Hello"));
        assert!(chunks.iter().any(|c| c["choices"][0]["delta"]["reasoning_content"] == "thinking"));
        assert!(chunks.iter().any(|c| c["choices"][0]["delta"]["tool_calls"][0]["id"] == "call_1"));
        let last = chunks.last().unwrap();
        assert_eq!(last["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(last["usage"]["prompt_tokens"], 10);
        assert_eq!(last["usage"]["prompt_tokens_details"]["cached_tokens"], 3);

        let err_chunks = run_stream(
            FORMAT_OPENAI_RESPONSES,
            FORMAT_OPENAI,
            vec![json!({ "type": "error", "error": { "message": "model_not_found" } })],
        );
        assert_eq!(err_chunks[0]["error"]["message"], "model_not_found");
    }

    #[test]
    fn pivot_route_claude_to_gemini_and_openai_to_responses_stream() {
        let claude_req = json!({
            "system": "Be concise",
            "messages": [{ "role": "user", "content": "Hi" }],
            "max_tokens": 256
        });
        let gemini_req = translate_request(FORMAT_CLAUDE, FORMAT_GEMINI, "gemini-2.5-pro", &claude_req, true, None, None);
        assert_eq!(gemini_req["systemInstruction"]["parts"][0]["text"], "Be concise");
        assert_eq!(gemini_req["contents"][0]["parts"][0]["text"], "Hi");

        let openai_chunks = vec![
            json!({ "id": "chatcmpl-1", "model": "gpt-5", "choices": [{ "delta": { "role": "assistant", "content": "Hi" }, "finish_reason": null }] }),
            json!({ "id": "chatcmpl-1", "model": "gpt-5", "choices": [{ "delta": {}, "finish_reason": "stop" }], "usage": { "prompt_tokens": 4, "completion_tokens": 2 } }),
        ];
        let resp_events = run_stream(FORMAT_OPENAI, FORMAT_OPENAI_RESPONSES, openai_chunks);
        assert!(resp_events.iter().any(|e| e["type"] == "response.created"));
        assert!(resp_events.iter().any(|e| e["type"] == "response.output_text.delta" && e["delta"] == "Hi"));
        assert!(resp_events.iter().any(|e| e["type"] == "response.completed"));
    }

    #[test]
    fn same_format_lifts_reasoning_aliases() {
        let chunk = json!({
            "id": "1",
            "choices": [{ "delta": { "reasoning": "alias thought" } }]
        });
        let mut state = init_state(FORMAT_OPENAI);
        let out = translate_response(FORMAT_OPENAI, FORMAT_OPENAI, Some(&chunk), &mut state);
        assert_eq!(out[0]["choices"][0]["delta"]["reasoning_content"], "alias thought");
        assert_eq!(out[0]["choices"][0]["delta"]["reasoning"], "alias thought");
    }
}
