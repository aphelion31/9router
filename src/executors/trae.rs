use anyhow::{Context, bail};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_BASE: &str = "https://core-normal.trae.ai/api/remote/v1";
const TRAE_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/149.0.0.0 Safari/537.36";

fn flatten_query(messages: &[Value]) -> String {
    let mut parts = Vec::new();
    for m in messages {
        let content = match m.get("content") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Array(arr)) => arr
                .iter()
                .filter_map(|p| match p {
                    Value::String(s) => Some(s.as_str()),
                    Value::Object(o) => o.get("text").and_then(Value::as_str),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(""),
            _ => String::new(),
        };
        match m.get("role").and_then(Value::as_str) {
            Some("system") => parts.push(format!("[System]\n{content}")),
            Some("assistant") => parts.push(format!("[Assistant]\n{content}")),
            _ => parts.push(content),
        }
    }
    json!([{"type": "text", "data": {"content": parts.join("\n\n")}}]).to_string()
}

fn resolve_mode(model: &str) -> (&'static str, &'static str, &str) {
    let lower = model.trim().to_ascii_lowercase();
    if matches!(lower.as_str(), "work" | "auto-work" | "solo-work") {
        return ("work", "auto", "");
    }
    if lower.is_empty() || lower == "auto" {
        return ("code", "auto", "");
    }
    ("code", "manual", model)
}

fn common_params(psd: &Value, mode: &str) -> String {
    let get = |k: &str, default: &str| -> String {
        psd.get(k)
            .and_then(Value::as_str)
            .unwrap_or(default)
            .to_string()
    };
    let region = get("region", "US-East");
    let ai_region = psd
        .get("aiRegion")
        .and_then(Value::as_str)
        .unwrap_or(&region);
    json!({
        "language": "en-us",
        "app_language": get("appLanguage", "en"),
        "quality": "stable",
        "app_version": get("appVersion", "1.0.0.1229"),
        "web_id": get("webId", ""),
        "user_identity": get("userIdentity", "Free"),
        "is_freshman": "0",
        "biz_user_id": get("bizUserId", ""),
        "user_unique_id": get("userUniqueId", ""),
        "scope": get("scope", "marscode-us"),
        "tenant": get("tenant", "marscode"),
        "region": region,
        "aiRegion": ai_region,
        "is_privacy_mode": 0,
        "privacy_mode": "off",
        "solo_chat_mode": mode,
    })
    .to_string()
}

pub(super) async fn execute(
    client: &reqwest::Client,
    request: &super::ExecutorRequest,
) -> anyhow::Result<reqwest::Response> {
    let base = request
        .base_url
        .as_deref()
        .unwrap_or(DEFAULT_BASE)
        .trim_end_matches('/');
    let token = request
        .access_token
        .as_deref()
        .or(request.api_key.as_deref())
        .unwrap_or("");
    let psd = request
        .provider_specific_data
        .as_ref()
        .unwrap_or(&Value::Null);
    let app_lang = psd
        .get("appLanguage")
        .and_then(Value::as_str)
        .unwrap_or("en");
    let user_region = psd
        .get("userRegion")
        .and_then(Value::as_str)
        .unwrap_or("US");

    let messages = request
        .body
        .get("messages")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let query = flatten_query(messages);
    let (mode, strategy, model_name) = resolve_mode(&request.model);

    let session_body = json!({
        "mode": mode,
        "environment_id": "default",
        "initial_message": {
            "chat_session_id": "",
            "content": [],
            "query": query,
            "model_name": model_name,
            "agent_type": "solo_agent_remote",
            "model_selection_strategy": strategy,
            "common_params": common_params(psd, mode),
        },
        "env": "remote",
        "auto_create_project": false,
        "origin": "web",
    });

    let create_res = client
        .post(format!("{base}/chat_sessions"))
        .header("Authorization", format!("Cloud-IDE-JWT {token}"))
        .header("Content-Type", "application/json")
        .header("X-Trae-Client-Type", "web")
        .header("X-Preferenced-Language", app_lang)
        .header("x-user-region", user_region)
        .header("Referer", "https://solo.trae.ai/")
        .header("User-Agent", TRAE_UA)
        .header("Accept", "application/json")
        .body(serde_json::to_vec(&session_body)?)
        .send()
        .await?;

    if !create_res.status().is_success() {
        return Ok(create_res);
    }

    let created_bytes = create_res.bytes().await?;
    let created_json: Value =
        serde_json::from_slice(&created_bytes).context("invalid Trae chat_sessions JSON")?;
    if created_json.get("code").and_then(Value::as_i64) != Some(0) {
        bail!("Trae create_session failed: {created_json}");
    }
    let session_id = created_json["data"]["chat_session_id"]
        .as_str()
        .context("missing chat_session_id")?;
    let message_id = created_json["data"]["message_id"]
        .as_str()
        .context("missing message_id")?;

    let mut events_url = reqwest::Url::parse(&format!("{base}/chat_sessions/{session_id}/events"))
        .context("invalid Trae events URL")?;
    events_url
        .query_pairs_mut()
        .append_pair("reply_to_message_id", message_id);

    Ok(client
        .get(events_url)
        .header("Authorization", format!("Cloud-IDE-JWT {token}"))
        .header("Content-Type", "application/json")
        .header("X-Trae-Client-Type", "web")
        .header("X-Preferenced-Language", app_lang)
        .header("x-user-region", user_region)
        .header("Referer", "https://solo.trae.ai/")
        .header("User-Agent", TRAE_UA)
        .header("Accept", "text/event-stream")
        .send()
        .await?)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Default)]
struct TraeState {
    order: Vec<String>,
    thoughts: HashMap<String, String>,
    sent: usize,
    usage: Option<Value>,
    error: Option<Value>,
    done: bool,
}

impl TraeState {
    fn render_new_text(&mut self, data: &Value) -> String {
        let Some(pid) = data.get("id").and_then(Value::as_str) else {
            return String::new();
        };
        if !self.thoughts.contains_key(pid) {
            self.order.push(pid.to_string());
        }
        let t = data.get("thought").and_then(Value::as_str).unwrap_or("");
        let entry = self.thoughts.entry(pid.to_string()).or_default();
        if t.len() >= entry.len() {
            *entry = t.to_string();
        }
        let full: String = self
            .order
            .iter()
            .filter_map(|id| self.thoughts.get(id))
            .cloned()
            .collect();
        let piece = if self.sent < full.len() {
            full[self.sent..].to_string()
        } else {
            String::new()
        };
        self.sent = full.len();
        piece
    }

    fn full_text(&self) -> String {
        self.order
            .iter()
            .filter_map(|id| self.thoughts.get(id))
            .cloned()
            .collect()
    }
}

pub(crate) struct Decoder {
    buffer: String,
    current_event: Option<String>,
    model: String,
    response_id: String,
    created: u64,
    started: bool,
    state: TraeState,
}

impl Decoder {
    pub(crate) fn new(model: &str) -> Self {
        let ts = now_secs();
        Self {
            buffer: String::new(),
            current_event: None,
            model: model.to_string(),
            response_id: format!("chatcmpl-trae-{ts}"),
            created: ts,
            started: false,
            state: TraeState::default(),
        }
    }

    pub(crate) fn push(&mut self, bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
        self.buffer.push_str(&String::from_utf8_lossy(bytes));
        let mut out = Vec::new();
        if !self.started {
            self.started = true;
            let chunk = json!({
                "id": self.response_id,
                "object": "chat.completion.chunk",
                "created": self.created,
                "model": self.model,
                "choices": [{"index": 0, "delta": {"role": "assistant"}, "finish_reason": null}],
            });
            Self::emit_sse(&chunk, &mut out);
        }
        while let Some(pos) = self.buffer.find('\n') {
            let line = self.buffer[..pos].trim_end_matches('\r').to_string();
            self.buffer.drain(..=pos);
            self.process_line(&line, &mut out);
        }
        Ok(out)
    }

    pub(crate) fn finish(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(err) = &self.state.error {
            let code = err.get("code").and_then(Value::as_str).unwrap_or("");
            let msg = err.get("message").and_then(Value::as_str).unwrap_or("");
            let chunk = json!({
                "id": self.response_id,
                "object": "chat.completion.chunk",
                "created": self.created,
                "model": self.model,
                "choices": [],
                "error": {"message": format!("trae {code}: {msg}"), "type": "api_error"},
            });
            Self::emit_sse(&chunk, &mut out);
        } else {
            let stop_chunk = json!({
                "id": self.response_id,
                "object": "chat.completion.chunk",
                "created": self.created,
                "model": self.model,
                "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
            });
            Self::emit_sse(&stop_chunk, &mut out);
            if let Some(usage) = &self.state.usage {
                let usage_chunk = json!({
                    "id": self.response_id,
                    "object": "chat.completion.chunk",
                    "created": self.created,
                    "model": self.model,
                    "choices": [],
                    "usage": {
                        "prompt_tokens": usage.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0),
                        "completion_tokens": usage.get("completion_tokens").and_then(Value::as_u64).unwrap_or(0),
                        "total_tokens": usage.get("total_tokens").and_then(Value::as_u64).unwrap_or(0),
                    },
                });
                Self::emit_sse(&usage_chunk, &mut out);
            }
        }
        out.extend_from_slice(b"data: [DONE]\n\n");
        out
    }

    fn process_line(&mut self, line: &str, out: &mut Vec<u8>) {
        if self.state.done {
            return;
        }
        if let Some(ev) = line.strip_prefix("event:") {
            self.current_event = Some(ev.trim().to_string());
        } else if let Some(payload) = line.strip_prefix("data:") {
            let data: Value = serde_json::from_str(payload.trim())
                .unwrap_or_else(|_| json!({"_raw": payload.trim()}));
            match self.current_event.as_deref() {
                Some("error") => {
                    self.state.error = Some(data);
                    self.state.done = true;
                }
                Some("token_usage") => {
                    self.state.usage = Some(data);
                }
                Some("plan_item") => {
                    let piece = self.state.render_new_text(&data);
                    if !piece.is_empty() {
                        let chunk = json!({
                            "id": self.response_id,
                            "object": "chat.completion.chunk",
                            "created": self.created,
                            "model": self.model,
                            "choices": [{"index": 0, "delta": {"content": piece}, "finish_reason": null}],
                        });
                        Self::emit_sse(&chunk, out);
                    }
                }
                Some("done") => {
                    self.state.done = true;
                }
                _ => {}
            }
        } else if line.is_empty() {
            self.current_event = None;
        }
    }

    fn emit_sse(val: &Value, out: &mut Vec<u8>) {
        if let Ok(s) = serde_json::to_string(val) {
            out.extend_from_slice(b"data: ");
            out.extend_from_slice(s.as_bytes());
            out.extend_from_slice(b"\n\n");
        }
    }
}

pub(crate) fn decode_non_stream(bytes: &[u8], model: &str) -> Vec<u8> {
    let text = String::from_utf8_lossy(bytes);
    let mut state = TraeState::default();
    let mut ev: Option<String> = None;
    for raw_line in text.lines() {
        let line = raw_line.trim_end_matches('\r');
        if let Some(e) = line.strip_prefix("event:") {
            ev = Some(e.trim().to_string());
        } else if let Some(payload) = line.strip_prefix("data:") {
            let data: Value = serde_json::from_str(payload.trim()).unwrap_or(Value::Null);
            match ev.as_deref() {
                Some("error") => {
                    state.error = Some(data);
                    break;
                }
                Some("token_usage") => state.usage = Some(data),
                Some("plan_item") => {
                    let _ = state.render_new_text(&data);
                }
                Some("done") => break,
                _ => {}
            }
        } else if line.is_empty() {
            ev = None;
        }
    }
    if let Some(err) = state.error {
        let code = err.get("code").and_then(Value::as_str).unwrap_or("");
        let msg = err.get("message").and_then(Value::as_str).unwrap_or("");
        return serde_json::to_vec(&json!({
            "error": {"message": format!("trae {code}: {msg}"), "type": "api_error", "code": ""}
        }))
        .unwrap_or_default();
    }
    let ts = now_secs();
    let mut out = json!({
        "id": format!("chatcmpl-trae-{ts}"),
        "object": "chat.completion",
        "created": ts,
        "model": model,
        "choices": [{"index": 0, "message": {"role": "assistant", "content": state.full_text()}, "finish_reason": "stop"}],
    });
    if let Some(usage) = state.usage {
        out["usage"] = json!({
            "prompt_tokens": usage.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0),
            "completion_tokens": usage.get("completion_tokens").and_then(Value::as_u64).unwrap_or(0),
            "total_tokens": usage.get("total_tokens").and_then(Value::as_u64).unwrap_or(0),
        });
    }
    serde_json::to_vec(&out).unwrap_or_default()
}
