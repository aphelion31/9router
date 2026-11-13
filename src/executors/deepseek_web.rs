use anyhow::{Context, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::time::{SystemTime, UNIX_EPOCH};
use crate::executors::deepseek_web_done_terminator::{DrainWindow, FinishOnce};

const DEEPSEEK_WEB_BASE: &str = "https://chat.deepseek.com";
const MAX_POW_DIFFICULTY: u64 = 250_000;

const KECCAK_PI: [usize; 24] = [
    10, 7, 11, 17, 18, 3, 5, 16, 8, 21, 24, 4, 15, 23, 19, 13, 12, 2, 20, 14, 22, 9, 6, 1,
];
const KECCAK_ROTL: [u32; 24] = [
    1, 3, 6, 10, 15, 21, 28, 36, 45, 55, 2, 14, 27, 41, 56, 8, 25, 43, 62, 18, 39, 61, 20, 44,
];
const KECCAK_IOTA: [u64; 24] = [
    0x0000000000000001, 0x0000000000008082, 0x800000000000808a, 0x8000000080008000,
    0x000000000000808b, 0x0000000080000001, 0x8000000080008081, 0x8000000000008009,
    0x000000000000008a, 0x0000000000000088, 0x0000000080008009, 0x000000008000000a,
    0x000000008000808b, 0x800000000000008b, 0x8000000000008089, 0x8000000000008003,
    0x8000000000008002, 0x8000000000000080, 0x000000000000800a, 0x800000008000000a,
    0x8000000080008081, 0x8000000000008080, 0x0000000080000001, 0x8000000080008008,
];

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Keccak-f[1600] with 23 rounds (round 0 skipped, matching DeepSeekHashV1).
fn keccak_p23(s: &mut [u64; 25]) {
    for &iota in &KECCAK_IOTA[1..] {
        let mut c = [0u64; 5];
        for x in 0..5 {
            c[x] = s[x] ^ s[x + 5] ^ s[x + 10] ^ s[x + 15] ^ s[x + 20];
        }
        for x in 0..5 {
            let d = c[(x + 4) % 5] ^ c[(x + 1) % 5].rotate_left(1);
            for y in (0..25).step_by(5) {
                s[x + y] ^= d;
            }
        }
        let mut cur = s[1];
        for t in 0..24 {
            let pi = KECCAK_PI[t];
            let next = s[pi];
            s[pi] = cur.rotate_left(KECCAK_ROTL[t]);
            cur = next;
        }
        for y in (0..25).step_by(5) {
            let row = [s[y], s[y + 1], s[y + 2], s[y + 3], s[y + 4]];
            for x in 0..5 {
                s[y + x] = row[x] ^ ((!row[(x + 1) % 5]) & row[(x + 2) % 5]);
            }
        }
        s[0] ^= iota;
    }
}

pub fn find_deepseek_pow_nonce(prefix: &str, challenge_hex: &str, difficulty: u64) -> Option<u64> {
    if challenge_hex.len() != 64 || difficulty == 0 || difficulty > MAX_POW_DIFFICULTY {
        return None;
    }
    let mut target = [0u8; 32];
    for (i, slot) in target.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&challenge_hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    let mut target_words = [0u64; 4];
    for i in 0..4 {
        target_words[i] = u64::from_le_bytes(target[i * 8..(i + 1) * 8].try_into().unwrap());
    }

    const BLOCK: usize = 136;
    let mut snap = [0u64; 25];
    let mut pos = 0usize;
    for &b in prefix.as_bytes() {
        let word = pos / 8;
        let shift = (pos % 8) * 8;
        snap[word] ^= (b as u64) << shift;
        pos += 1;
        if pos == BLOCK {
            keccak_p23(&mut snap);
            pos = 0;
        }
    }

    for nonce in 0..difficulty {
        let mut st = snap;
        let mut p = pos;
        let s = nonce.to_string();
        for &b in s.as_bytes() {
            st[p / 8] ^= (b as u64) << ((p % 8) * 8);
            p += 1;
            if p == BLOCK {
                keccak_p23(&mut st);
                p = 0;
            }
        }
        st[p / 8] ^= 0x06u64 << ((p % 8) * 8);
        st[(BLOCK - 1) / 8] ^= 0x80u64 << (((BLOCK - 1) % 8) * 8);
        keccak_p23(&mut st);
        if st[0..4] == target_words {
            return Some(nonce);
        }
    }
    None
}

pub fn solve_pow(challenge: &Value, target_path: &str) -> anyhow::Result<String> {
    let algorithm = challenge
        .get("algorithm")
        .and_then(Value::as_str)
        .context("missing PoW algorithm")?;
    if algorithm != "DeepSeekHashV1" {
        bail!("unsupported DeepSeek PoW algorithm: {algorithm}");
    }
    let challenge_hex = challenge
        .get("challenge")
        .and_then(Value::as_str)
        .context("missing PoW challenge")?;
    let salt = challenge
        .get("salt")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= 1024)
        .context("invalid PoW salt")?;
    let difficulty = challenge
        .get("difficulty")
        .and_then(Value::as_u64)
        .filter(|&d| d >= 1 && d <= MAX_POW_DIFFICULTY)
        .context("invalid PoW difficulty")?;
    let expire_at = challenge
        .get("expire_at")
        .and_then(Value::as_u64)
        .context("missing PoW expire_at")?;
    let signature = challenge
        .get("signature")
        .and_then(Value::as_str)
        .context("missing PoW signature")?;

    let prefix = format!("{salt}_{expire_at}_");
    let answer = find_deepseek_pow_nonce(&prefix, challenge_hex, difficulty)
        .context("DeepSeek PoW solver failed")?;
    let payload = json!({
        "algorithm": algorithm,
        "challenge": challenge_hex,
        "salt": salt,
        "answer": answer,
        "signature": signature,
        "target_path": target_path,
    });
    Ok(STANDARD.encode(payload.to_string().as_bytes()))
}

pub fn extract_user_token(api_key: Option<&str>, access_token: Option<&str>) -> Option<String> {
    let raw = api_key.or(access_token)?.trim();
    if raw.is_empty() {
        return None;
    }
    if let Ok(v) = serde_json::from_str::<Value>(raw)
        && let Some(val) = v.get("value").and_then(Value::as_str)
    {
        return Some(val.to_string());
    }
    Some(raw.to_string())
}

fn content_to_string(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn messages_to_prompt(messages: &[Value]) -> String {
    let mut sys = Vec::new();
    let mut turns = Vec::new();
    for m in messages {
        let role = m.get("role").and_then(Value::as_str).unwrap_or("user");
        let text = content_to_string(m.get("content"));
        if text.is_empty() {
            continue;
        }
        if role == "system" {
            sys.push(text);
        } else {
            turns.push((role, text));
        }
    }
    let mut parts = Vec::new();
    if !sys.is_empty() {
        parts.push(sys.join("\n\n"));
    }
    if turns.len() > 1 {
        let transcript = turns
            .iter()
            .map(|(r, t)| match *r {
                "assistant" => format!("Assistant: {t}"),
                "tool" => format!("Tool result {t}"),
                _ => format!("User: {t}"),
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        parts.push(transcript);
    } else if let Some((_, last)) = turns.last() {
        parts.push(last.clone());
    }
    parts.join("\n\n")
}

fn resolve_model_options(model: &str, body: &Value) -> (&'static str, bool, bool) {
    let m = model.to_ascii_lowercase();
    let model_type = if m.contains("pro") || m.contains("expert") {
        "expert"
    } else {
        "default"
    };
    let thinking = m.contains("r1")
        || m.contains("think")
        || m.contains("reason")
        || body.get("thinking_enabled").and_then(Value::as_bool) == Some(true)
        || body.get("thinking").and_then(Value::as_bool) == Some(true)
        || body.get("reasoning_effort").is_some_and(|v| !v.is_null());
    let search = m.contains("search")
        || body.get("search_enabled").and_then(Value::as_bool) == Some(true)
        || body.get("search").and_then(Value::as_bool) == Some(true)
        || body.get("web_search").and_then(Value::as_bool) == Some(true);
    (model_type, thinking, search)
}

fn apply_fake_headers(builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    builder
        .header("Accept", "*/*")
        .header("Accept-Language", "en-US,en;q=0.9")
        .header("Origin", DEEPSEEK_WEB_BASE)
        .header("Referer", format!("{DEEPSEEK_WEB_BASE}/"))
        .header(
            "User-Agent",
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/149.0.0.0 Safari/537.36",
        )
        .header("X-Client-Bundle-Id", "com.deepseek.chat")
        .header("X-Client-Locale", "en-US")
        .header("X-Client-Platform", "web")
        .header("X-Client-Version", "2.0.0")
}

pub(crate) struct Decoder {
    id: String,
    created: u64,
    model: String,
    buf: String,
    emitted_role: bool,
    current_path: String,
    thinking_model: bool,
    search_model: bool,
    finished: FinishOnce,
    drain: DrainWindow,
}

impl Decoder {
    pub fn new(model: &str) -> Self {
        let created = unix_secs();
        let lower = model.to_ascii_lowercase();
        Self {
            id: format!("chatcmpl-deepseek-{created}"),
            created,
            model: if model.is_empty() {
                "deepseek-web".into()
            } else {
                model.to_string()
            },
            buf: String::new(),
            emitted_role: false,
            current_path: String::new(),
            thinking_model: lower.contains("think") || lower.contains("r1") || lower.contains("reason"),
            search_model: lower.contains("search") || lower.contains("fold"),
            finished: FinishOnce::default(),
            drain: DrainWindow::default(),
        }
    }

    pub fn push(&mut self, bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
        self.buf.push_str(&String::from_utf8_lossy(bytes));
        let mut out = Vec::new();
        while let Some(nl) = self.buf.find('\n') {
            let line = self.buf[..nl].trim().to_string();
            self.buf.drain(..=nl);
            self.process_line(&line, &mut out);
        }
        self.close_if_drained(&mut out);
        Ok(out)
    }

    /// Closes the stream once the post-FINISHED drain window elapses (750ms, see done-terminator).
    fn close_if_drained(&mut self, out: &mut Vec<u8>) {
        if self.drain.is_pending() && self.drain.is_due(std::time::Instant::now()) {
            self.drain.clear();
            self.finish_stream(out);
        }
    }

    pub fn finish(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        if !self.buf.trim().is_empty() {
            let rem = std::mem::take(&mut self.buf);
            self.process_line(rem.trim(), &mut out);
        }
        if !self.finished.has_finished() {
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
            self.emit_chunk(json!({"role": "assistant", "content": ""}), None, out);
        }
    }

    fn send_by_path(&mut self, raw: &str, out: &mut Vec<u8>) {
        let text = raw.replace("FINISHED", "");
        if text.is_empty() {
            return;
        }
        self.ensure_role(out);
        let path = if !self.current_path.is_empty() {
            self.current_path.as_str()
        } else if self.thinking_model {
            "thinking"
        } else if self.search_model {
            "content"
        } else {
            "content"
        };
        if path == "thinking" {
            self.emit_chunk(json!({"reasoning_content": text}), None, out);
        } else {
            self.emit_chunk(json!({"content": text}), None, out);
        }
    }

    fn finish_stream(&mut self, out: &mut Vec<u8>) {
        if !self.finished.try_finish() {
            return;
        }
        self.ensure_role(out);
        self.emit_chunk(json!({}), Some("stop"), out);
        out.extend_from_slice(b"data: [DONE]\n\n");
    }

    fn process_line(&mut self, line: &str, out: &mut Vec<u8>) {
        if self.finished.has_finished() || !line.starts_with("data:") {
            return;
        }
        let payload = line[5..].trim();
        if payload == "[DONE]" {
            self.finish_stream(out);
            return;
        }
        let Ok(data) = serde_json::from_str::<Value>(payload) else {
            return;
        };
        let p = data.get("p").and_then(Value::as_str).unwrap_or("");
        let v = data.get("v");

        if let Some(resp) = v.and_then(|val| val.get("response")) {
            match resp.get("thinking_enabled").and_then(Value::as_bool) {
                Some(true) => self.current_path = "thinking".into(),
                Some(false) => self.current_path = "content".into(),
                None => {}
            }
            if let Some(frags) = resp.get("fragments").and_then(Value::as_array) {
                for frag in frags {
                    self.handle_fragment(frag, out);
                }
            }
        }
        if p == "response/fragments" {
            if let Some(arr) = v.and_then(Value::as_array) {
                for frag in arr {
                    self.handle_fragment(frag, out);
                }
            } else if let Some(frag) = v.filter(|x| x.is_object()) {
                self.handle_fragment(frag, out);
            }
        }
        if let Some(s) = v.and_then(Value::as_str)
            && p != "response/status"
            && p != "response/search_status"
        {
            self.send_by_path(s, out);
        }
        if p == "response/status" && v.and_then(Value::as_str) == Some("FINISHED") {
            // Drain window: late search_results may still arrive (deepseek-web-done-terminator.js).
            self.drain.schedule(std::time::Instant::now());
        } else if self.drain.is_pending() && !p.is_empty() {
            // Late payload after FINISHED extends the window.
            self.drain.schedule(std::time::Instant::now());
        }
    }

    fn handle_fragment(&mut self, frag: &Value, out: &mut Vec<u8>) {
        let ty = frag
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_uppercase();
        if ty == "THINK" {
            self.current_path = "thinking".into();
        } else if ty == "ANSWER" || ty == "RESPONSE" {
            self.current_path = "content".into();
        }
        if let Some(content) = frag.get("content").and_then(Value::as_str)
            && !content.is_empty()
        {
            self.send_by_path(content, out);
        }
    }
}

pub async fn execute(
    client: &reqwest::Client,
    request: &super::ExecutorRequest,
) -> anyhow::Result<reqwest::Response> {
    let user_token = extract_user_token(request.api_key.as_deref(), request.access_token.as_deref())
        .context("Missing userToken for deepseek-web")?;
    let base = request
        .base_url
        .as_deref()
        .unwrap_or(DEEPSEEK_WEB_BASE)
        .trim_end_matches('/');
    let api_base = format!("{base}/api");

    // 1. Exchange userToken for short-lived accessToken via /v0/users/current
    let user_resp = apply_fake_headers(
        client
            .get(format!("{api_base}/v0/users/current"))
            .header("Authorization", format!("Bearer {user_token}")),
    )
    .send()
    .await?;
    if !user_resp.status().is_success() {
        bail!("deepseek-web users/current HTTP {}", user_resp.status());
    }
    let user_bytes = user_resp.bytes().await?;
    let user_json: Value = serde_json::from_slice(&user_bytes)?;
    let access_token = user_json
        .pointer("/data/biz_data/token")
        .or_else(|| user_json.pointer("/biz_data/token"))
        .and_then(Value::as_str)
        .context("deepseek-web missing access token in users/current")?
        .to_string();

    // 2. Create chat session
    let sess_resp = apply_fake_headers(
        client
            .post(format!("{api_base}/v0/chat_session/create"))
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {access_token}")),
    )
    .body("{}")
    .send()
    .await?;
    if !sess_resp.status().is_success() {
        bail!("deepseek-web chat_session/create HTTP {}", sess_resp.status());
    }
    let sess_bytes = sess_resp.bytes().await?;
    let sess_json: Value = serde_json::from_slice(&sess_bytes)?;
    let session_id = sess_json
        .pointer("/data/biz_data/chat_session/id")
        .or_else(|| sess_json.pointer("/biz_data/chat_session/id"))
        .and_then(Value::as_str)
        .context("deepseek-web missing chat_session.id")?
        .to_string();

    // 3. Request & solve DeepSeekHashV1 PoW challenge
    let target_path = "/api/v0/chat/completion";
    let pow_resp = apply_fake_headers(
        client
            .post(format!("{api_base}/v0/chat/create_pow_challenge"))
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {access_token}")),
    )
    .body(json!({"target_path": target_path}).to_string())
    .send()
    .await?;
    if !pow_resp.status().is_success() {
        bail!(
            "deepseek-web create_pow_challenge HTTP {}",
            pow_resp.status()
        );
    }
    let pow_bytes = pow_resp.bytes().await?;
    let pow_json: Value = serde_json::from_slice(&pow_bytes)?;
    let challenge = pow_json
        .pointer("/data/biz_data/challenge")
        .or_else(|| pow_json.pointer("/biz_data/challenge"))
        .context("deepseek-web missing PoW challenge")?;
    let pow_answer = solve_pow(challenge, target_path)?;

    // 4. Send completion
    let messages = request
        .body
        .get("messages")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let prompt = messages_to_prompt(messages);
    let (model_type, thinking_enabled, search_enabled) =
        resolve_model_options(&request.model, &request.body);
    let payload = json!({
        "chat_session_id": session_id,
        "parent_message_id": Value::Null,
        "model_type": model_type,
        "prompt": prompt,
        "ref_file_ids": request.body.get("ref_file_ids").cloned().unwrap_or_else(|| json!([])),
        "thinking_enabled": thinking_enabled,
        "search_enabled": search_enabled,
        "preempt": false,
    });

    Ok(apply_fake_headers(
        client
            .post(format!("{api_base}/v0/chat/completion"))
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {access_token}"))
            .header("X-Ds-Pow-Response", pow_answer),
    )
    .body(payload.to_string())
    .send()
    .await?)
}

#[cfg(test)]
pub(crate) fn solve_pow_challenge_for_test(salt: &str, expire_at: u64, nonce: u64) -> String {
    let mut st = [0u64; 25];
    let input = format!("{salt}_{expire_at}_{nonce}");
    for (i, &b) in input.as_bytes().iter().enumerate() {
        st[i / 8] ^= (b as u64) << ((i % 8) * 8);
    }
    st[input.len() / 8] ^= 0x06u64 << ((input.len() % 8) * 8);
    st[135 / 8] ^= 0x80u64 << ((135 % 8) * 8);
    keccak_p23(&mut st);
    let mut hex = String::new();
    for w in &st[0..4] {
        for b in w.to_le_bytes() {
            hex.push_str(&format!("{b:02x}"));
        }
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pow_solver_finds_known_nonce() {
        // Compute challenge for prefix "salt_123_" + nonce "7" using keccak_p23 directly
        let mut st = [0u64; 25];
        let input = b"salt_123_7";
        for (i, &b) in input.iter().enumerate() {
            st[i / 8] ^= (b as u64) << ((i % 8) * 8);
        }
        st[input.len() / 8] ^= 0x06u64 << ((input.len() % 8) * 8);
        st[135 / 8] ^= 0x80u64 << ((135 % 8) * 8);
        keccak_p23(&mut st);
        let mut hex = String::new();
        for w in &st[0..4] {
            for b in w.to_le_bytes() {
                hex.push_str(&format!("{b:02x}"));
            }
        }
        let nonce = find_deepseek_pow_nonce("salt_123_", &hex, 100).unwrap();
        assert_eq!(nonce, 7);
    }

    #[test]
    fn decoder_transforms_fragments_to_openai_sse() {
        let mut dec = Decoder::new("deepseek-r1");
        let raw = concat!(
            "data: {\"p\":\"response/fragments\",\"v\":[{\"type\":\"THINK\",\"content\":\"hmm\"}]}\n",
            "data: {\"p\":\"response/fragments\",\"v\":[{\"type\":\"ANSWER\",\"content\":\"42\"}]}\n",
            "data: {\"p\":\"response/status\",\"v\":\"FINISHED\"}\n",
        );
        let out = String::from_utf8(dec.push(raw.as_bytes()).unwrap()).unwrap()
            + &String::from_utf8(dec.finish()).unwrap();
        assert!(out.contains("\"reasoning_content\":\"hmm\""));
        assert!(out.contains("\"content\":\"42\""));
        assert!(out.contains("data: [DONE]"));
    }

    #[test]
    fn finished_defers_stop_until_drain_window_elapses() {
        let mut dec = Decoder::new("deepseek-r1");
        let finished = b"data: {\"p\":\"response/status\",\"v\":\"FINISHED\"}\n";
        let out = String::from_utf8(dec.push(finished).unwrap()).unwrap();
        // Within the 750ms drain window: no stop frame, no [DONE] yet.
        assert!(!out.contains("data: [DONE]"));
        assert!(!out.contains("\"finish_reason\":\"stop\""));
        // Force the window past its deadline, then the next push closes the stream.
        dec.drain.schedule(std::time::Instant::now() - std::time::Duration::from_secs(1));
        let closed = String::from_utf8(dec.push(b"").unwrap()).unwrap();
        assert!(closed.contains("\"finish_reason\":\"stop\""));
        assert!(closed.contains("data: [DONE]"));
    }
}
