use anyhow::{Context, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static SEQ: AtomicU64 = AtomicU64::new(1);

fn new_hex_id() -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{ts:016x}{seq:016x}")
}

fn new_uuid_v4() -> String {
    let h = new_hex_id();
    format!("{}-{}-4{}-8{}-{}", &h[0..8], &h[8..12], &h[13..16], &h[17..20], &h[20..32])
}

const GROK_CHAT_API: &str = "https://grok.com/rest/app-chat/conversations/new";
const GROK_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36";

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn resolve_model(model: &str) -> (&'static str, &'static str, bool) {
    match model {
        "grok-3" | "grok-4" | "grok-4.1-fast" => {
            ("grok-4-1-thinking-1129", "MODEL_MODE_FAST", false)
        }
        "grok-3-mini" | "grok-4-mini" | "grok-4.1-mini" => (
            "grok-4-1-thinking-1129",
            "MODEL_MODE_GROK_4_1_MINI_THINKING",
            true,
        ),
        "grok-3-thinking" | "grok-4-thinking" | "grok-4.1-thinking" => {
            ("grok-4-1-thinking-1129", "MODEL_MODE_GROK_4_1_THINKING", true)
        }
        "grok-4-heavy" | "grok-4.1-expert" => {
            ("grok-4-1-thinking-1129", "MODEL_MODE_EXPERT", true)
        }
        "grok-4.2" | "grok-4.20" | "grok-4.20-beta" => {
            ("grok-4-1-thinking-1129", "MODEL_MODE_GROK_420", false)
        }
        _ => ("grok-4-1-thinking-1129", "MODEL_MODE_FAST", false),
    }
}

use crate::services::web_cookie_auth::build_grok_cookie_header;

fn generate_statsig_id() -> String {
    let seed = new_hex_id();
    let msg = format!(
        "x1:TypeError: Cannot read properties of null (reading 'children[\"{}\"]')",
        &seed[..5]
    );
    STANDARD.encode(msg.as_bytes())
}

fn parse_openai_messages(messages: &[Value]) -> String {
    let mut extracted = Vec::new();
    for msg in messages {
        let mut role = msg.get("role").and_then(Value::as_str).unwrap_or("user");
        if role == "developer" {
            role = "system";
        }
        let text = match msg.get("content") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Array(arr)) => arr
                .iter()
                .filter(|c| c.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|c| c.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(" "),
            _ => String::new(),
        };
        if !text.trim().is_empty() {
            extracted.push((role.to_string(), text));
        }
    }
    let last_user_idx = extracted.iter().rposition(|(r, _)| r == "user");
    extracted
        .iter()
        .enumerate()
        .map(|(i, (r, t))| {
            if Some(i) == last_user_idx {
                t.clone()
            } else {
                format!("{r}: {t}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn build_tool_manifest(tools: &[Value]) -> String {
    let mut lines = vec![
        "CLIENT_TOOLS (emit <tool_call>{\"name\":\"...\",\"arguments\":{...}}</tool_call> to invoke):"
            .to_string(),
    ];
    for t in tools {
        if let Some(fn_obj) = t.get("function")
            && let Some(name) = fn_obj.get("name").and_then(Value::as_str)
        {
            let desc = fn_obj
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("");
            lines.push(format!("- {name}: {desc}"));
        }
    }
    lines.join("\n")
}

pub fn parse_tool_call_markup(text: &str) -> Vec<Value> {
    let mut calls = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("<tool_call>") {
        let after = &rest[start + 11..];
        let Some(end) = after.find("</tool_call>") else {
            break;
        };
        let inner = after[..end].trim();
        if let Ok(v) = serde_json::from_str::<Value>(inner)
            && let Some(name) = v.get("name").and_then(Value::as_str)
        {
            let args = v
                .get("arguments")
                .map(|a| {
                    if let Some(s) = a.as_str() {
                        s.to_string()
                    } else {
                        a.to_string()
                    }
                })
                .unwrap_or_else(|| "{}".to_string());
            calls.push(json!({
                "id": format!("call_{}", &new_hex_id()[..12]),
                "type": "function",
                "function": { "name": name, "arguments": args }
            }));
        }
        rest = &after[end + 12..];
    }
    calls
}

pub(crate) struct Decoder {
    id: String,
    created: u64,
    model: String,
    buf: String,
    emitted_role: bool,
    fingerprint: Option<String>,
    pending: String,
    has_tools: bool,
    finished: bool,
}

impl Decoder {
    pub fn new(model: &str, has_tools: bool) -> Self {
        let created = unix_secs();
        Self {
            id: format!("chatcmpl-grok-{}", &new_hex_id()[..12]),
            created,
            model: if model.is_empty() {
                "grok-web".into()
            } else {
                model.to_string()
            },
            buf: String::new(),
            emitted_role: false,
            fingerprint: None,
            pending: String::new(),
            has_tools,
            finished: false,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
        self.buf.push_str(&String::from_utf8_lossy(bytes));
        let mut out = Vec::new();
        while let Some(nl) = self.buf.find('\n') {
            let line = self.buf[..nl].trim().to_string();
            self.buf.drain(..=nl);
            if !line.is_empty() {
                self.process_line(&line, &mut out);
            }
        }
        Ok(out)
    }

    pub fn finish(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        if !self.buf.trim().is_empty() {
            let rem = std::mem::take(&mut self.buf);
            self.process_line(rem.trim(), &mut out);
        }
        if !self.finished {
            self.finish_stream(&mut out);
        }
        out
    }

    fn emit_chunk(&self, delta: Value, finish: Option<&str>, out: &mut Vec<u8>) {
        let chunk = json!({
            "id": self.id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.model,
            "system_fingerprint": self.fingerprint,
            "choices": [{
                "index": 0,
                "delta": delta,
                "finish_reason": finish,
            }]
        });
        out.extend_from_slice(format!("data: {chunk}\n\n").as_bytes());
    }

    fn ensure_role(&mut self, out: &mut Vec<u8>) {
        if !self.emitted_role {
            self.emitted_role = true;
            self.emit_chunk(json!({"role": "assistant"}), None, out);
        }
    }

    fn process_line(&mut self, line: &str, out: &mut Vec<u8>) {
        if self.finished {
            return;
        }
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            return;
        };
        if let Some(err) = event.get("error") {
            let msg = err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Grok error");
            self.ensure_role(out);
            self.emit_chunk(json!({"content": format!("[Error: {msg}]")}), None, out);
            self.finish_stream(out);
            return;
        }
        let Some(resp) = event.pointer("/result/response") else {
            return;
        };
        if let Some(fp) = resp.pointer("/llmInfo/modelHash").and_then(Value::as_str)
            && self.fingerprint.is_none()
        {
            self.fingerprint = Some(fp.to_string());
        }
        if let Some(mr) = resp.get("modelResponse")
            && let Some(full) = mr.get("message").and_then(Value::as_str)
            && self.has_tools
        {
            self.pending = full.to_string();
            return;
        }
        if let Some(token) = resp.get("token").and_then(Value::as_str) {
            self.ensure_role(out);
            if self.has_tools {
                self.pending.push_str(token);
            } else if !token.is_empty() {
                self.emit_chunk(json!({"content": token}), None, out);
            }
        }
    }

    fn finish_stream(&mut self, out: &mut Vec<u8>) {
        if self.finished {
            return;
        }
        self.finished = true;
        self.ensure_role(out);
        if self.has_tools {
            let calls = parse_tool_call_markup(&self.pending);
            if !calls.is_empty() {
                for (i, c) in calls.into_iter().enumerate() {
                    let mut entry = c;
                    entry["index"] = json!(i);
                    self.emit_chunk(json!({"tool_calls": [entry]}), None, out);
                }
                self.emit_chunk(json!({}), Some("tool_calls"), out);
                out.extend_from_slice(b"data: [DONE]\n\n");
                return;
            }
            if !self.pending.is_empty() {
                let text = std::mem::take(&mut self.pending);
                self.emit_chunk(json!({"content": text}), None, out);
            }
        }
        self.emit_chunk(json!({}), Some("stop"), out);
        out.extend_from_slice(b"data: [DONE]\n\n");
    }
}

pub async fn execute(
    client: &reqwest::Client,
    request: &super::ExecutorRequest,
) -> anyhow::Result<reqwest::Response> {
    let messages = request
        .body
        .get("messages")
        .and_then(Value::as_array)
        .filter(|m| !m.is_empty())
        .context("Missing or empty messages array for grok-web")?;

    let (grok_model, model_mode, _is_thinking) = resolve_model(&request.model);
    let mut message = parse_openai_messages(messages);
    if let Some(tools) = request.body.get("tools").and_then(Value::as_array)
        && !tools.is_empty()
    {
        message = format!("{}\n\n{}", build_tool_manifest(tools), message);
    }
    if message.trim().is_empty() {
        bail!("Empty query after processing for grok-web");
    }

    let payload = json!({
        "temporary": true,
        "modelName": grok_model,
        "modelMode": model_mode,
        "message": message,
        "fileAttachments": [],
        "imageAttachments": [],
        "disableSearch": false,
        "enableImageGeneration": false,
        "returnImageBytes": false,
        "returnRawGrokInXaiRequest": false,
        "enableImageStreaming": false,
        "imageGenerationCount": 0,
        "forceConcise": false,
        "toolOverrides": {},
        "enableSideBySide": true,
        "sendFinalMetadata": true,
        "isReasoning": false,
        "disableTextFollowUps": false,
        "disableMemory": true,
        "forceSideBySide": false,
        "isAsyncChat": false,
        "disableSelfHarmShortCircuit": false,
        "deviceEnvInfo": {
            "darkModeEnabled": false,
            "devicePixelRatio": 2,
            "screenWidth": 2056,
            "screenHeight": 1329,
            "viewportWidth": 2056,
            "viewportHeight": 1083
        }
    });

    let url = request.base_url.as_deref().unwrap_or(GROK_CHAT_API);
    let trace_id = new_hex_id();
    let span_id = &new_hex_id()[..16];

    let mut builder = client
        .post(url)
        .header("Accept", "*/*")
        .header("Content-Type", "application/json")
        .header("Origin", "https://grok.com")
        .header("Referer", "https://grok.com/")
        .header("User-Agent", GROK_UA)
        .header("x-statsig-id", generate_statsig_id())
        .header("Browser-Id", new_uuid_v4())
        .header("x-xai-request-id", new_uuid_v4())
        .header("traceparent", format!("00-{trace_id}-{span_id}-00"));

    if let Some(raw_key) = request.api_key.as_deref().or(request.access_token.as_deref()) {
        let cookie = build_grok_cookie_header(raw_key);
        if !cookie.is_empty() {
            builder = builder.header("Cookie", cookie);
        }
    }

    Ok(builder.body(payload.to_string()).send().await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_header_preserves_sso_and_cf_cookies() {
        assert_eq!(build_grok_cookie_header("abc"), "sso=abc");
        assert_eq!(
            build_grok_cookie_header("sso=abc; sso-rw=xyz; cf_clearance=cf1; other=skip"),
            "sso=abc; sso-rw=xyz; cf_clearance=cf1"
        );
    }

    #[test]
    fn decoder_handles_ndjson_tokens_and_tool_call_markup() {
        let mut dec = Decoder::new("grok-4.1-fast", true);
        let ndjson = concat!(
            "{\"result\":{\"response\":{\"token\":\"<tool_call>{\\\"name\\\":\\\"bash\\\",\\\"arguments\\\":{\\\"cmd\\\":\\\"ls\\\"}}</tool_call>\"}}}\n",
        );
        let mut out = String::from_utf8(dec.push(ndjson.as_bytes()).unwrap()).unwrap();
        out.push_str(&String::from_utf8(dec.finish()).unwrap());
        assert!(out.contains("\"name\":\"bash\""));
        assert!(out.contains("\"finish_reason\":\"tool_calls\""));
    }
}
