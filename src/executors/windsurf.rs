use anyhow::Context;
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::{Value, json};
use std::time::{SystemTime, UNIX_EPOCH};

const WS_CHAT_URL: &str =
    "https://server.codeium.com/exa.language_server_pb.LanguageServerService/GetChatMessage";
const WS_IDE_NAME: &str = "windsurf";
const WS_IDE_VERSION: &str = "3.14.0";
const WS_EXT_VERSION: &str = "3.14.0";
const WS_LOCALE: &str = "en-US";

pub fn resolve_ws_model_id(model: &str) -> &str {
    match model {
        "swe-1.6-fast" => "swe-1-6-fast",
        "swe-1.6" => "swe-1-6",
        "swe-1.5-fast" => "swe-1-5-fast",
        "swe-1.5" => "swe-1-5",
        "claude-opus-4.7-max" => "claude-opus-4-7-max",
        "claude-opus-4.7-xhigh" => "claude-opus-4-7-xhigh",
        "claude-opus-4.7-high" => "claude-opus-4-7-high",
        "claude-opus-4.7-medium" => "claude-opus-4-7-medium",
        "claude-opus-4.7-low" => "claude-opus-4-7-low",
        "claude-opus-4.7-review" => "opus-4-7-review",
        "claude-sonnet-4.6-thinking-1m" => "claude-sonnet-4-6-thinking-1m",
        "claude-sonnet-4.6-1m" => "claude-sonnet-4-6-1m",
        "claude-sonnet-4.6-thinking" => "claude-sonnet-4-6-thinking",
        "claude-sonnet-4.6" => "claude-sonnet-4-6",
        "claude-opus-4.6-thinking" => "claude-opus-4-6-thinking",
        "claude-opus-4.6" => "claude-opus-4-6",
        "claude-opus-4.5-thinking" => "MODEL_CLAUDE_4_5_OPUS_THINKING",
        "claude-opus-4.5" => "MODEL_CLAUDE_4_5_OPUS",
        "claude-sonnet-4.5-thinking" => "MODEL_PRIVATE_3",
        "claude-sonnet-4.5" => "MODEL_PRIVATE_2",
        "claude-haiku-4.5" => "MODEL_PRIVATE_11",
        "gpt-5.5-xhigh-fast" => "gpt-5-5-xhigh-priority",
        "gpt-5.5-high-fast" => "gpt-5-5-high-priority",
        "gpt-5.5-medium-fast" => "gpt-5-5-medium-priority",
        "gpt-5.5-low-fast" => "gpt-5-5-low-priority",
        "gpt-5.5-none-fast" => "gpt-5-5-none-priority",
        "gpt-5.5-xhigh" => "gpt-5-5-xhigh",
        "gpt-5.5-high" => "gpt-5-5-high",
        "gpt-5.5-medium" => "gpt-5-5-medium",
        "gpt-5.5-low" => "gpt-5-5-low",
        "gpt-5.5-none" => "gpt-5-5-none",
        "gpt-5.5-review" => "gpt-5-5-review",
        "gpt-5.5" => "gpt-5-5-medium",
        "gpt-5.4-xhigh-fast" => "gpt-5-4-xhigh-priority",
        "gpt-5.4-high-fast" => "gpt-5-4-high-priority",
        "gpt-5.4-medium-fast" => "gpt-5-4-medium-priority",
        "gpt-5.4-low-fast" => "gpt-5-4-low-priority",
        "gpt-5.4-none-fast" => "gpt-5-4-none-priority",
        "gpt-5.4-xhigh" => "gpt-5-4-xhigh",
        "gpt-5.4-high" => "gpt-5-4-high",
        "gpt-5.4-medium" => "gpt-5-4-medium",
        "gpt-5.4-low" => "gpt-5-4-low",
        "gpt-5.4-none" => "gpt-5-4-none",
        "gpt-5.4-mini-xhigh" => "gpt-5-4-mini-xhigh",
        "gpt-5.4-mini-high" => "gpt-5-4-mini-high",
        "gpt-5.4-mini-medium" => "gpt-5-4-mini-medium",
        "gpt-5.4-mini-low" => "gpt-5-4-mini-low",
        "gpt-5.4" => "gpt-5-4-medium",
        "gpt-5.3-codex-xhigh-fast" => "gpt-5-3-codex-xhigh-priority",
        "gpt-5.3-codex-high-fast" => "gpt-5-3-codex-high-priority",
        "gpt-5.3-codex-medium-fast" => "gpt-5-3-codex-medium-priority",
        "gpt-5.3-codex-low-fast" => "gpt-5-3-codex-low-priority",
        "gpt-5.3-codex-xhigh" => "gpt-5-3-codex-xhigh",
        "gpt-5.3-codex-high" => "gpt-5-3-codex-high",
        "gpt-5.3-codex-medium" => "gpt-5-3-codex-medium",
        "gpt-5.3-codex-low" => "gpt-5-3-codex-low",
        "gpt-5.3-codex" => "gpt-5-3-codex-medium",
        "gpt-5.2-xhigh" => "MODEL_GPT_5_2_XHIGH",
        "gpt-5.2-high" => "MODEL_GPT_5_2_HIGH",
        "gpt-5.2-medium" => "MODEL_GPT_5_2_MEDIUM",
        "gpt-5.2-low" => "MODEL_GPT_5_2_LOW",
        "gpt-5.2-none" => "MODEL_GPT_5_2_NONE",
        "gpt-5.2" => "MODEL_GPT_5_2_MEDIUM",
        "gpt-5" => "gpt-5",
        "gpt-4.1" => "MODEL_CHAT_GPT_4_1_2025_04_14",
        "gpt-4.1-mini" => "gpt-4.1-mini",
        "gpt-4o" => "MODEL_CHAT_GPT_4O_2024_08_06",
        "gemini-3.1-pro-high" => "gemini-3-1-pro-high",
        "gemini-3.1-pro-low" => "gemini-3-1-pro-low",
        "gemini-3.1-pro" => "gemini-3-1-pro-high",
        "gemini-3.0-flash-high" => "MODEL_GOOGLE_GEMINI_3_0_FLASH_HIGH",
        "gemini-3.0-flash-medium" => "MODEL_GOOGLE_GEMINI_3_0_FLASH_MEDIUM",
        "gemini-3.0-flash-low" => "MODEL_GOOGLE_GEMINI_3_0_FLASH_LOW",
        "gemini-3.0-flash-minimal" => "MODEL_GOOGLE_GEMINI_3_0_FLASH_MINIMAL",
        "gemini-3.0-flash" => "MODEL_GOOGLE_GEMINI_3_0_FLASH_HIGH",
        "gemini-2.5-pro" => "MODEL_GOOGLE_GEMINI_2_5_PRO",
        "deepseek-v4" => "deepseek-v4",
        "kimi-k2.6" => "kimi-k2-6",
        "kimi-k2.5" => "kimi-k2-5",
        "glm-5.1" => "glm-5-1",
        other => other,
    }
}

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

fn encode_varint(mut v: u64, out: &mut Vec<u8>) {
    while v > 0x7f {
        out.push(((v & 0x7f) as u8) | 0x80);
        v >>= 7;
    }
    out.push((v & 0x7f) as u8);
}

fn encode_field(field_num: u32, payload: &[u8], out: &mut Vec<u8>) {
    encode_varint(((field_num as u64) << 3) | 2, out);
    encode_varint(payload.len() as u64, out);
    out.extend_from_slice(payload);
}

fn encode_string(field_num: u32, s: &str, out: &mut Vec<u8>) {
    encode_field(field_num, s.as_bytes(), out);
}

pub fn build_get_chat_message_request(
    api_key: &str,
    model: &str,
    messages: &[(&str, &str, Option<&str>)],
) -> Vec<u8> {
    let session_id = random_uuid();
    let cascade_id = random_uuid();

    let mut metadata = Vec::new();
    encode_string(1, api_key, &mut metadata);
    encode_string(2, WS_IDE_NAME, &mut metadata);
    encode_string(3, WS_IDE_VERSION, &mut metadata);
    encode_string(4, WS_EXT_VERSION, &mut metadata);
    encode_string(5, &session_id, &mut metadata);
    encode_string(6, WS_LOCALE, &mut metadata);

    let mut model_or_alias = Vec::new();
    encode_string(1, model, &mut model_or_alias);

    let mut out = Vec::new();
    encode_field(1, &metadata, &mut out);
    encode_string(2, &cascade_id, &mut out);
    encode_field(3, &model_or_alias, &mut out);

    for (role, content, tool_call_id) in messages {
        let mut msg = Vec::new();
        encode_string(1, role, &mut msg);
        encode_string(2, content, &mut msg);
        if let Some(id) = tool_call_id {
            encode_string(3, id, &mut msg);
        }
        encode_field(4, &msg, &mut out);
    }
    out
}

pub fn grpc_web_frame(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + payload.len());
    out.push(0x00);
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

fn read_varint(buf: &[u8], offset: &mut usize) -> u64 {
    let mut result = 0u64;
    let mut shift = 0u32;
    while *offset < buf.len() {
        let b = buf[*offset];
        *offset += 1;
        result |= ((b & 0x7f) as u64) << shift;
        if (b & 0x80) == 0 {
            break;
        }
        shift += 7;
        if shift >= 64 {
            break;
        }
    }
    result
}

fn decode_string_field(buf: &[u8], target_field: u32) -> Option<String> {
    let mut offset = 0;
    while offset < buf.len() {
        let tag = read_varint(buf, &mut offset);
        let field_num = (tag >> 3) as u32;
        let wire_type = tag & 0x07;
        match wire_type {
            2 => {
                let len = read_varint(buf, &mut offset) as usize;
                let end = offset.saturating_add(len).min(buf.len());
                let payload = &buf[offset..end];
                offset = end;
                if field_num == target_field {
                    return Some(String::from_utf8_lossy(payload).into_owned());
                }
            }
            0 => {
                let _ = read_varint(buf, &mut offset);
            }
            1 => offset = offset.saturating_add(8),
            5 => offset = offset.saturating_add(4),
            _ => break,
        }
    }
    None
}

fn decode_done_chunk(buf: &[u8]) -> (u64, u64) {
    let mut offset = 0;
    let mut usage_bytes: Option<&[u8]> = None;
    while offset < buf.len() {
        let tag = read_varint(buf, &mut offset);
        let field_num = (tag >> 3) as u32;
        let wire_type = tag & 0x07;
        match wire_type {
            2 => {
                let len = read_varint(buf, &mut offset) as usize;
                let end = offset.saturating_add(len).min(buf.len());
                if field_num == 1 {
                    usage_bytes = Some(&buf[offset..end]);
                }
                offset = end;
            }
            0 => {
                let _ = read_varint(buf, &mut offset);
            }
            _ => break,
        }
    }
    let Some(usage) = usage_bytes else {
        return (0, 0);
    };
    let mut prompt = 0;
    let mut completion = 0;
    let mut uoff = 0;
    while uoff < usage.len() {
        let tag = read_varint(usage, &mut uoff);
        let field_num = (tag >> 3) as u32;
        let wire_type = tag & 0x07;
        match wire_type {
            0 => {
                let v = read_varint(usage, &mut uoff);
                if field_num == 1 {
                    prompt = v;
                } else if field_num == 2 {
                    completion = v;
                }
            }
            2 => {
                let len = read_varint(usage, &mut uoff) as usize;
                uoff = uoff.saturating_add(len).min(usage.len());
            }
            _ => break,
        }
    }
    (prompt, completion)
}

#[derive(Debug, PartialEq, Eq)]
pub enum CompletionChunk {
    Content(String),
    Done { prompt_tokens: u64, completion_tokens: u64 },
    Error(String),
    Unknown,
}

pub fn decode_completion_chunk(buf: &[u8]) -> CompletionChunk {
    let mut offset = 0;
    while offset < buf.len() {
        let tag = read_varint(buf, &mut offset);
        let field_num = (tag >> 3) as u32;
        let wire_type = tag & 0x07;
        match wire_type {
            2 => {
                let len = read_varint(buf, &mut offset) as usize;
                let end = offset.saturating_add(len).min(buf.len());
                let payload = &buf[offset..end];
                offset = end;
                match field_num {
                    1 => {
                        if let Some(text) = decode_string_field(payload, 1) {
                            return CompletionChunk::Content(text);
                        }
                    }
                    3 => {
                        let (prompt_tokens, completion_tokens) = decode_done_chunk(payload);
                        return CompletionChunk::Done {
                            prompt_tokens,
                            completion_tokens,
                        };
                    }
                    4 => {
                        let msg = decode_string_field(payload, 1)
                            .unwrap_or_else(|| "unknown windsurf error".to_string());
                        return CompletionChunk::Error(msg);
                    }
                    _ => {}
                }
            }
            0 => {
                let _ = read_varint(buf, &mut offset);
            }
            1 => offset = offset.saturating_add(8),
            5 => offset = offset.saturating_add(4),
            _ => break,
        }
    }
    CompletionChunk::Unknown
}

fn extract_messages(body: &Value) -> Vec<(String, String, Option<String>)> {
    let mut out = Vec::new();
    if let Some(messages) = body.get("messages").and_then(Value::as_array) {
        for m in messages {
            let role = m
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("user")
                .to_string();
            let content = match m.get("content") {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Array(parts)) => parts
                    .iter()
                    .filter_map(|p| {
                        (p.get("type").and_then(Value::as_str) == Some("text"))
                            .then(|| p.get("text").and_then(Value::as_str).unwrap_or(""))
                    })
                    .collect::<Vec<_>>()
                    .join(""),
                _ => String::new(),
            };
            let tool_call_id = m
                .get("tool_call_id")
                .and_then(Value::as_str)
                .map(str::to_owned);
            out.push((role, content, tool_call_id));
        }
    }
    if out.is_empty() {
        out.push(("user".to_string(), String::new(), None));
    }
    out
}

pub(super) async fn execute(
    client: &reqwest::Client,
    request: &super::ExecutorRequest,
) -> anyhow::Result<reqwest::Response> {
    let url = request
        .base_url
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(WS_CHAT_URL);
    let parsed = reqwest::Url::parse(url).context("invalid Windsurf URL")?;
    if !matches!(parsed.scheme(), "http" | "https") {
        anyhow::bail!("upstream URL must use HTTP or HTTPS");
    }

    let api_key = request
        .access_token
        .as_deref()
        .or(request.api_key.as_deref())
        .unwrap_or("");
    let ws_model = resolve_ws_model_id(&request.model);
    let msgs = extract_messages(&request.body);
    let refs: Vec<(&str, &str, Option<&str>)> = msgs
        .iter()
        .map(|(r, c, t)| (r.as_str(), c.as_str(), t.as_deref()))
        .collect();
    let proto = build_get_chat_message_request(api_key, ws_model, &refs);
    let framed = grpc_web_frame(&proto);

    let mut builder = client
        .post(parsed)
        .header(reqwest::header::CONTENT_TYPE, "application/grpc-web+proto")
        .header(reqwest::header::ACCEPT, "application/grpc-web+proto")
        .header(
            reqwest::header::USER_AGENT,
            format!("windsurf/{WS_IDE_VERSION}"),
        )
        .header("x-grpc-web", "1")
        .body(framed);
    if !api_key.is_empty() {
        builder = builder.header(reqwest::header::AUTHORIZATION, format!("Bearer {api_key}"));
    }
    super::send_request(client, builder.build()?, request.provider_specific_data.as_ref()).await
}

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub(crate) struct Decoder {
    response_id: String,
    created: u64,
    model: String,
    buf: Vec<u8>,
    role_emitted: bool,
    prompt_tokens: u64,
    completion_tokens: u64,
    error: Option<String>,
    finished: bool,
}

impl Decoder {
    pub fn new(model: &str) -> Self {
        let created = unix_secs();
        Self {
            response_id: format!("chatcmpl-ws-{created}"),
            created,
            model: model.to_string(),
            buf: Vec::new(),
            role_emitted: false,
            prompt_tokens: 0,
            completion_tokens: 0,
            error: None,
            finished: false,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        let mut offset = 0;
        while offset + 5 <= self.buf.len() {
            let flag = self.buf[offset];
            let len = u32::from_be_bytes([
                self.buf[offset + 1],
                self.buf[offset + 2],
                self.buf[offset + 3],
                self.buf[offset + 4],
            ]) as usize;
            if offset + 5 + len > self.buf.len() {
                break;
            }
            let payload = self.buf[offset + 5..offset + 5 + len].to_vec();
            self.handle_frame(flag, &payload, &mut out);
            offset += 5 + len;
        }
        if offset > 0 {
            self.buf.drain(..offset);
        }
        Ok(out)
    }

    fn handle_frame(&mut self, flag: u8, payload: &[u8], out: &mut Vec<u8>) {
        if flag == 0x80 {
            let trailer = String::from_utf8_lossy(payload);
            for line in trailer.lines() {
                let lower = line.to_ascii_lowercase();
                if let Some(rest) = lower.strip_prefix("grpc-status:") {
                    if rest.trim() != "0" && self.error.is_none() {
                        self.error = Some(format!("gRPC status {}", rest.trim()));
                    }
                } else if let Some(idx) = lower.find("grpc-message:") {
                    let msg = line[idx + "grpc-message:".len()..].trim();
                    if !msg.is_empty() {
                        self.error = Some(msg.to_string());
                    }
                }
            }
            return;
        }
        if flag != 0x00 {
            return;
        }
        match decode_completion_chunk(payload) {
            CompletionChunk::Content(text) if !text.is_empty() => {
                if !self.role_emitted {
                    let chunk = json!({
                        "id": self.response_id,
                        "object": "chat.completion.chunk",
                        "created": self.created,
                        "model": self.model,
                        "choices": [{"index": 0, "delta": {"role": "assistant", "content": ""}, "finish_reason": null}]
                    });
                    out.extend_from_slice(format!("data: {chunk}\n\n").as_bytes());
                    self.role_emitted = true;
                }
                let chunk = json!({
                    "id": self.response_id,
                    "object": "chat.completion.chunk",
                    "created": self.created,
                    "model": self.model,
                    "choices": [{"index": 0, "delta": {"content": text}, "finish_reason": null}]
                });
                out.extend_from_slice(format!("data: {chunk}\n\n").as_bytes());
            }
            CompletionChunk::Done {
                prompt_tokens,
                completion_tokens,
            } => {
                self.prompt_tokens = prompt_tokens;
                self.completion_tokens = completion_tokens;
            }
            CompletionChunk::Error(msg) => {
                self.error = Some(msg);
            }
            _ => {}
        }
    }

    pub fn finish(&mut self) -> Vec<u8> {
        if self.finished {
            return Vec::new();
        }
        self.finished = true;
        let mut out = Vec::new();
        if let Some(err) = &self.error {
            let payload = json!({
                "error": {"message": err, "type": "windsurf_error", "code": "upstream_error"}
            });
            out.extend_from_slice(format!("data: {payload}\n\n").as_bytes());
            out.extend_from_slice(b"data: [DONE]\n\n");
            return out;
        }
        let mut finish_payload = json!({
            "id": self.response_id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.model,
            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]
        });
        if self.prompt_tokens > 0 || self.completion_tokens > 0 {
            finish_payload["usage"] = json!({
                "prompt_tokens": self.prompt_tokens,
                "completion_tokens": self.completion_tokens,
                "total_tokens": self.prompt_tokens + self.completion_tokens
            });
        }
        out.extend_from_slice(format!("data: {finish_payload}\n\n").as_bytes());
        out.extend_from_slice(b"data: [DONE]\n\n");
        out
    }
}

pub(crate) fn decode_non_stream(raw: &[u8], model: &str) -> Vec<u8> {
    let created = unix_secs();
    let mut content = String::new();
    let mut prompt_tokens = 0u64;
    let mut completion_tokens = 0u64;
    let mut offset = 0;
    while offset + 5 <= raw.len() {
        let flag = raw[offset];
        let len = u32::from_be_bytes([
            raw[offset + 1],
            raw[offset + 2],
            raw[offset + 3],
            raw[offset + 4],
        ]) as usize;
        if offset + 5 + len > raw.len() {
            break;
        }
        if flag == 0x00 {
            match decode_completion_chunk(&raw[offset + 5..offset + 5 + len]) {
                CompletionChunk::Content(text) => content.push_str(&text),
                CompletionChunk::Done {
                    prompt_tokens: p,
                    completion_tokens: c,
                } => {
                    prompt_tokens = p;
                    completion_tokens = c;
                }
                CompletionChunk::Error(msg) => {
                    return serde_json::to_vec(&json!({
                        "error": {"message": msg, "type": "windsurf_error", "code": "upstream_error"}
                    }))
                    .unwrap_or_default();
                }
                CompletionChunk::Unknown => {}
            }
        }
        offset += 5 + len;
    }
    let mut body = json!({
        "id": format!("chatcmpl-ws-{created}"),
        "object": "chat.completion",
        "created": created,
        "model": model,
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": content},
            "finish_reason": "stop"
        }]
    });
    if prompt_tokens > 0 || completion_tokens > 0 {
        body["usage"] = json!({
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "total_tokens": prompt_tokens + completion_tokens
        });
    }
    serde_json::to_vec(&body).unwrap_or_default()
}
