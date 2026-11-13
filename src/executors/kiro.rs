use crate::services::aws_event_stream::{crc32, parse_event_frame};
use axum::http::Response as HttpResponse;
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

pub const KIRO_CODEWHISPERER_TARGET: &str =
    "AmazonCodeWhispererStreamingService.GenerateAssistantResponse";

const KIRO_BASE_URLS: [&str; 3] = [
    "https://runtime.us-east-1.kiro.dev/generateAssistantResponse",
    "https://codewhisperer.us-east-1.amazonaws.com/generateAssistantResponse",
    "https://q.us-east-1.amazonaws.com/generateAssistantResponse",
];

const KIRO_REPAIR_BUFFER_MAX_BYTES: usize = 8 * 1024 * 1024;
const EVENTSTREAM_MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
const EVENTSTREAM_MAX_HEADERS_BYTES: usize = 128 * 1024;
const KIRO_SHORT_FINAL_MAX_CHARS: usize = 800;
const DEFAULT_CONTEXT_WINDOW: u64 = 200_000;

fn random_uuid() -> String {
    let mut bytes = [0u8; 16];
    let _ = SystemRandom::new().fill(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn regionalize_url(url: &str, region: &str) -> String {
    if region.is_empty() || region == "us-east-1" || !url.contains("amazonaws.com") {
        return url.to_string();
    }
    if let Some(scheme_end) = url.find("://") {
        let rest = &url[scheme_end + 3..];
        if let Some(host_end) = rest.find('/') {
            let host = &rest[..host_end];
            let path = &rest[host_end..];
            if let Some(first_dot) = host.find('.') {
                let svc = &host[..first_dot];
                if host.ends_with(".amazonaws.com") {
                    return format!("{}://{svc}.{region}.amazonaws.com{path}", &url[..scheme_end]);
                }
            }
        }
    }
    url.to_string()
}

pub fn ordered_base_urls(base_override: Option<&str>, psd: Option<&Value>) -> Vec<String> {
    if let Some(u) = base_override.map(str::trim).filter(|s| !s.is_empty()) {
        return vec![u.to_string()];
    }
    let region = psd
        .and_then(|v| v.get("region"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("us-east-1");

    let mut amazon_q = Vec::new();
    let mut amazon_other = Vec::new();
    let mut others = Vec::new();

    for url in KIRO_BASE_URLS {
        if url.contains("amazonaws.com") {
            let r = regionalize_url(url, region);
            if r.contains("://q.") {
                amazon_q.push(r);
            } else {
                amazon_other.push(r);
            }
        } else {
            others.push(url.to_string());
        }
    }

    let mut out = Vec::with_capacity(KIRO_BASE_URLS.len());
    out.extend(amazon_q);
    out.extend(amazon_other);
    out.extend(others);
    out
}

pub fn build_headers(request: &super::ExecutorRequest, url: &str) -> Vec<(String, String)> {
    let mut headers = vec![
        ("Content-Type".into(), "application/json".into()),
        ("Accept".into(), "application/vnd.amazon.eventstream".into()),
        ("User-Agent".into(), "AWS-SDK-JS/3.0.0 kiro-ide/1.0.0".into()),
        (
            "X-Amz-User-Agent".into(),
            "aws-sdk-js/3.0.0 kiro-ide/1.0.0".into(),
        ),
        ("Amz-Sdk-Request".into(), "attempt=1; max=3".into()),
        ("Amz-Sdk-Invocation-Id".into(), random_uuid()),
    ];
    if url.contains("://codewhisperer.") {
        headers.push(("X-Amz-Target".into(), KIRO_CODEWHISPERER_TARGET.into()));
    }

    let psd = request.provider_specific_data.as_ref();
    let auth_method = psd
        .and_then(|v| v.get("authMethod"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let is_api_key = auth_method == "api_key";
    let is_external_idp = auth_method == "external_idp";

    let api_key = request
        .api_key
        .as_deref()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            if is_api_key {
                request.access_token.as_deref().filter(|s| !s.is_empty())
            } else {
                None
            }
        });

    if is_api_key {
        if let Some(key) = api_key {
            headers.push(("Authorization".into(), format!("Bearer {key}")));
            headers.push(("TokenType".into(), "API_KEY".into()));
        }
    } else if let Some(token) = request.access_token.as_deref().filter(|s| !s.is_empty()) {
        headers.push(("Authorization".into(), format!("Bearer {token}")));
        if is_external_idp {
            headers.push(("TokenType".into(), "EXTERNAL_IDP".into()));
        }
    }

    if let Some(token) = request.access_token.as_deref().filter(|s| !s.is_empty()) {
        headers.push(("x-amz-sso-bearer".into(), token.into()));
    }
    headers.push(("x-amzn-kiro-agent-mode".into(), "spec".into()));
    headers.push(("x-amzn-codewhisperer-machine-id".into(), "kiro-desktop".into()));
    if let Some(arn) = psd
        .and_then(|v| v.get("profileArn"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        headers.push(("x-amzn-codewhisperer-profile-arn".into(), arn.into()));
    }

    headers
}

fn should_fallback_status(status: u16) -> bool {
    matches!(status, 401 | 403 | 404 | 429 | 500..=599)
}

fn normalize_stop_reason(value: Option<&Value>) -> Option<String> {
    let raw = value?.as_str()?.trim();
    if raw.is_empty() {
        return None;
    }
    let mut snake = String::with_capacity(raw.len() + 4);
    let mut prev_lower = false;
    for ch in raw.chars() {
        if ch.is_ascii_uppercase() && prev_lower {
            snake.push('_');
            snake.push(ch.to_ascii_lowercase());
            prev_lower = false;
        } else if ch == '-' || ch == ' ' {
            snake.push('_');
            prev_lower = false;
        } else {
            prev_lower = ch.is_ascii_lowercase();
            snake.push(ch.to_ascii_lowercase());
        }
    }
    match snake.as_str() {
        "endturn" | "end_turn" | "stop" | "stop_sequence" => Some("end_turn".into()),
        "tooluse" | "tool_use" | "tool_calls" => Some("tool_use".into()),
        "maxtokens" | "max_tokens" | "max_output_tokens" | "length" => Some("max_tokens".into()),
        _ => Some(snake),
    }
}

fn stop_disposition(stop_reason: Option<&str>, has_tool_calls: bool) -> &'static str {
    let Some(sr) = stop_reason else {
        return if has_tool_calls { "tool_use" } else { "complete" };
    };
    if matches!(sr, "malformed_model_output" | "invalid_model_output") {
        return "retryable_protocol_failure";
    }
    if matches!(
        sr,
        "cancelled" | "pause_turn" | "model_context_window_exceeded"
    ) {
        return "terminal_incomplete";
    }
    if sr == "refusal"
        || sr.contains("content") && sr.contains("filter")
        || sr.contains("guardrail")
        || sr.contains("safety")
        || sr.contains("policy")
        || sr.contains("blocked")
    {
        return "terminal_refusal";
    }
    if sr == "max_tokens" {
        return if has_tool_calls {
            "terminal_incomplete"
        } else {
            "length"
        };
    }
    if !matches!(sr, "end_turn" | "tool_use") {
        return "unknown_failure";
    }
    if has_tool_calls || sr == "tool_use" {
        return "tool_use";
    }
    "complete"
}

fn merge_stop_reason(current: Option<String>, incoming: Option<String>) -> Option<String> {
    let Some(inc) = incoming else {
        return current;
    };
    let Some(cur) = current else {
        return Some(inc);
    };
    let severity = |reason: &str| -> u8 {
        match stop_disposition(Some(reason), false) {
            "terminal_refusal" => 6,
            "terminal_incomplete" => 5,
            "unknown_failure" => 4,
            "retryable_protocol_failure" => 3,
            "length" => 2,
            _ => 1,
        }
    };
    if severity(&inc) > severity(&cur) {
        Some(inc)
    } else {
        Some(cur)
    }
}

fn is_truncation_stop_reason(stop_reason: Option<&str>) -> bool {
    matches!(
        stop_reason,
        Some("model_context_window_exceeded" | "max_tokens")
    )
}

fn is_ellipsis_only(value: &str) -> bool {
    matches!(value.trim(), "..." | "…")
}

fn is_short_future_action(value: &str) -> bool {
    let text = value.trim().replace('’', "'");
    if text.is_empty() || text.chars().count() > KIRO_SHORT_FINAL_MAX_CHARS {
        return false;
    }
    let lower = text.to_ascii_lowercase();
    let has_user_wait = lower.contains("after you")
        || lower.contains("when you")
        || lower.contains("once you")
        || lower.contains("your approval")
        || lower.contains("your confirmation")
        || lower.contains("your permission")
        || lower.contains("your input")
        || lower.contains("waiting for you")
        || lower.contains("wait for you")
        || lower.contains("please approve")
        || lower.contains("please confirm")
        || lower.contains("please provide")
        || lower.contains("please send")
        || text.contains("請你")
        || text.contains("請先")
        || text.contains("等你")
        || text.contains("等待你")
        || text.contains("核准")
        || text.contains("同意")
        || text.contains("授權");
    if has_user_wait {
        return false;
    }
    let has_completed = lower.contains("done")
        || lower.contains("completed")
        || lower.contains("fixed")
        || lower.contains("verified")
        || lower.contains("confirmed")
        || lower.contains("passed")
        || lower.contains("in conclusion")
        || lower.contains("summary")
        || lower.contains("is complete")
        || lower.contains("are complete")
        || text.contains("已經完成")
        || text.contains("已完成")
        || text.contains("修復完成")
        || text.contains("確認無誤")
        || text.contains("驗證完成")
        || text.contains("驗證通過")
        || text.contains("結論")
        || text.contains("總結");
    if has_completed {
        return false;
    }
    let has_result_evidence = lower.contains("found")
        || lower.contains("shows")
        || lower.contains("showed")
        || lower.contains("because")
        || lower.contains("therefore")
        || lower.contains("succeeded")
        || lower.contains("failed")
        || lower.contains("healthy")
        || lower.contains("no error")
        || text.contains("顯示")
        || text.contains("發現")
        || text.contains("因此")
        || text.contains("成功")
        || text.contains("失敗")
        || text.contains("正常")
        || text.contains("無錯誤")
        || text.contains("沒有錯誤");
    if has_result_evidence {
        return false;
    }
    let en_trimmed = lower
        .trim_start_matches(|c: char| c.is_whitespace() || c == ',' || c == ':' || c == '-')
        .trim_start_matches("next")
        .trim_start_matches("now")
        .trim_start_matches("then")
        .trim_start_matches(|c: char| c.is_whitespace() || c == ',' || c == ':' || c == '-');
    let en_future = [
        "i'll ",
        "i will ",
        "i am going to ",
        "i need to ",
        "let me ",
    ]
    .iter()
    .any(|prefix| {
        if let Some(rest) = en_trimmed.strip_prefix(prefix) {
            [
                "verify",
                "check",
                "confirm",
                "validate",
                "investigate",
                "trace",
                "continue",
                "follow up",
                "test",
            ]
            .iter()
            .any(|verb| rest.starts_with(verb))
        } else {
            false
        }
    });
    if en_future {
        let has_en_result = lower.contains(':')
            || lower.contains(';')
            || lower.contains('\n')
            || lower.contains("status is")
            || lower.contains("response is")
            || lower.contains("checksum is");
        return !has_en_result;
    }
    false
}

fn append_repair_instruction(body: &Value, kind: &str) -> Value {
    let instruction = match kind {
        "tool" => {
            "Retry the previous response because its Kiro tool_call wrapper was malformed. If you use the wrapper tool named tool_call, its input must contain a non-empty name and an arguments field."
        }
        "ellipsis" => {
            "Retry the previous response because it ended with only an ellipsis. Return the complete final answer, not only ... or …."
        }
        "short_final" => {
            "Retry the previous response because its final only announced a future action. Complete the check now and return the result or a concrete blocker."
        }
        _ => "Retry the previous incomplete Kiro response.",
    };
    let mut repaired = body.clone();
    if let Some(msg) = repaired
        .pointer_mut("/conversationState/currentMessage/userInputMessage")
        .and_then(Value::as_object_mut)
    {
        let existing = msg
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let next = if existing.is_empty() {
            instruction.to_string()
        } else {
            format!("{existing}\n\n{instruction}")
        };
        msg.insert("content".into(), Value::String(next));
    }
    repaired
}

fn encode_sse_error(code: &str, message: &str, details: Option<&Value>) -> Vec<u8> {
    let mut err = Map::new();
    err.insert("message".into(), Value::String(message.into()));
    err.insert("type".into(), Value::String("upstream_error".into()));
    err.insert("code".into(), Value::String(code.into()));
    if let Some(d) = details {
        err.insert("details".into(), d.clone());
    }
    let payload = json!({ "error": Value::Object(err) });
    format!("data: {payload}\n\ndata: [DONE]\n\n").into_bytes()
}

fn sse_chunk(
    id: &str,
    created: u64,
    model: &str,
    delta: Value,
    finish_reason: Option<&str>,
    usage: Option<&Value>,
) -> Vec<u8> {
    let mut obj = Map::new();
    obj.insert("id".into(), Value::String(id.into()));
    obj.insert("object".into(), Value::String("chat.completion.chunk".into()));
    obj.insert("created".into(), Value::from(created));
    obj.insert("model".into(), Value::String(model.into()));
    obj.insert(
        "choices".into(),
        json!([{ "index": 0, "delta": delta, "finish_reason": finish_reason }]),
    );
    if let Some(u) = usage {
        obj.insert("usage".into(), u.clone());
    }
    format!("data: {}\n\n", Value::Object(obj)).into_bytes()
}

enum ToolInput {
    StringChunks(Vec<String>),
    Object(Value, usize),
}

struct ToolEntry {
    id: String,
    name: String,
    input: Option<ToolInput>,
}

enum AttemptOutcome {
    Complete(Vec<u8>),
    Retryable { kind: &'static str, message: String },
    TerminalError(Vec<u8>),
}

fn transform_eventstream_to_sse(
    bytes: &[u8],
    model: &str,
    max_tool_bytes: usize,
) -> AttemptOutcome {
    let created = now_epoch_secs();
    let response_id = format!("chatcmpl-{created}");
    let mut out = Vec::new();
    let mut offset = 0usize;
    let mut chunk_index = 0usize;
    let mut tool_counter = 0usize;
    let mut tools: BTreeMap<String, ToolEntry> = BTreeMap::new();
    let mut tool_order: Vec<String> = Vec::new();
    let mut buffered_tool_bytes = 0usize;

    let mut has_text = false;
    let mut has_reasoning = false;
    let mut has_code = false;
    let mut has_tool_calls = false;
    let mut saw_tool_use = false;
    let mut explicit_stop = false;
    let mut stop_reason: Option<String> = None;
    let mut terminal_provenance: Option<&'static str> = None;
    let mut total_content_length = 0usize;
    let mut context_usage_percentage = 0.0f64;
    let mut has_context_usage = false;
    let mut has_metering = false;
    let mut usage: Option<Map<String, Value>> = None;
    let mut in_thinking = false;
    let mut tool_validation_error: Option<String> = None;
    let mut accumulated_content = String::new();
    let mut accumulated_reasoning = String::new();

    let emit_delta = |out: &mut Vec<u8>, chunk_index: &mut usize, mut delta: Map<String, Value>| {
        if *chunk_index == 0 {
            delta.insert("role".into(), Value::String("assistant".into()));
        }
        *chunk_index += 1;
        out.extend_from_slice(&sse_chunk(
            &response_id,
            created,
            model,
            Value::Object(delta),
            None,
            None,
        ));
    };

    while bytes.len() - offset >= 12 {
        let slice = &bytes[offset..];
        let prelude_crc = u32::from_be_bytes(slice[8..12].try_into().unwrap());
        if prelude_crc != crc32(&slice[..8]) {
            return AttemptOutcome::TerminalError(encode_sse_error(
                "kiro_missing_terminal",
                "Kiro EventStream prelude CRC mismatch",
                None,
            ));
        }
        let total_len = u32::from_be_bytes(slice[0..4].try_into().unwrap()) as usize;
        let headers_len = u32::from_be_bytes(slice[4..8].try_into().unwrap()) as usize;
        if total_len < 16
            || total_len > EVENTSTREAM_MAX_MESSAGE_BYTES
            || headers_len > EVENTSTREAM_MAX_HEADERS_BYTES
            || headers_len > total_len - 16
        {
            return AttemptOutcome::TerminalError(encode_sse_error(
                "kiro_missing_terminal",
                "Kiro EventStream frame bounds are invalid",
                None,
            ));
        }
        if slice.len() < total_len {
            break;
        }
        let frame = &slice[..total_len];
        offset += total_len;
        let (headers, payload) = match parse_event_frame(
            frame,
            EVENTSTREAM_MAX_MESSAGE_BYTES,
            EVENTSTREAM_MAX_HEADERS_BYTES,
        ) {
            Ok(parsed) => parsed,
            Err(err) => {
                return AttemptOutcome::TerminalError(encode_sse_error(
                    "kiro_missing_terminal",
                    &err,
                    None,
                ));
            }
        };

        let msg_type = headers
            .get(":message-type")
            .and_then(Value::as_str)
            .unwrap_or("");
        if msg_type == "error" || msg_type == "exception" {
            let msg = payload
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("Kiro upstream sent an EventStream {msg_type}"));
            return AttemptOutcome::TerminalError(encode_sse_error(
                "kiro_upstream_eventstream_error",
                &msg,
                None,
            ));
        }

        let event_type = headers
            .get(":event-type")
            .and_then(Value::as_str)
            .unwrap_or("");

        match event_type {
            "assistantResponseEvent" => {
                if let Some(raw_content) = payload.get("content").and_then(Value::as_str) {
                    let mut content = raw_content.to_string();
                    if in_thinking {
                        if let Some(end) = content.find("</thinking>") {
                            in_thinking = false;
                            let rest = &content[end + 11..];
                            content = rest.strip_prefix('\n').unwrap_or(rest).to_string();
                        } else {
                            content.clear();
                        }
                    } else if let Some(start) = content.find("<thinking>") {
                        if let Some(rel_end) = content[start + 10..].find("</thinking>") {
                            let end = start + 10 + rel_end;
                            let after = &content[end + 11..];
                            let after = after.strip_prefix('\n').unwrap_or(after);
                            content = format!("{}{}", &content[..start], after);
                        } else {
                            in_thinking = true;
                            content.truncate(start);
                        }
                    }
                    if !content.is_empty() || !has_reasoning {
                        if !content.is_empty() {
                            has_text = true;
                        }
                        total_content_length += content.len();
                        accumulated_content.push_str(&content);
                        let mut delta = Map::new();
                        delta.insert("content".into(), Value::String(content));
                        emit_delta(&mut out, &mut chunk_index, delta);
                    }
                }
            }
            "reasoningContentEvent" => {
                let value = payload.get("reasoningContentEvent").unwrap_or(&payload);
                let text = if let Some(s) = value.as_str() {
                    s
                } else {
                    value
                        .get("text")
                        .or_else(|| value.get("content"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                };
                if !text.is_empty() {
                    has_reasoning = true;
                    total_content_length += text.len();
                    accumulated_reasoning.push_str(text);
                    let mut delta = Map::new();
                    delta.insert("reasoning_content".into(), Value::String(text.into()));
                    emit_delta(&mut out, &mut chunk_index, delta);
                }
            }
            "codeEvent" => {
                if let Some(content) = payload.get("content").and_then(Value::as_str) {
                    has_code = true;
                    total_content_length += content.len();
                    accumulated_content.push_str(content);
                    let mut delta = Map::new();
                    delta.insert("content".into(), Value::String(content.into()));
                    emit_delta(&mut out, &mut chunk_index, delta);
                }
            }
            "toolUseEvent" => {
                saw_tool_use = true;
                let values: Vec<&Value> = match payload.as_array() {
                    Some(arr) => arr.iter().collect(),
                    None => vec![&payload],
                };
                for val in values {
                    let Some(name) = val
                        .get("name")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                    else {
                        tool_validation_error
                            .get_or_insert_with(|| "Kiro toolUseEvent is missing a tool name".into());
                        continue;
                    };
                    let id = match val.get("toolUseId") {
                        None | Some(Value::Null) => {
                            format!("call_{created}_{}", tools.len() + 1)
                        }
                        Some(Value::String(s)) if !s.trim().is_empty() => s.clone(),
                        _ => {
                            tool_validation_error.get_or_insert_with(|| {
                                "Kiro toolUseEvent has an invalid toolUseId".into()
                            });
                            continue;
                        }
                    };
                    if !tools.contains_key(&id) {
                        buffered_tool_bytes += id.len() + name.len() + 32;
                        if buffered_tool_bytes > max_tool_bytes {
                            return AttemptOutcome::TerminalError(encode_sse_error(
                                "kiro_integrity_buffer_exceeded",
                                "Kiro buffered tool input exceeded the integrity memory bound",
                                None,
                            ));
                        }
                        tool_order.push(id.clone());
                        tools.insert(
                            id.clone(),
                            ToolEntry {
                                id: id.clone(),
                                name: name.to_string(),
                                input: None,
                            },
                        );
                    } else if tools.get(&id).is_some_and(|t| t.name != name) {
                        tool_validation_error.get_or_insert_with(|| {
                            "Kiro tool name changed between fragments".into()
                        });
                        continue;
                    }
                    if let Some(input_val) = val.get("input") {
                        if let Some(entry) = tools.get_mut(&id) {
                            match input_val {
                                Value::String(s) => match &mut entry.input {
                                    None => {
                                        buffered_tool_bytes += s.len();
                                        entry.input = Some(ToolInput::StringChunks(vec![s.clone()]));
                                    }
                                    Some(ToolInput::StringChunks(chunks)) => {
                                        buffered_tool_bytes += s.len();
                                        chunks.push(s.clone());
                                    }
                                    Some(ToolInput::Object(_, _)) => {
                                        tool_validation_error.get_or_insert_with(|| {
                                            "Kiro tool input changed fragment type".into()
                                        });
                                    }
                                },
                                Value::Object(_) => {
                                    let serialized_len = input_val.to_string().len();
                                    match &mut entry.input {
                                        None => {
                                            buffered_tool_bytes += serialized_len;
                                            entry.input = Some(ToolInput::Object(
                                                input_val.clone(),
                                                serialized_len,
                                            ));
                                        }
                                        Some(ToolInput::Object(obj, old_len)) => {
                                            buffered_tool_bytes =
                                                buffered_tool_bytes.saturating_sub(*old_len)
                                                    + serialized_len;
                                            *obj = input_val.clone();
                                            *old_len = serialized_len;
                                        }
                                        Some(ToolInput::StringChunks(_)) => {
                                            tool_validation_error.get_or_insert_with(|| {
                                                "Kiro tool input changed fragment type".into()
                                            });
                                        }
                                    }
                                }
                                _ => {
                                    tool_validation_error.get_or_insert_with(|| {
                                        "Kiro tool input must be a JSON object".into()
                                    });
                                }
                            }
                        }
                        if buffered_tool_bytes > max_tool_bytes {
                            return AttemptOutcome::TerminalError(encode_sse_error(
                                "kiro_integrity_buffer_exceeded",
                                "Kiro buffered tool input exceeded the integrity memory bound",
                                None,
                            ));
                        }
                    }
                }
            }
            "messageStopEvent" => {
                explicit_stop = true;
                let reason = normalize_stop_reason(
                    payload
                        .get("stopReason")
                        .or_else(|| payload.get("stop_reason")),
                )
                .unwrap_or_else(|| {
                    if saw_tool_use {
                        "tool_use".into()
                    } else {
                        "end_turn".into()
                    }
                });
                let merged = merge_stop_reason(stop_reason.clone(), Some(reason));
                if merged != stop_reason {
                    terminal_provenance = Some("message_stop_event");
                }
                stop_reason = merged;
            }
            "metadataEvent" | "MetadataEvent" => {
                let metadata = payload
                    .get("metadataEvent")
                    .or_else(|| payload.get("metadata"))
                    .unwrap_or(&payload);
                if let Some(reason) = normalize_stop_reason(
                    metadata
                        .get("stopReason")
                        .or_else(|| metadata.get("stop_reason")),
                ) {
                    explicit_stop = true;
                    let merged = merge_stop_reason(stop_reason.clone(), Some(reason));
                    if merged != stop_reason {
                        terminal_provenance = Some("metadata_stop_reason");
                    }
                    stop_reason = merged;
                }
            }
            "contextUsageEvent" => {
                if let Some(pct) = payload
                    .get("contextUsagePercentage")
                    .and_then(Value::as_f64)
                    .filter(|n| n.is_finite())
                {
                    context_usage_percentage = pct;
                    has_context_usage = true;
                }
            }
            "meteringEvent" => {
                has_metering = true;
                let metering = payload.get("meteringEvent").unwrap_or(&payload);
                if let Some(credits) = metering
                    .get("usage")
                    .and_then(Value::as_f64)
                    .filter(|n| n.is_finite())
                {
                    let unit = metering
                        .get("unit")
                        .and_then(Value::as_str)
                        .unwrap_or("credit");
                    let u = usage.get_or_insert_with(Map::new);
                    u.insert("kiro_credits".into(), Value::from(credits));
                    u.insert("kiro_credit_unit".into(), Value::String(unit.into()));
                }
            }
            "metricsEvent" => {
                let metrics = payload.get("metricsEvent").unwrap_or(&payload);
                let prompt = metrics
                    .get("inputTokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let completion = metrics
                    .get("outputTokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                if prompt > 0 || completion > 0 {
                    let u = usage.get_or_insert_with(Map::new);
                    u.insert("prompt_tokens".into(), Value::from(prompt));
                    u.insert("completion_tokens".into(), Value::from(completion));
                    u.insert("total_tokens".into(), Value::from(prompt + completion));
                    let cache_read = metrics
                        .get("cacheReadInputTokens")
                        .or_else(|| metrics.get("cache_read_input_tokens"))
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    let cache_create = metrics
                        .get("cacheCreationInputTokens")
                        .or_else(|| metrics.get("cache_creation_input_tokens"))
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    if cache_read > 0 {
                        u.insert("cache_read_input_tokens".into(), Value::from(cache_read));
                    }
                    if cache_create > 0 {
                        u.insert(
                            "cache_creation_input_tokens".into(),
                            Value::from(cache_create),
                        );
                    }
                }
            }
            _ => {}
        }
    }

    if offset < bytes.len() {
        return AttemptOutcome::TerminalError(encode_sse_error(
            "kiro_missing_terminal",
            "Kiro EventStream ended with a truncated frame",
            None,
        ));
    }

    let declared_disposition = stop_disposition(stop_reason.as_deref(), saw_tool_use);
    let declared_truncated_after_output = declared_disposition == "terminal_incomplete"
        && is_truncation_stop_reason(stop_reason.as_deref())
        && chunk_index > 0;
    if !declared_truncated_after_output
        && matches!(
            declared_disposition,
            "retryable_protocol_failure"
                | "terminal_incomplete"
                | "terminal_refusal"
                | "unknown_failure"
        )
    {
        if declared_disposition == "retryable_protocol_failure" {
            return AttemptOutcome::Retryable {
                kind: "retryable_stop",
                message: format!(
                    "Kiro ended with non-success stop reason: {}",
                    stop_reason.as_deref().unwrap_or("")
                ),
            };
        }
        let code = match declared_disposition {
            "terminal_refusal" => "kiro_terminal_refusal",
            "terminal_incomplete" => "kiro_terminal_incomplete",
            _ => "kiro_unknown_stop_reason",
        };
        return AttemptOutcome::TerminalError(encode_sse_error(
            code,
            &format!(
                "Kiro ended with non-success stop reason: {}",
                stop_reason.as_deref().unwrap_or("")
            ),
            None,
        ));
    }

    for id in tool_order {
        let Some(tool) = tools.remove(&id) else {
            continue;
        };
        let parsed = match tool.input {
            None => Err("Kiro tool call is missing input".to_string()),
            Some(ToolInput::Object(obj, _)) => Ok(obj),
            Some(ToolInput::StringChunks(chunks)) => {
                let joined = chunks.join("");
                match serde_json::from_str::<Value>(&joined) {
                    Ok(v) if v.is_object() => Ok(v),
                    Ok(_) => Err("Kiro tool input must be valid object JSON (not an object)".into()),
                    Err(e) => Err(format!("Kiro tool input must be valid object JSON ({e})")),
                }
            }
        };
        let input = match parsed {
            Ok(v) => {
                if tool.name == "tool_call" {
                    let valid_name = v
                        .get("name")
                        .and_then(Value::as_str)
                        .is_some_and(|s| !s.trim().is_empty());
                    let has_args = v.get("arguments").is_some();
                    if !valid_name {
                        tool_validation_error.get_or_insert_with(|| {
                            "Invalid Kiro tool_call payload: missing nested MCP tool name".into()
                        });
                        continue;
                    }
                    if !has_args {
                        tool_validation_error.get_or_insert_with(|| {
                            "Invalid Kiro tool_call payload: missing nested MCP tool arguments"
                                .into()
                        });
                        continue;
                    }
                }
                v
            }
            Err(err) => {
                tool_validation_error.get_or_insert(err);
                continue;
            }
        };

        let index = tool_counter;
        tool_counter += 1;
        let mut d1 = Map::new();
        d1.insert(
            "tool_calls".into(),
            json!([{
                "index": index,
                "id": tool.id,
                "type": "function",
                "function": { "name": tool.name, "arguments": "" }
            }]),
        );
        emit_delta(&mut out, &mut chunk_index, d1);

        let serialized_input = input.to_string();
        let mut d2 = Map::new();
        d2.insert(
            "tool_calls".into(),
            json!([{
                "index": index,
                "function": { "arguments": serialized_input }
            }]),
        );
        emit_delta(&mut out, &mut chunk_index, d2);
        total_content_length += tool.name.len() + serialized_input.len();
        has_tool_calls = true;
    }

    if (stop_reason.as_deref() == Some("tool_use") || tool_validation_error.is_some())
        && !has_tool_calls
        && !has_text
        && !has_reasoning
        && !has_code
    {
        return AttemptOutcome::Retryable {
            kind: "invalid_tool",
            message: tool_validation_error.unwrap_or_else(|| {
                "Kiro tool_use stop reason did not include a complete tool call".into()
            }),
        };
    }

    let has_output = has_text || has_reasoning || has_code || has_tool_calls;
    if !has_output && !explicit_stop {
        return AttemptOutcome::TerminalError(encode_sse_error(
            "kiro_missing_terminal",
            "Kiro EventStream ended without model output",
            None,
        ));
    }

    let disposition = stop_disposition(stop_reason.as_deref(), has_tool_calls);
    let truncated_after_output = disposition == "terminal_incomplete"
        && is_truncation_stop_reason(stop_reason.as_deref())
        && chunk_index > 0;
    if !truncated_after_output
        && matches!(
            disposition,
            "retryable_protocol_failure"
                | "terminal_incomplete"
                | "terminal_refusal"
                | "unknown_failure"
        )
    {
        if disposition == "retryable_protocol_failure" {
            return AttemptOutcome::Retryable {
                kind: "retryable_stop",
                message: format!(
                    "Kiro ended with non-success stop reason: {}",
                    stop_reason.as_deref().unwrap_or("")
                ),
            };
        }
        let code = match disposition {
            "terminal_refusal" => "kiro_terminal_refusal",
            "terminal_incomplete" => "kiro_terminal_incomplete",
            _ => "kiro_unknown_stop_reason",
        };
        return AttemptOutcome::TerminalError(encode_sse_error(
            code,
            &format!(
                "Kiro ended with non-success stop reason: {}",
                stop_reason.as_deref().unwrap_or("")
            ),
            None,
        ));
    }

    if !has_tool_calls {
        if is_ellipsis_only(&accumulated_content)
            || (accumulated_content.trim().is_empty() && is_ellipsis_only(&accumulated_reasoning))
        {
            return AttemptOutcome::Retryable {
                kind: "ellipsis",
                message: "Kiro response ended with only an ellipsis".into(),
            };
        }
        if is_short_future_action(&accumulated_content) {
            return AttemptOutcome::Retryable {
                kind: "short_final",
                message: "Kiro response only announced a future action".into(),
            };
        }
    }

    let _ = terminal_provenance;

    if has_metering
        && has_context_usage
        && usage
            .as_ref()
            .and_then(|u| u.get("total_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
            == 0
    {
        let completion = if total_content_length > 0 {
            (total_content_length / 4).max(1) as u64
        } else {
            0
        };
        let prompt = ((context_usage_percentage * DEFAULT_CONTEXT_WINDOW as f64) / 100.0) as u64;
        let u = usage.get_or_insert_with(Map::new);
        u.insert("prompt_tokens".into(), Value::from(prompt));
        u.insert("completion_tokens".into(), Value::from(completion));
        u.insert("total_tokens".into(), Value::from(prompt + completion));
    }

    let finish_reason = if truncated_after_output {
        "length"
    } else if has_tool_calls {
        "tool_calls"
    } else if disposition == "length" {
        "length"
    } else {
        "stop"
    };

    let usage_val = usage.map(Value::Object);
    out.extend_from_slice(&sse_chunk(
        &response_id,
        created,
        model,
        json!({}),
        Some(finish_reason),
        usage_val.as_ref(),
    ));
    out.extend_from_slice(b"data: [DONE]\n\n");
    AttemptOutcome::Complete(out)
}

async fn send_upstream_with_fallback(
    client: &reqwest::Client,
    request: &super::ExecutorRequest,
    body: &Value,
) -> anyhow::Result<reqwest::Response> {
    let urls = ordered_base_urls(
        request.base_url.as_deref(),
        request.provider_specific_data.as_ref(),
    );
    let mut last_err: Option<anyhow::Error> = None;

    for (idx, url) in urls.iter().enumerate() {
        let has_next = idx + 1 < urls.len();
        let payload = serde_json::to_vec(body)?;
        let headers = build_headers(request, url);
        let mut builder = client.post(url).body(payload);
        for (name, value) in headers {
            builder = builder.header(name, value);
        }
        match builder.send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                if has_next && should_fallback_status(status) {
                    continue;
                }
                return Ok(resp);
            }
            Err(err) => {
                if has_next {
                    last_err = Some(err.into());
                    continue;
                }
                return Err(err.into());
            }
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("kiro: no upstream URLs configured")))
}

fn sse_response(status: u16, bytes: Vec<u8>) -> anyhow::Result<reqwest::Response> {
    let resp = HttpResponse::builder()
        .status(status)
        .header("content-type", "text/event-stream; charset=utf-8")
        .header("cache-control", "no-cache")
        .body(bytes)?;
    Ok(reqwest::Response::from(resp))
}

pub(super) async fn execute(
    client: &reqwest::Client,
    request: &super::ExecutorRequest,
) -> anyhow::Result<reqwest::Response> {
    let first_resp = send_upstream_with_fallback(client, request, &request.body).await?;
    let status = first_resp.status().as_u16();
    if !first_resp.status().is_success() {
        return Ok(first_resp);
    }

    let repair_enabled = request
        .provider_specific_data
        .as_ref()
        .and_then(|v| v.get("kiroToolCallRepair"))
        .and_then(Value::as_bool)
        .unwrap_or(true)
        && std::env::var("KIRO_TOOL_CALL_REPAIR").as_deref() != Ok("false");

    let max_tool_bytes = KIRO_REPAIR_BUFFER_MAX_BYTES / 2;
    let first_bytes = first_resp.bytes().await?;
    match transform_eventstream_to_sse(&first_bytes, &request.model, max_tool_bytes) {
        AttemptOutcome::Complete(bytes) => sse_response(status, bytes),
        AttemptOutcome::TerminalError(bytes) => sse_response(status, bytes),
        AttemptOutcome::Retryable { kind, message } => {
            if kind == "invalid_tool" && !repair_enabled {
                return sse_response(
                    status,
                    encode_sse_error("invalid_kiro_tool_call", &message, None),
                );
            }
            let repair_kind = match kind {
                "invalid_tool" => "tool",
                other => other,
            };
            let repaired_body = append_repair_instruction(&request.body, repair_kind);
            let retry_resp = send_upstream_with_fallback(client, request, &repaired_body).await?;
            if !retry_resp.status().is_success() {
                let retry_status = retry_resp.status().as_u16();
                let err_body = retry_resp.text().await.unwrap_or_default();
                let msg = if err_body.is_empty() {
                    format!("Kiro integrity retry failed with HTTP {retry_status}")
                } else {
                    err_body
                };
                return sse_response(
                    status,
                    encode_sse_error("kiro_integrity_retry_upstream_error", &msg, None),
                );
            }
            let retry_bytes = retry_resp.bytes().await?;
            match transform_eventstream_to_sse(&retry_bytes, &request.model, max_tool_bytes) {
                AttemptOutcome::Complete(bytes) => sse_response(status, bytes),
                AttemptOutcome::TerminalError(bytes) => sse_response(status, bytes),
                AttemptOutcome::Retryable { kind: k2, message: m2 } => {
                    let code = match k2 {
                        "ellipsis" => "kiro_ellipsis_retry_failed",
                        "short_final" => "kiro_short_final_retry_failed",
                        "invalid_tool" => "kiro_tool_call_repair_retry_failed",
                        _ => "kiro_missing_terminal_retry_failed",
                    };
                    sse_response(
                        status,
                        encode_sse_error(
                            code,
                            &format!("Kiro integrity validation failed after one bounded retry: {m2}"),
                            None,
                        ),
                    )
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, routing::post};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::net::TcpListener;

    fn encode_event_frame(event_type: &str, payload: &Value) -> Vec<u8> {
        let mut headers = Vec::new();
        for (k, v) in [(":message-type", "event"), (":event-type", event_type)] {
            headers.push(k.len() as u8);
            headers.extend_from_slice(k.as_bytes());
            headers.push(7); // string
            headers.extend_from_slice(&(v.len() as u16).to_be_bytes());
            headers.extend_from_slice(v.as_bytes());
        }
        let payload_bytes = serde_json::to_vec(payload).unwrap();
        let total_len = (12 + headers.len() + payload_bytes.len() + 4) as u32;
        let headers_len = headers.len() as u32;
        let mut frame = Vec::with_capacity(total_len as usize);
        frame.extend_from_slice(&total_len.to_be_bytes());
        frame.extend_from_slice(&headers_len.to_be_bytes());
        let prelude_crc = crc32(&frame[..8]);
        frame.extend_from_slice(&prelude_crc.to_be_bytes());
        frame.extend_from_slice(&headers);
        frame.extend_from_slice(&payload_bytes);
        let msg_crc = crc32(&frame);
        frame.extend_from_slice(&msg_crc.to_be_bytes());
        frame
    }

    #[test]
    fn orders_urls_and_builds_auth_headers() {
        let urls = ordered_base_urls(None, Some(&json!({ "region": "eu-west-1" })));
        assert_eq!(
            urls[0],
            "https://q.eu-west-1.amazonaws.com/generateAssistantResponse"
        );
        assert_eq!(
            urls[1],
            "https://codewhisperer.eu-west-1.amazonaws.com/generateAssistantResponse"
        );
        assert_eq!(
            urls[2],
            "https://runtime.us-east-1.kiro.dev/generateAssistantResponse"
        );

        let req = super::super::ExecutorRequest {
            provider: "kiro".into(),
            model: "claude-sonnet-4.5".into(),
            access_token: Some("tok-123".into()),
            provider_specific_data: Some(json!({
                "authMethod": "api_key",
                "profileArn": "arn:aws:codewhisperer:us-east-1:123:profile/abc"
            })),
            ..Default::default()
        };
        let cw_headers = build_headers(&req, &urls[1]);
        assert!(cw_headers
            .iter()
            .any(|(k, v)| k == "X-Amz-Target" && v == KIRO_CODEWHISPERER_TARGET));
        assert!(cw_headers
            .iter()
            .any(|(k, v)| k == "TokenType" && v == "API_KEY"));
        assert!(cw_headers
            .iter()
            .any(|(k, v)| k == "Authorization" && v == "Bearer tok-123"));
    }

    #[tokio::test]
    async fn transforms_eventstream_and_retries_ellipsis_once() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_clone = calls.clone();
        let app = Router::new().route(
            "/generateAssistantResponse",
            post(move |body: axum::Json<Value>| {
                let calls = calls_clone.clone();
                async move {
                    let n = calls.fetch_add(1, Ordering::SeqCst);
                    if n == 0 {
                        let mut frame = encode_event_frame(
                            "assistantResponseEvent",
                            &json!({ "content": "..." }),
                        );
                        frame.extend_from_slice(&encode_event_frame(
                            "messageStopEvent",
                            &json!({ "stopReason": "end_turn" }),
                        ));
                        frame
                    } else {
                        let msg = body["conversationState"]["currentMessage"]["userInputMessage"]
                            ["content"]
                            .as_str()
                            .unwrap_or("");
                        assert!(msg.contains("ended with only an ellipsis"));
                        let mut frame = encode_event_frame(
                            "assistantResponseEvent",
                            &json!({ "content": "<thinking>plan</thinking>\nFinal answer." }),
                        );
                        frame.extend_from_slice(&encode_event_frame(
                            "metricsEvent",
                            &json!({ "inputTokens": 10, "outputTokens": 5 }),
                        ));
                        frame.extend_from_slice(&encode_event_frame(
                            "messageStopEvent",
                            &json!({ "stopReason": "end_turn" }),
                        ));
                        frame
                    }
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let client = reqwest::Client::new();
        let req = super::super::ExecutorRequest {
            provider: "kiro".into(),
            model: "claude-sonnet-4.5".into(),
            base_url: Some(format!("http://{addr}/generateAssistantResponse")),
            access_token: Some("tok".into()),
            body: json!({
                "conversationState": {
                    "currentMessage": {
                        "userInputMessage": { "content": "hi" }
                    }
                }
            }),
            ..Default::default()
        };

        let resp = execute(&client, &req).await.unwrap();
        assert_eq!(resp.status(), 200);
        let text = resp.text().await.unwrap();
        assert!(text.contains("Final answer."));
        assert!(!text.contains("<thinking>"));
        assert!(text.contains("data: [DONE]"));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}
