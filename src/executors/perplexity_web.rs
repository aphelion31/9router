use anyhow::{Context, bail};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const PPLX_SSE_ENDPOINT: &str = "https://www.perplexity.ai/rest/sse/perplexity_ask";
const PPLX_API_VERSION: &str = "2.18";
const PPLX_UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36";

static SEQ: AtomicU64 = AtomicU64::new(1);

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn new_uuid_v4() -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let h = format!("{ts:016x}{seq:016x}");
    format!(
        "{}-{}-4{}-8{}-{}",
        &h[0..8],
        &h[8..12],
        &h[13..16],
        &h[17..20],
        &h[20..32]
    )
}

pub fn resolve_model(model: &str, thinking: bool) -> (&'static str, String) {
    if thinking {
        match model {
            "pplx-gpt" => return ("copilot", "gpt54_thinking".into()),
            "pplx-sonnet" => return ("copilot", "claude46sonnetthinking".into()),
            "pplx-opus" => return ("copilot", "claude46opusthinking".into()),
            _ => {}
        }
    }
    match model {
        "pplx-auto" => ("concise", "pplx_pro".into()),
        "pplx-sonar" => ("copilot", "experimental".into()),
        "pplx-gpt" => ("copilot", "gpt54".into()),
        "pplx-gemini" => ("copilot", "gemini31pro_high".into()),
        "pplx-sonnet" => ("copilot", "claude46sonnet".into()),
        "pplx-opus" => ("copilot", "claude46opus".into()),
        "pplx-nemotron" => ("copilot", "nv_nemotron_3_super".into()),
        other => ("copilot", other.to_string()),
    }
}

fn parse_openai_messages(messages: &[Value]) -> (String, Vec<Value>, String) {
    let mut system_msg = String::new();
    let mut history = Vec::new();
    for msg in messages {
        let mut role = msg.get("role").and_then(Value::as_str).unwrap_or("user");
        if role == "developer" {
            role = "system";
        }
        let content = match msg.get("content") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Array(arr)) => arr
                .iter()
                .filter(|c| c.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|c| c.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(" "),
            _ => String::new(),
        };
        if content.trim().is_empty() {
            continue;
        }
        if role == "system" {
            system_msg.push_str(&content);
            system_msg.push('\n');
        } else if role == "user" || role == "assistant" {
            history.push(json!({ "role": role, "content": content }));
        }
    }
    let current_msg = if history
        .last()
        .and_then(|m| m.get("role"))
        .and_then(Value::as_str)
        == Some("user")
    {
        history
            .pop()
            .and_then(|m| m.get("content").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_default()
    } else {
        String::new()
    };
    (system_msg, history, current_msg)
}

fn build_query(system_msg: &str, history: &[Value], current_msg: &str, tools: Option<&[Value]>) -> String {
    let mut instr = Vec::new();
    if !system_msg.trim().is_empty() {
        instr.push(system_msg.trim().to_string());
    }
    if let Some(tools) = tools
        && !tools.is_empty()
    {
        let lines: Vec<String> = tools
            .iter()
            .filter_map(|t| {
                let fn_obj = t.get("function").unwrap_or(t);
                let name = fn_obj.get("name").and_then(Value::as_str)?;
                let desc = fn_obj
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .lines()
                    .next()
                    .unwrap_or("");
                Some(format!("- {name}: {desc}"))
            })
            .collect();
        if !lines.is_empty() {
            instr.push(format!(
                "Available tools (reference only, cannot invoke):\n{}",
                lines.join("\n")
            ));
        }
    }
    instr.push("You have built-in web search. Answer questions directly using search results.".into());

    let mut obj = serde_json::Map::new();
    obj.insert("instructions".into(), json!(instr));
    if !history.is_empty() {
        obj.insert("history".into(), Value::Array(history.to_vec()));
    }
    if !current_msg.is_empty() || history.is_empty() {
        obj.insert("query".into(), json!(current_msg));
    }
    let s = Value::Object(obj).to_string();
    if s.len() > 96_000 {
        s[s.len() - 96_000..].to_string()
    } else {
        s
    }
}

pub(crate) struct Decoder {
    id: String,
    created: u64,
    model: String,
    buf: String,
    data_lines: Vec<String>,
    emitted_role: bool,
    seen_len: usize,
    full_answer: String,
    finished: bool,
}

impl Decoder {
    pub fn new(model: &str) -> Self {
        let created = unix_secs();
        Self {
            id: format!("chatcmpl-pplx-{}", &new_uuid_v4()[..8]),
            created,
            model: if model.is_empty() {
                "perplexity-web".into()
            } else {
                model.to_string()
            },
            buf: String::new(),
            data_lines: Vec::new(),
            emitted_role: false,
            seen_len: 0,
            full_answer: String::new(),
            finished: false,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
        self.buf.push_str(&String::from_utf8_lossy(bytes));
        let mut out = Vec::new();
        while let Some(nl) = self.buf.find('\n') {
            let line = self.buf[..nl].trim_end_matches('\r').to_string();
            self.buf.drain(..=nl);
            if line.is_empty() {
                self.flush_event(&mut out);
            } else if let Some(rest) = line.strip_prefix("data:") {
                self.data_lines.push(rest.trim_start().to_string());
            } else if line == "event: end_of_stream" {
                self.flush_event(&mut out);
                self.finish_stream(&mut out);
            }
        }
        Ok(out)
    }

    pub fn finish(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        let rem = std::mem::take(&mut self.buf);
        if let Some(rest) = rem.trim().strip_prefix("data:") {
            self.data_lines.push(rest.trim_start().to_string());
        }
        self.flush_event(&mut out);
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

    fn flush_event(&mut self, out: &mut Vec<u8>) {
        if self.finished || self.data_lines.is_empty() {
            self.data_lines.clear();
            return;
        }
        let payload = self.data_lines.join("\n");
        self.data_lines.clear();
        let trimmed = payload.trim();
        if trimmed.is_empty() || trimmed == "[DONE]" {
            self.finish_stream(out);
            return;
        }
        let Ok(event) = serde_json::from_str::<Value>(trimmed) else {
            return;
        };
        if let Some(err_msg) = event.get("error_message").and_then(Value::as_str) {
            self.ensure_role(out);
            self.emit_chunk(json!({"content": format!("[Error: {err_msg}]")}), None, out);
            self.finish_stream(out);
            return;
        }

        if let Some(blocks) = event.get("blocks").and_then(Value::as_array) {
            for block in blocks {
                let usage = block
                    .get("intended_usage")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if !usage.contains("markdown") {
                    continue;
                }
                if let Some(chunks) = block
                    .pointer("/markdown_block/chunks")
                    .and_then(Value::as_array)
                {
                    let joined: String = chunks.iter().filter_map(Value::as_str).collect();
                    if joined.is_empty() {
                        continue;
                    }
                    let progress = block
                        .pointer("/markdown_block/progress")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    if progress == "DONE" {
                        self.full_answer = joined;
                    } else {
                        let cumulative = format!("{}{}", self.full_answer, joined);
                        if cumulative.len() > self.seen_len {
                            let delta = &cumulative[self.seen_len..];
                            self.ensure_role(out);
                            self.emit_chunk(json!({"content": delta}), None, out);
                            self.full_answer = cumulative;
                            self.seen_len = self.full_answer.len();
                        }
                    }
                }
            }
        } else if let Some(text) = event.get("text").and_then(Value::as_str) {
            let t = text.trim();
            if t.len() > self.seen_len {
                let delta = &t[self.seen_len..];
                self.ensure_role(out);
                self.emit_chunk(json!({"content": delta}), None, out);
                self.full_answer = t.to_string();
                self.seen_len = t.len();
            }
        }

        if event.get("final").and_then(Value::as_bool) == Some(true)
            || event.get("status").and_then(Value::as_str) == Some("COMPLETED")
        {
            self.finish_stream(out);
        }
    }

    fn finish_stream(&mut self, out: &mut Vec<u8>) {
        if self.finished {
            return;
        }
        self.finished = true;
        self.ensure_role(out);
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
        .context("Missing or empty messages array for perplexity-web")?;

    let thinking = request.body.get("thinking").and_then(Value::as_bool) == Some(true)
        || request
            .body
            .get("reasoning_effort")
            .and_then(Value::as_str)
            .is_some_and(|r| r != "none");

    let (mode, model_pref) = resolve_model(&request.model, thinking);
    let (system_msg, history, current_msg) = parse_openai_messages(messages);
    let tools = request
        .body
        .get("tools")
        .and_then(Value::as_array)
        .map(Vec::as_slice);
    let query = build_query(&system_msg, &history, &current_msg, tools);
    if query.trim().is_empty() {
        bail!("Empty query after processing for perplexity-web");
    }

    let payload = json!({
        "query_str": query,
        "params": {
            "query_str": query,
            "search_focus": "internet",
            "mode": mode,
            "model_preference": model_pref,
            "sources": ["web"],
            "attachments": [],
            "frontend_uuid": new_uuid_v4(),
            "frontend_context_uuid": new_uuid_v4(),
            "version": PPLX_API_VERSION,
            "language": "en-US",
            "timezone": "UTC",
            "search_recency_filter": Value::Null,
            "is_incognito": true,
            "use_schematized_api": true,
            "last_backend_uuid": Value::Null
        }
    });

    let url = request.base_url.as_deref().unwrap_or(PPLX_SSE_ENDPOINT);
    let mut builder = client
        .post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "text/event-stream")
        .header("Origin", "https://www.perplexity.ai")
        .header("Referer", "https://www.perplexity.ai/")
        .header("User-Agent", PPLX_UA)
        .header("X-App-ApiClient", "default")
        .header("X-App-ApiVersion", PPLX_API_VERSION);

    if let Some(tok) = request.access_token.as_deref().filter(|s| !s.is_empty()) {
        builder = builder.header("Authorization", format!("Bearer {tok}"));
    } else if let Some(key) = request.api_key.as_deref().filter(|s| !s.is_empty()) {
        builder = builder.header("Cookie", format!("__Secure-next-auth.session-token={key}"));
    }

    Ok(builder.body(payload.to_string()).send().await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_transforms_markdown_blocks_to_openai_sse() {
        let mut dec = Decoder::new("pplx-sonar");
        let raw = concat!(
            "data: {\"blocks\":[{\"intended_usage\":\"ask_text_markdown\",\"markdown_block\":{\"chunks\":[\"Hello \"]}}]}\n\n",
            "data: {\"blocks\":[{\"intended_usage\":\"ask_text_markdown\",\"markdown_block\":{\"chunks\":[\"Perplexity\"]}}],\"status\":\"COMPLETED\"}\n\n",
        );
        let mut out = String::from_utf8(dec.push(raw.as_bytes()).unwrap()).unwrap();
        out.push_str(&String::from_utf8(dec.finish()).unwrap());
        assert!(out.contains("\"content\":\"Hello \""));
        assert!(out.contains("\"content\":\"Perplexity\""));
        assert!(out.contains("data: [DONE]"));
    }
}
