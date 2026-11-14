//! Antigravity quota cache and repeated exhaustion protection.
use serde_json::Value;
use std::{collections::HashMap, sync::{Mutex, OnceLock}, time::{Duration, Instant}};

#[derive(Default)]
struct Entry { quotas: Value, last_refresh: Option<Instant>, strikes: HashMap<String, (Instant, u8)>, blocks: HashMap<String, Instant> }
static CACHE: OnceLock<Mutex<HashMap<String, Entry>>> = OnceLock::new();
fn cache() -> &'static Mutex<HashMap<String, Entry>> { CACHE.get_or_init(Default::default) }

fn candidates<'a>(quotas: &'a Value, model: &str) -> Vec<&'a Value> {
    let clean = model.split_once('/').map_or(model, |(_, m)| m).split('(').next().unwrap_or("").trim();
    let mut keys = vec![clean];
    match clean {
        "claude-sonnet-5-5" => keys.push("claude-sonnet-5-5-high"),
        "claude-opus-5-5" => keys.push("claude-opus-5-5-high"),
        "gemini-3.8-flash" => keys.push("gemini-3.8-flash-medium"),
        "gemini-3.7-flash" => keys.push("gemini-3.7-flash-medium"),
        "gemini-3.6-flash" => keys.push("gemini-3.6-flash-medium"),
        _ => {}
    }
    if clean.starts_with("claude-") || clean.starts_with("gpt-") || clean == "claude" { keys.extend(["claude_gpt_session", "claude_gpt_weekly"]); }
    else if (clean.starts_with("gemini-") && !clean.contains("image")) || clean == "gemini" { keys.extend(["gemini_session", "gemini_weekly"]); }
    keys.into_iter().filter_map(|k| quotas.get(k)).collect()
}
fn active_exhausted(q: &Value) -> bool {
    q["remainingPercentage"].as_f64().is_some_and(|n| n <= 0.0) && q["resetAt"].as_str().is_none_or(|s| {
        rusqlite::Connection::open_in_memory().ok().and_then(|db| db.query_row("SELECT julianday(?1)>julianday('now')", [s], |r| r.get::<_, Option<bool>>(0)).ok().flatten()).unwrap_or(false)
    })
}
pub fn blocked(id: &str, model: &str) -> bool {
    let Ok(cache) = cache().lock() else { return false };
    let Some(e) = cache.get(id) else { return false };
    e.blocks.get(model).is_some_and(|until| *until > Instant::now()) || candidates(&e.quotas, model).into_iter().any(active_exhausted)
}
pub fn reset_seconds(id: &str, model: &str) -> Option<u64> {
    let cache = cache().lock().ok()?;
    let e = cache.get(id)?;
    let db = rusqlite::Connection::open_in_memory().ok()?;
    candidates(&e.quotas, model).into_iter().filter(|q| active_exhausted(q)).filter_map(|q| q["resetAt"].as_str()).filter_map(|s| db.query_row("SELECT CAST((julianday(?1)-julianday('now'))*86400 AS INTEGER)", [s], |r| r.get::<_, Option<i64>>(0)).ok().flatten()).filter(|s| *s > 0).map(|s| s as u64 + 1).max()
}
pub fn clear(id: &str, model: &str) {
    if let Ok(mut cache) = cache().lock() && let Some(e) = cache.get_mut(id) { e.strikes.remove(model); e.blocks.remove(model); }
}
pub async fn on_error(id: &str, model: &str, token: &str) {
    static LOCKS: OnceLock<Mutex<HashMap<String, std::sync::Weak<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    let lock = {
        let Ok(mut locks) = LOCKS.get_or_init(Default::default).lock() else { return };
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(id).and_then(std::sync::Weak::upgrade) { lock } else {
            let lock = std::sync::Arc::new(tokio::sync::Mutex::new(()));
            locks.insert(id.to_owned(), std::sync::Arc::downgrade(&lock)); lock
        }
    };
    let _guard = lock.lock().await;
    let should_refresh = if let Ok(mut c) = cache().lock() {
        let e = c.entry(id.to_owned()).or_default();
        if e.last_refresh.is_none_or(|t| t.elapsed() >= Duration::from_secs(30)) { e.last_refresh = Some(Instant::now()); true } else { false }
    } else { false };
    if should_refresh {
        let Ok(client) = reqwest::Client::builder().timeout(Duration::from_secs(10)).redirect(reqwest::redirect::Policy::none()).build() else { return };
        let usage = crate::services::usage::google::antigravity(&client, "https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist", "https://daily-cloudcode-pa.googleapis.com/v1internal:fetchAvailableModels", "https://daily-cloudcode-pa.googleapis.com/v1internal:retrieveUserQuotaSummary", token).await;
        if usage["message"].is_null() && usage["quotas"].is_object() && let Ok(mut c) = cache().lock() { c.entry(id.to_owned()).or_default().quotas = usage["quotas"].clone(); }
    }
    if let Ok(mut c) = cache().lock() {
        let e = c.entry(id.to_owned()).or_default();
        if candidates(&e.quotas, model).into_iter().any(active_exhausted) { e.strikes.remove(model); return; }
        let strike = e.strikes.entry(model.to_owned()).or_insert((Instant::now(), 0));
        if strike.0.elapsed() > Duration::from_secs(60) { *strike = (Instant::now(), 0); }
        strike.1 += 1;
        if strike.1 >= 3 { e.strikes.remove(model); e.blocks.insert(model.to_owned(), Instant::now() + Duration::from_secs(900)); }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn family_quota_and_strike_clear() {
        let id = "quota-test";
        cache().lock().unwrap().insert(id.into(), Entry { quotas: json!({"gemini_weekly":{"remainingPercentage":0}}), ..Default::default() });
        assert!(blocked(id, "gemini-3.8-flash"));
        assert!(!blocked(id, "gemini-image"));
        cache().lock().unwrap().get_mut(id).unwrap().quotas = json!({"gemini_weekly":{"remainingPercentage":0,"resetAt":"2099-01-01T00:00:00Z"},"gemini_session":{"remainingPercentage":0,"resetAt":"2098-01-01T00:00:00Z"}});
        assert!(reset_seconds(id, "gemini-3.8-flash").unwrap() > 1_000_000);
        let mut c = cache().lock().unwrap();
        let e = c.get_mut(id).unwrap(); e.quotas = json!({}); e.blocks.insert("gemini-3.8-flash".into(), Instant::now() + Duration::from_secs(30)); drop(c);
        assert!(blocked(id, "gemini-3.8-flash")); clear(id, "gemini-3.8-flash"); assert!(!blocked(id, "gemini-3.8-flash"));
    }
    #[tokio::test]
    async fn repeated_errors_block_then_success_clears() {
        let id = "quota-strike-test";
        cache().lock().unwrap().insert(id.into(), Entry { last_refresh: Some(Instant::now()), ..Default::default() });
        for _ in 0..2 { on_error(id, "gemini-3.8-flash", "unused").await; assert!(!blocked(id, "gemini-3.8-flash")); }
        on_error(id, "gemini-3.8-flash", "unused").await;
        assert!(blocked(id, "gemini-3.8-flash"));
        clear(id, "gemini-3.8-flash");
        assert!(!blocked(id, "gemini-3.8-flash"));
    }
}
