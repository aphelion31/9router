use serde_json::{json, Value};

use crate::translator::antigravity::antigravity_to_openai_request;
use crate::translator::commandcode::{
    commandcode_to_openai_response, openai_to_commandcode_request, to_commandcode_session_uuid,
};
use crate::translator::concerns::{to_openai_finish, to_openai_usage};
use crate::translator::cursor::{cursor_to_openai_response, openai_to_cursor_request};
use crate::translator::deepseek_web_tools::{
    bind_deepseek_web_tools, build_deepseek_tools_prompt, find_tag_blocks,
    parse_deepseek_tool_calls, strip_tag_blocks,
};
use crate::translator::formats::{
    FORMAT_ANTIGRAVITY, FORMAT_CLAUDE, FORMAT_COMMANDCODE, FORMAT_KIRO,
    FORMAT_OLLAMA, FORMAT_OPENAI, FORMAT_VERTEX,
};
use crate::translator::kiro::{
    canonicalize_kiro_conversation, claude_to_kiro_request, kiro_to_claude_response,
    normalize_kiro_tool_specs, openai_to_kiro_request,
};
use crate::translator::modality::{
    extract_claude_text, filter_unsupported_params, is_claude_empty_content, strip_content_types,
};
use crate::translator::ollama::{
    ollama_body_to_openai, ollama_to_openai_response, openai_to_ollama_request,
};
use crate::translator::thinking::{
    apply_thinking, budget_to_effort, capture_thinking, effort_to_budget, extract_thinking,
    parse_suffix, ThinkingOverride,
};
use crate::translator::vertex::sanitize_vertex_schema;
use crate::translator::web_tools::{
    detect_web_tools, extract_urls_from_text, inject_web_context_into_messages,
    is_web_fetch_tool, is_web_search_tool, strip_web_tools,
};
use crate::translator::{init_state, translate_request, translate_response};

fn fixtures() -> Value {
    serde_json::from_str(include_str!("fixtures/parity_cases.json")).expect("valid parity_cases.json")
}

#[test]
fn antigravity_to_openai_request_fixture() {
    let fx = fixtures();
    let req = &fx["antigravity_request"];
    let out = antigravity_to_openai_request("claude-sonnet-4-5", req, true, None);
    let msgs = out["messages"].as_array().unwrap();
    assert_eq!(msgs[0]["role"], "system");
    assert!(msgs[0]["content"]
        .as_str()
        .unwrap()
        .contains("Be helpful and concise."));
    assert_eq!(msgs[1]["role"], "user");
    assert_eq!(msgs[1]["content"][1]["type"], "image_url");
    assert_eq!(msgs[2]["role"], "assistant");
    assert_eq!(msgs[2]["reasoning_content"], "Let me think");
    assert_eq!(msgs[2]["tool_calls"][0]["function"]["name"], "read_file");
    assert_eq!(msgs[3]["role"], "tool");
    assert_eq!(out["tools"][0]["function"]["parameters"]["type"], "OBJECT");
    assert_eq!(out["reasoning_effort"], "medium");

    let routed = translate_request(
        FORMAT_ANTIGRAVITY,
        FORMAT_OPENAI,
        "claude-sonnet-4-5",
        req,
        true,
        None,
        None,
    );
    assert_eq!(routed["model"], "claude-sonnet-4-5");
}

#[test]
fn vertex_request_and_schema_sanitization() {
    let fx = fixtures();
    let out = translate_request(
        FORMAT_OPENAI,
        FORMAT_VERTEX,
        "gemini-2.5-pro",
        &fx["vertex_request"],
        true,
        None,
        None,
    );
    assert!(out.get("systemInstruction").is_some());
    let params = &out["tools"][0]["functionDeclarations"][0]["parameters"];
    assert!(params.get("$schema").is_none());
    assert!(params.get("additionalProperties").is_none());
    assert_eq!(params["required"], json!(["q", "missing_field"]));

    let sanitized = sanitize_vertex_schema(&json!({
        "type": ["string", "null"],
        "additionalProperties": false,
        "properties": { "a": { "type": "number", "minimum": 0 } },
        "required": ["a", "b"]
    }));
    assert_eq!(sanitized["type"], "string");
    assert!(sanitized.get("additionalProperties").is_none());
}

#[test]
fn ollama_request_stream_and_nonstream() {
    let fx = fixtures();
    let req = json!({
        "messages": [
            { "role": "user", "content": [
                { "type": "text", "text": "describe" },
                { "type": "image_url", "image_url": { "url": "data:image/png;base64,AAAA" } }
            ] },
            { "role": "assistant", "content": "", "tool_calls": [
                { "id": "call_1", "type": "function", "function": { "name": "f", "arguments": "{\"x\":1}" } }
            ] },
            { "role": "tool", "tool_call_id": "call_1", "content": "ok" }
        ],
        "temperature": 0.4,
        "max_tokens": 512,
        "top_p": 0.9
    });
    let out = openai_to_ollama_request("qwen3:8b", &req, true, None);
    assert_eq!(out["model"], "qwen3:8b");
    assert_eq!(out["messages"][0]["images"], json!(["AAAA"]));
    assert_eq!(out["messages"][1]["tool_calls"][0]["function"]["arguments"], json!({ "x": 1 }));
    assert_eq!(out["messages"][2]["tool_name"], "f");
    assert_eq!(out["options"]["num_predict"], 512);

    let mut state = init_state(FORMAT_OPENAI);
    let mut chunks = Vec::new();
    for ev in fx["ollama_stream"].as_array().unwrap() {
        chunks.extend(ollama_to_openai_response(Some(ev), &mut state));
    }
    assert!(chunks.iter().any(|c| c["choices"][0]["delta"]["reasoning_content"] == "hmm"));
    assert!(chunks.iter().any(|c| c["choices"][0]["delta"]["content"] == "Hello "));
    assert!(chunks.iter().any(|c| c["choices"][0]["delta"]["tool_calls"][0]["id"] == "call_ol_1"));
    let last = chunks.last().unwrap();
    assert_eq!(last["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(last["usage"]["prompt_tokens"], 12);
    assert_eq!(last["usage"]["completion_tokens"], 7);

    let body = ollama_body_to_openai(&json!({
        "model": "qwen3:8b",
        "message": { "role": "assistant", "content": "Done" },
        "done_reason": "stop",
        "prompt_eval_count": 5,
        "eval_count": 3
    }));
    assert_eq!(body["choices"][0]["message"]["content"], "Done");
    assert_eq!(body["choices"][0]["finish_reason"], "stop");
}

#[test]
fn cursor_request_and_response() {
    let body = json!({
        "max_tokens": 200,
        "user": "u1",
        "messages": [
            { "role": "system", "content": "sys" },
            { "role": "assistant", "content": "", "tool_calls": [
                { "id": "call_c1\nextra", "index": 0, "type": "function", "function": { "name": "read_file", "arguments": "{}" } }
            ] },
            { "role": "tool", "tool_call_id": "call_c1\nextra", "content": "ok\x00" }
        ]
    });
    let out = openai_to_cursor_request("cursor-small", &body, true, None);
    assert!(out.get("user").is_none());
    assert_eq!(out["max_tokens"], 32000);
    assert!(out["messages"][0]["content"].as_str().unwrap().contains("[System Instructions]"));
    assert!(out["messages"][1]["tool_calls"][0].get("index").is_none());
    let tool_msg = out["messages"][2]["content"].as_str().unwrap();
    assert!(tool_msg.contains("<tool_name>read_file</tool_name>"));
    assert!(!tool_msg.contains('\x00'));

    let mut state = init_state(FORMAT_OPENAI);
    let passthrough = json!({ "id": "c1", "choices": [{ "delta": { "content": "hi" } }] });
    assert_eq!(cursor_to_openai_response(Some(&passthrough), &mut state), vec![passthrough]);
}

#[test]
fn commandcode_request_session_uuid_and_stream() {
    let fx = fixtures();
    let u1 = to_commandcode_session_uuid(Some("sess-abc"));
    let u2 = to_commandcode_session_uuid(Some("sess-abc"));
    assert_eq!(u1, u2);
    assert_eq!(u1.len(), 36);

    let req = json!({
        "messages": [
            { "role": "system", "content": "Follow rules" },
            { "role": "user", "content": [
                { "type": "text", "text": "look" },
                { "type": "image_url", "image_url": { "url": "data:image/png;base64,BBBB" } }
            ] }
        ],
        "tools": [
            { "type": "function", "function": { "name": "bash", "description": "run", "parameters": { "type": "object" } } }
        ]
    });
    let out = openai_to_commandcode_request("claude-sonnet-4", &req, true, Some(&json!({ "_clientSessionId": "sess-1" })));
    assert_eq!(out["params"]["system"], "Follow rules");
    assert_eq!(out["params"]["messages"][0]["content"][1]["type"], "image");
    assert_eq!(out["params"]["tools"][0]["name"], "bash");

    let mut state = init_state(FORMAT_OPENAI);
    let mut chunks = Vec::new();
    for ev in fx["commandcode_stream"].as_array().unwrap() {
        chunks.extend(commandcode_to_openai_response(Some(ev), &mut state));
    }
    assert!(chunks.iter().any(|c| c["choices"][0]["delta"]["reasoning_content"] == "plan"));
    assert!(chunks.iter().any(|c| c["choices"][0]["delta"]["content"] == "Hi"));
    assert!(chunks.iter().any(|c| c["choices"][0]["delta"]["tool_calls"][0]["id"] == "tc_1"));
    let last = chunks.last().unwrap();
    assert_eq!(last["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(last["usage"]["prompt_tokens"], 20);
    assert_eq!(last["usage"]["completion_tokens"], 10);
}

#[test]
fn kiro_requests_conversation_canonicalization_and_streams() {
    let fx = fixtures();
    let (specs, name_map) = normalize_kiro_tool_specs(Some(&vec![
        json!({ "name": "mcp__gitea__search_repos", "description": "Search" }),
        json!({ "name": "my.tool/search", "description": "Dot" }),
        json!({ "name": "my_tool_search", "description": "Exact" }),
    ]));
    assert_eq!(specs.len(), 3);
    assert_eq!(name_map.get("mcp__gitea__search_repos").unwrap(), "mcp__gitea__search_repos");
    assert_eq!(name_map.get("my.tool/search").unwrap(), "my_tool_search");
    assert_eq!(name_map.get("my_tool_search").unwrap(), "my_tool_search_2");

    let claude_body = json!({
        "system": "Be brief",
        "thinking": { "type": "enabled", "budget_tokens": 4096 },
        "tools": [{ "name": "read_file", "description": "Read", "input_schema": { "type": "object", "properties": { "path": { "type": "string" } } } }],
        "messages": [
            { "role": "user", "content": "Read Cargo.toml" },
            { "role": "assistant", "content": [{ "type": "tool_use", "id": "tu_1", "name": "read_file", "input": { "path": "Cargo.toml" } }] },
            { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": "tu_1", "content": "ok" }] }
        ]
    });
    let kiro_from_claude = claude_to_kiro_request("claude-sonnet-4", &claude_body, true, None);
    let hist = kiro_from_claude["conversationState"]["history"].as_array().unwrap();
    assert_eq!(hist.len(), 2);
    assert!(hist[0]["userInputMessage"]["content"].as_str().unwrap().contains("<thinking_mode>enabled</thinking_mode>"));
    let cur = &kiro_from_claude["conversationState"]["currentMessage"]["userInputMessage"];
    assert_eq!(cur["userInputMessageContext"]["toolResults"][0]["toolUseId"], "tu_1");

    let openai_body = json!({
        "messages": [
            { "role": "user", "content": "go" },
            { "role": "assistant", "content": "", "tool_calls": [{ "id": "c1", "type": "function", "function": { "name": "f", "arguments": "{bad json" } }] },
            { "role": "tool", "tool_call_id": "c1", "content": "r" }
        ]
    });
    let kiro_from_openai = openai_to_kiro_request("claude-sonnet-4", &openai_body, true, None);
    assert!(kiro_from_openai["conversationState"]["currentMessage"]["userInputMessage"]["content"].is_string());

    // Canonicalize orphan tool results and assistant-first history
    let (canon_hist, canon_cur) = canonicalize_kiro_conversation(
        vec![json!({ "assistantResponseMessage": { "content": "hello", "toolUses": [{ "toolUseId": "missing", "name": "unknown", "input": {} }] } })],
        None,
        "claude-sonnet-4",
        &[],
        &Default::default(),
    );
    assert!(canon_hist[0].get("userInputMessage").is_some());
    assert!(canon_cur.get("userInputMessage").is_some());

    // Kiro -> Claude stream
    let mut c_state = init_state(FORMAT_CLAUDE);
    let mut c_events = Vec::new();
    for ev in fx["kiro_stream"].as_array().unwrap() {
        c_events.extend(kiro_to_claude_response(Some(ev), &mut c_state));
    }
    c_events.extend(kiro_to_claude_response(None, &mut c_state));
    assert!(c_events.iter().any(|e| e["type"] == "message_start"));
    assert!(c_events.iter().any(|e| e["type"] == "content_block_delta" && e["delta"]["text"] == "Working on it."));
    assert!(c_events.iter().any(|e| e["type"] == "content_block_start" && e["content_block"]["name"] == "read_file"));
    assert!(c_events.iter().any(|e| e["type"] == "message_stop"));

    // Kiro -> OpenAI stream via translate_response
    let mut o_state = init_state(FORMAT_OPENAI);
    let mut o_chunks = Vec::new();
    for ev in fx["kiro_stream"].as_array().unwrap() {
        o_chunks.extend(translate_response(FORMAT_KIRO, FORMAT_OPENAI, Some(ev), &mut o_state));
    }
    assert!(o_chunks.iter().any(|c| c["choices"][0]["delta"]["content"] == "Working on it."));
    assert!(o_chunks.iter().any(|c| c["choices"][0]["delta"]["tool_calls"][0]["id"] == "tu_k_1"));
    assert_eq!(o_chunks.last().unwrap()["choices"][0]["finish_reason"], "tool_calls");
}

#[test]
fn concerns_kiro_ollama_commandcode_finish_and_usage() {
    assert_eq!(to_openai_finish(FORMAT_KIRO, Some("tool_use"), false), "tool_calls");
    assert_eq!(to_openai_finish(FORMAT_OLLAMA, Some("length"), false), "length");
    assert_eq!(to_openai_finish(FORMAT_COMMANDCODE, Some("tool-calls"), false), "tool_calls");

    let u_kiro = to_openai_usage(FORMAT_KIRO, Some(&json!({ "inputTokens": 120, "outputTokens": 30, "cachedTokens": 40 }))).unwrap();
    assert_eq!(u_kiro["prompt_tokens"], 120);
    assert_eq!(u_kiro["completion_tokens"], 30);
    assert_eq!(u_kiro["prompt_tokens_details"]["cached_tokens"], 40);
    let u_ol = to_openai_usage(FORMAT_OLLAMA, Some(&json!({ "prompt_eval_count": 7, "eval_count": 3 }))).unwrap();
    assert_eq!(u_ol["total_tokens"], 10);
    let u_cc = to_openai_usage(FORMAT_COMMANDCODE, Some(&json!({ "inputTokens": 9, "outputTokens": 4, "totalTokens": 13 }))).unwrap();
    assert_eq!(u_cc["total_tokens"], 13);
}

#[test]
fn thinking_modality_web_tools_and_deepseek_helpers() {
    // thinking.rs
    let (base, suffix) = parse_suffix("claude-opus-4-6(high)");
    assert_eq!(base, "claude-opus-4-6");
    assert_eq!(suffix, Some(ThinkingOverride::Level("high".to_string())));
    assert_eq!(effort_to_budget("high"), Some(16384));
    assert_eq!(budget_to_effort(16384), "high");

    let cfg = extract_thinking(&json!({ "reasoning_effort": "high" })).unwrap();
    assert_eq!(cfg.mode, "level");
    let mut applied = json!({});
    apply_thinking("claude", "claude-opus-4-6", &mut applied, Some(&cfg));
    assert_eq!(applied["thinking"]["type"], "enabled");
    assert_eq!(applied["thinking"]["budget_tokens"], 16384);

    let cap_cfg = capture_thinking(&json!({ "reasoning_effort": "medium" })).unwrap();
    assert_eq!(cap_cfg.mode, "level");
    assert_eq!(cap_cfg.level.as_deref(), Some("medium"));

    // modality.rs
    let mut stripped = json!({
        "messages": [{
            "role": "user",
            "content": [
                { "type": "text", "text": "hello" },
                { "type": "image_url", "image_url": { "url": "data:image/png;base64,AA" } }
            ]
        }]
    });
    strip_content_types(&mut stripped, &["image"]);
    assert_eq!(stripped["messages"][0]["content"].as_array().unwrap().len(), 1);
    let mut filtered = json!({ "model": "m", "temperature": 0.5, "frequency_penalty": 0.1 });
    filter_unsupported_params(&mut filtered, &["frequency_penalty"]);
    assert!(filtered.get("frequency_penalty").is_none());
    assert_eq!(extract_claude_text(&json!([{ "type": "text", "text": "a" }, { "type": "text", "text": "b" }])), "a\nb");
    assert!(is_claude_empty_content(&json!([{ "type": "text", "text": "  " }])));

    // web_tools.rs
    assert!(is_web_search_tool(&json!({ "type": "web_search_20250305" })));
    assert!(is_web_fetch_tool(&json!({ "type": "function", "function": { "name": "web_fetch" } })));
    let tools = vec![
        json!({ "type": "web_search" }),
        json!({ "type": "function", "function": { "name": "read_file" } }),
    ];
    let tools_val = Value::Array(tools.clone());
    let (has_search, _has_fetch) = detect_web_tools(Some(&tools_val));
    assert!(has_search);
    let mut strip_body = json!({ "tools": tools });
    let (removed, _, _) = strip_web_tools(&mut strip_body);
    assert_eq!(removed, 1);
    assert_eq!(strip_body["tools"].as_array().unwrap().len(), 1);
    assert_eq!(extract_urls_from_text("check https://example.com/docs."), vec!["https://example.com/docs"]);
    let mut inject_body = json!({ "messages": [{ "role": "user", "content": "hi" }] });
    inject_web_context_into_messages(&mut inject_body, "CTX");
    assert!(inject_body["messages"][0]["content"].as_str().unwrap().contains("CTX"));

    // deepseek_web_tools.rs
    let blocks = find_tag_blocks("before <tool_call>{\"name\":\"x\"}</tool_call> after", "tool_call");
    assert_eq!(blocks.len(), 1);
    assert_eq!(strip_tag_blocks("a <think>b</think> c", "think"), "a  c");
    let prompt = build_deepseek_tools_prompt(&tools);
    assert!(prompt.contains("read_file"));
    let (_cleaned, calls) = parse_deepseek_tool_calls("<tool_call>{\"name\":\"web_search\",\"arguments\":{\"q\":\"rust\"}}</tool_call>");
    assert_eq!(calls[0].name, "web_search");
    let mut ds_body = json!({ "messages": [{ "role": "user", "content": "hi" }], "tools": tools });
    bind_deepseek_web_tools(&mut ds_body);
    assert!(ds_body.get("tools").is_none());
}
