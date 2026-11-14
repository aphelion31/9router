use serde_json::{Value, json};

pub const DEFAULT_SYSTEMONE_ROUTER_MODEL: &str = "oc/jev-1.13-free";
pub const DEFAULT_SYSTEMONE_TIMEOUT_MS: u64 = 8000;
pub const MAX_STATE_CHARS: usize = 2000;
pub const MAX_STICKY_SESSIONS: usize = 5000;
pub const STICKY_SESSION_TTL_MS: u64 = 2 * 60 * 60 * 1000;

// combo.js getRotatedModels: rotation is per combo, with consecutive-use limits.
#[derive(Default)]
pub struct ComboRotation(std::collections::HashMap<String, (usize, usize)>);
impl ComboRotation {
    pub fn rotate(&mut self, models: &mut [String], combo: &str, limit: usize) {
        if models.len() <= 1 { return; }
        let state = self.0.entry(if combo.is_empty() { "__default__" } else { combo }.into()).or_default();
        let index = state.0 % models.len();
        models.rotate_left(index);
        state.1 += 1;
        if state.1 >= limit.max(1) { *state = ((index + 1) % models.len(), 0); } else { state.0 = index; }
    }
}

// sessionManager.js extractClientSessionId: explicit client identifiers take precedence.
pub fn client_session_id(headers: &axum::http::HeaderMap, body: &Value) -> Option<String> {
    fn normalize(s: &str) -> Option<&str> { let s = s.trim(); (!s.is_empty() && s.encode_utf16().count() <= 256).then_some(s) }
    let metadata = body["metadata"]["user_id"].as_str().unwrap_or("");
    let claude = metadata.rsplit_once("_session_").and_then(|(_, s)| (!s.is_empty() && s.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c) || c == b'-')).then_some(s.to_owned()))
        .or_else(|| serde_json::from_str::<Value>(metadata).ok().and_then(|v| v["session_id"].as_str().and_then(normalize).map(str::to_owned)))
        .or_else(|| headers.get("x-claude-code-session-id").and_then(|v| v.to_str().ok()).and_then(normalize).map(str::to_owned));
    if let Some(s) = claude { return Some(format!("claude:{s}")); }
    if let Some(s) = body["request"]["sessionId"].as_str().and_then(normalize) { return Some(format!("antigravity:{s}")); }
    for key in ["x-session-id", "session-id", "session_id", "x-amp-thread-id", "x-client-request-id"] {
        if let Some(s) = headers.get(key).and_then(|v| v.to_str().ok()).and_then(normalize) { return Some(s.into()); }
    }
    [body["prompt_cache_key"].as_str(), body["session_id"].as_str(), body["conversation_id"].as_str(), Some(metadata)].into_iter().flatten().find_map(normalize).map(str::to_owned)
}

// sessionManager.js resolveSessionIdentity and assistantTextSessionId.
pub fn session_id(headers: &axum::http::HeaderMap, body: &Value) -> String {
    if let Some(id) = client_session_id(headers, body) { return id; }
    let mut text = String::new();
    if let Some(items) = body["messages"].as_array().or_else(|| body["input"].as_array()) {
        for item in items.iter().filter(|m| m["role"] == "assistant") {
            if let Some(s) = item["content"].as_str() { text.push_str(s); }
            else if let Some(blocks) = item["content"].as_array() {
                for block in blocks { if let Some(s) = block["text"].as_str().or_else(|| block["output"].as_str()) { text.push_str(s); } }
            }
            if text.encode_utf16().count() >= 50 { break; }
        }
    }
    if text.encode_utf16().count() < 50 { return crate::server::uuid_like(); }
    let prefix = String::from_utf16_lossy(&text.encode_utf16().take(50).collect::<Vec<_>>());
    let digest = ring::digest::digest(&ring::digest::SHA256, format!("::{prefix}").as_bytes());
    let key: String = digest.as_ref()[..8].iter().map(|b| format!("{b:02x}")).collect();
    static SESSIONS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, (String, u64)>>> = std::sync::OnceLock::new();
    let mut sessions = SESSIONS.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
    if let Some((id, last)) = sessions.get_mut(&key) { *last = now; return id.clone(); }
    if sessions.len() >= 5000 && let Some(oldest) = sessions.iter().min_by_key(|(_, (_, at))| *at).map(|(key, _)| key.clone()) { sessions.remove(&oldest); }
    let id = crate::server::uuid_like();
    sessions.insert(key, (id.clone(), now));
    id
}

#[derive(Default)]
pub struct StickyComboStore {
    entries: std::collections::HashMap<String, (String, u64)>,
}

pub fn sticky_store() -> &'static std::sync::Mutex<StickyComboStore> {
    static STORE: std::sync::OnceLock<std::sync::Mutex<StickyComboStore>> = std::sync::OnceLock::new();
    STORE.get_or_init(Default::default)
}

impl StickyComboStore {
    pub fn get(
        &mut self,
        combo: &str,
        session: &str,
        models: &[String],
        now: u64,
    ) -> Option<String> {
        if combo.is_empty() || session.is_empty() {
            return None;
        }
        let key = format!("{combo}:{session}");
        let entry = self.entries.get_mut(&key)?;
        if now.saturating_sub(entry.1) > STICKY_SESSION_TTL_MS || !models.contains(&entry.0) {
            self.entries.remove(&key);
            return None;
        }
        entry.1 = now;
        Some(entry.0.clone())
    }
    pub fn set(&mut self, combo: &str, session: &str, model: &str, now: u64) {
        if combo.is_empty() || session.is_empty() || model.is_empty() {
            return;
        }
        self.prune(now);
        self.entries
            .insert(format!("{combo}:{session}"), (model.into(), now));
    }
    pub fn clear(&mut self, combo: &str, session: Option<&str>) {
        if combo.is_empty() {
            self.entries.clear();
        } else if let Some(session) = session.filter(|s| !s.is_empty()) {
            self.entries.remove(&format!("{combo}:{session}"));
        } else {
            let prefix = format!("{combo}:");
            self.entries.retain(|key, _| !key.starts_with(&prefix));
        }
    }
    pub fn prune(&mut self, now: u64) {
        if self.entries.len() < MAX_STICKY_SESSIONS {
            return;
        }
        self.entries
            .retain(|_, (_, last)| now.saturating_sub(*last) <= STICKY_SESSION_TTL_MS);
        if self.entries.len() >= MAX_STICKY_SESSIONS {
            let mut keys: Vec<_> = self
                .entries
                .iter()
                .map(|(key, (_, at))| (key.clone(), *at))
                .collect();
            keys.sort_by_key(|(_, at)| *at);
            for (key, _) in keys.into_iter().take(MAX_STICKY_SESSIONS / 4) {
                self.entries.remove(&key);
            }
        }
    }
}

fn text(content: &Value) -> String {
    if let Some(s) = content.as_str() {
        return s.to_owned();
    }
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|part| {
            part.as_str().or_else(|| {
                if part["type"] == "text" {
                    part["text"].as_str()
                } else {
                    None
                }
            })
        })
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn extract_prompt_state(body: &Value, max_chars: usize) -> String {
    let messages = body["messages"]
        .as_array()
        .or_else(|| body["input"].as_array());
    if let Some(messages) = messages.filter(|m| !m.is_empty()) {
        for message in messages.iter().rev() {
            if message["role"] == "user"
                || message["type"] == "message"
                || (message["role"].is_null() && !message["content"].is_null())
            {
                let t = text(
                    message
                        .get("content")
                        .filter(|v| !v.is_null())
                        .unwrap_or(&message["text"]),
                );
                if !t.trim().is_empty() {
                    return t.trim().chars().take(max_chars).collect();
                }
            }
        }
        let last = messages.last().unwrap();
        let t = text(
            last.get("content")
                .filter(|v| !v.is_null())
                .unwrap_or(&last["text"]),
        );
        if !t.is_empty() {
            return t.trim().chars().take(max_chars).collect();
        }
    }
    for key in ["prompt", "input"] {
        if let Some(s) = body[key].as_str().filter(|s| !s.trim().is_empty()) {
            return s.trim().chars().take(max_chars).collect();
        }
    }
    String::new()
}

pub fn build_choice_criteria(models: &[String]) -> Value {
    let mut criteria = serde_json::Map::new();
    for model in models {
        let m = model.trim();
        if !m.is_empty() {
            criteria.insert(m.into(), Value::Null);
        }
    }
    Value::Object(criteria)
}

pub fn parse_systemone_route_answer(value: &Value) -> Option<Value> {
    let route = value["answers"]["route"].as_object()?;
    let choice = route.get("choice")?.as_str()?.trim();
    if choice.is_empty() {
        return None;
    }
    let confidence = route
        .get("confidence")
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite());
    let probabilities = route
        .get("probabilities")
        .and_then(Value::as_object)
        .map(|values| {
            values
                .iter()
                .filter_map(|(k, v)| {
                    let n = v
                        .as_f64()
                        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                        .filter(|n: &f64| n.is_finite())?;
                    Some((k.clone(), json!(n)))
                })
                .collect::<serde_json::Map<_, _>>()
        });
    Some(json!({"choice":choice,"confidence":confidence,"probabilities":probabilities}))
}

pub fn order_models_by_choice(models: &[String], result: &Value) -> Vec<String> {
    let Some(choice) = result["choice"]
        .as_str()
        .filter(|c| models.iter().any(|m| m == c))
    else {
        return models.to_vec();
    };
    let mut others: Vec<_> = models
        .iter()
        .filter(|m| m.as_str() != choice)
        .cloned()
        .collect();
    if let Some(probabilities) = result["probabilities"].as_object() {
        others.sort_by(|a, b| {
            probabilities
                .get(b)
                .and_then(Value::as_f64)
                .unwrap_or(0.0)
                .total_cmp(&probabilities.get(a).and_then(Value::as_f64).unwrap_or(0.0))
        });
    }
    let mut out = vec![choice.to_owned()];
    out.extend(others);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn combo_rotation_is_scoped_and_honors_limit() {
        // combo.js getRotatedModels
        let mut state = ComboRotation::default();
        let pick = |state: &mut ComboRotation, combo| { let mut m = vec!["a".into(), "b".into()]; state.rotate(&mut m, combo, 2); m };
        assert_eq!(pick(&mut state, "x"), ["a", "b"]);
        assert_eq!(pick(&mut state, "y"), ["a", "b"]);
        assert_eq!(pick(&mut state, "x"), ["a", "b"]);
        assert_eq!(pick(&mut state, "x"), ["b", "a"]);
    }
    #[test]
    fn client_session_precedence() {
        // sessionManager.js extractClientSessionId
        let mut h = axum::http::HeaderMap::new();
        h.insert("x-session-id", " header ".parse().unwrap());
        assert_eq!(client_session_id(&h, &json!({"metadata":{"user_id":"user_session_ab-12"}})).as_deref(), Some("claude:ab-12"));
        assert_eq!(client_session_id(&h, &json!({"session_id":"body"})).as_deref(), Some("header"));
        assert_eq!(client_session_id(&Default::default(), &json!({"prompt_cache_key":" key "})).as_deref(), Some("key"));
    }
    #[test]
    fn assistant_text_session_stable_after_fifty_utf16_units() {
        // sessionManager.js assistantTextSessionId and resolveSessionIdentity
        let headers = Default::default();
        let a = json!({"messages":[{"role":"assistant","content":"a".repeat(50)}]});
        let b = json!({"messages":[{"role":"assistant","content":format!("{}extra", "a".repeat(50))}]});
        assert_eq!(session_id(&headers, &a), session_id(&headers, &b));
        assert_ne!(session_id(&headers, &json!({})), session_id(&headers, &json!({})));
    }
    #[test]
    fn sticky_entries_expire_and_validate_membership() {
        let mut store = StickyComboStore::default();
        store.set("combo", "session", "a", 10);
        assert_eq!(
            store.get("combo", "session", &["a".into()], 11).as_deref(),
            Some("a")
        );
        assert_eq!(store.get("combo", "session", &["b".into()], 12), None);
        store.set("combo", "session", "a", 10);
        assert_eq!(
            store.get(
                "combo",
                "session",
                &["a".into()],
                11 + STICKY_SESSION_TTL_MS
            ),
            None
        );
    }
    #[test]
    fn extracts_and_ranks_models() {
        assert_eq!(
            extract_prompt_state(
                &json!({"messages":[{"role":"user","content":"old"},{"role":"user","content":[{"type":"text","text":" hello "}]}]}),
                100
            ),
            "hello"
        );
        let answer = parse_systemone_route_answer(&json!({"answers":{"route":{"choice":" b ","confidence":0.8,"probabilities":{"a":0.1,"c":"0.9"}}}})).unwrap();
        assert_eq!(
            order_models_by_choice(&["a".into(), "b".into(), "c".into()], &answer),
            ["b", "c", "a"]
        );
        assert_eq!(
            build_choice_criteria(&[" x ".into(), "".into()]),
            json!({"x":null})
        );
    }
}
