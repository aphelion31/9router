use serde_json::{Value, json};

pub const DEFAULT_SYSTEMONE_ROUTER_MODEL: &str = "oc/jev-1.13-free";
pub const DEFAULT_SYSTEMONE_TIMEOUT_MS: u64 = 8000;
pub const MAX_STATE_CHARS: usize = 2000;
pub const MAX_STICKY_SESSIONS: usize = 5000;
pub const STICKY_SESSION_TTL_MS: u64 = 2 * 60 * 60 * 1000;

#[derive(Default)]
pub struct StickyComboStore {
    entries: std::collections::HashMap<String, (String, u64)>,
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
