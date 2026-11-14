//! Key-scoped `/v1/models` cache, port of `src/sse/services/modelsListCache.js`.
//!
//! The list is cached per API key id (restricted keys) or `"full"` (unrestricted), so one key's
//! filtered list never serves another key. Entries live for 60s and at most 1000 are kept.
//! Management writes clear the whole cache, and the TTL bounds anything a write path misses.
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const TTL: Duration = Duration::from_secs(60);
const MAX_ENTRIES: usize = 1000;

struct Entry {
    at: Instant,
    data: Value,
}

fn store() -> &'static Mutex<HashMap<String, Entry>> {
    static STORE: OnceLock<Mutex<HashMap<String, Entry>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Cache key for a request: the restricted key's id, or `"full"` when unrestricted.
pub fn cache_key(restricted_key_id: Option<&str>) -> String {
    restricted_key_id.map_or_else(|| "full".to_owned(), |id| format!("key:{id}"))
}

/// Cached list for `key` if fresh. An expired entry is dropped.
pub fn get(key: &str) -> Option<Value> {
    let mut map = store().lock().ok()?;
    let entry = map.get(key)?;
    if entry.at.elapsed() > TTL {
        map.remove(key);
        return None;
    }
    Some(entry.data.clone())
}

/// Stores `data` for `key`. At capacity the oldest entry is evicted first.
pub fn set(key: &str, data: Value) {
    let Ok(mut map) = store().lock() else { return };
    if map.len() >= MAX_ENTRIES
        && !map.contains_key(key)
        && let Some(oldest) = map.iter().min_by_key(|(_, e)| e.at).map(|(k, _)| k.clone())
    {
        map.remove(&oldest);
    }
    map.insert(key.to_owned(), Entry { at: Instant::now(), data });
}

/// Drops every cached list. Called after any management write.
pub fn invalidate() {
    if let Ok(mut map) = store().lock() {
        map.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keys_are_isolated_and_invalidate_clears() {
        invalidate();
        set("full", json!({"n": 1}));
        set("key:a", json!({"n": 2}));
        assert_eq!(get("full"), Some(json!({"n": 1})));
        assert_eq!(get("key:a"), Some(json!({"n": 2})));
        assert_eq!(get("key:b"), None);
        invalidate();
        assert_eq!(get("full"), None);
        assert_eq!(get("key:a"), None);
    }

    #[test]
    fn cache_key_separates_restricted_and_unrestricted() {
        assert_eq!(cache_key(None), "full");
        assert_eq!(cache_key(Some("k1")), "key:k1");
        assert_ne!(cache_key(Some("full")), cache_key(None));
    }
}
