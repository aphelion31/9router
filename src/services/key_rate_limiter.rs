use rusqlite::Connection;
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};

const MINUTE: u64 = 60_000;
const HOUR: u64 = 60 * MINUTE;
const DAY: u64 = 24 * HOUR;
const SLOT_TTL: u64 = 15 * MINUTE;

#[derive(Clone, Copy, Default)]
pub struct Limits {
    pub rpm: usize,
    pub rph: usize,
    pub rpd: usize,
    pub max_concurrent: usize,
}
#[derive(Debug, PartialEq, Eq)]
pub struct Verdict {
    pub kind: &'static str,
    pub used: usize,
    pub limit: usize,
    pub retry_after_sec: u64,
}
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub rpm_used: usize,
    pub rph_used: usize,
    pub rpd_used: usize,
    pub in_flight: usize,
}
#[derive(Default)]
struct Entry {
    times: VecDeque<u64>,
    slots: HashMap<u64, u64>,
}
#[derive(Default)]
struct State {
    keys: HashMap<String, Entry>,
    next_id: u64,
}
#[derive(Clone, Default)]
pub struct KeyRateLimiter {
    state: Arc<Mutex<State>>,
}

fn prune(entry: &mut Entry, now: u64) {
    while entry
        .times
        .front()
        .is_some_and(|t| t.saturating_add(DAY) <= now)
    {
        entry.times.pop_front();
    }
    while entry.times.len() > 200_000 {
        entry.times.pop_front();
    }
    entry
        .slots
        .retain(|_, start| start.saturating_add(SLOT_TTL) > now);
}
fn count(entry: &Entry, now: u64, window: u64) -> usize {
    entry
        .times
        .iter()
        .filter(|t| t.saturating_add(window) > now)
        .count()
}
impl KeyRateLimiter {
    pub fn peek(&self, key: &str, limits: Limits, now: u64) -> Option<Verdict> {
        if key.is_empty() {
            return None;
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let entry = state.keys.get_mut(key)?;
        prune(entry, now);
        if limits.max_concurrent > 0 && entry.slots.len() >= limits.max_concurrent {
            return Some(Verdict {
                kind: "concurrency",
                used: entry.slots.len(),
                limit: limits.max_concurrent,
                retry_after_sec: 5,
            });
        }
        for (kind, limit, window) in [
            ("rpm", limits.rpm, MINUTE),
            ("rph", limits.rph, HOUR),
            ("rpd", limits.rpd, DAY),
        ] {
            if limit == 0 {
                continue;
            }
            let used = count(entry, now, window);
            if used >= limit {
                let first = entry
                    .times
                    .iter()
                    .find(|t| t.saturating_add(window) > now)
                    .copied()
                    .unwrap_or(now);
                return Some(Verdict {
                    kind,
                    used,
                    limit,
                    retry_after_sec: first
                        .saturating_add(window)
                        .saturating_sub(now)
                        .div_ceil(1000)
                        .max(1),
                });
            }
        }
        None
    }
    pub fn admit(&self, key: &str, now: u64) -> Slot {
        if key.is_empty() {
            return Slot {
                state: self.state.clone(),
                key: String::new(),
                id: 0,
            };
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.next_id = state.next_id.wrapping_add(1);
        let id = state.next_id;
        let entry = state.keys.entry(key.into()).or_default();
        prune(entry, now);
        entry.times.push_back(now);
        entry.slots.insert(id, now);
        Slot {
            state: self.state.clone(),
            key: key.into(),
            id,
        }
    }
    pub fn usage(&self, key: &str, now: u64) -> Usage {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(entry) = state.keys.get_mut(key) else {
            return Usage::default();
        };
        prune(entry, now);
        Usage {
            rpm_used: count(entry, now, MINUTE),
            rph_used: count(entry, now, HOUR),
            rpd_used: count(entry, now, DAY),
            in_flight: entry.slots.len(),
        }
    }
}
// Process-wide limiter shared by every /v1 request. Per process by design,
// matching the JS reference (multi-process needs a shared store).
pub fn limiter() -> &'static KeyRateLimiter {
    static LIMITER: OnceLock<KeyRateLimiter> = OnceLock::new();
    LIMITER.get_or_init(KeyRateLimiter::default)
}

// Reads the per-key config stored at kv scope "keyLimits". Returns None when
// no field is set, so unlimited keys skip the limiter entirely. Lookup errors
// fail open.
pub fn load_limits(conn: &Connection, key_id: &str) -> Option<Limits> {
    let raw = crate::db::kv_get(conn, "keyLimits", key_id).ok().flatten()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    let field = |name: &str| {
        v[name]
            .as_f64()
            .filter(|n| n.is_finite() && *n >= 1.0)
            .map_or(0, |n| n.floor() as usize)
    };
    let limits = Limits {
        rpm: field("rpm"),
        rph: field("rph"),
        rpd: field("rpd"),
        max_concurrent: field("maxConcurrent"),
    };
    (limits.rpm | limits.rph | limits.rpd | limits.max_concurrent != 0).then_some(limits)
}

pub struct Slot {
    state: Arc<Mutex<State>>,
    key: String,
    id: u64,
}
impl Slot {
    pub fn release(&mut self) {
        if let Some(entry) = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys
            .get_mut(&self.key)
        {
            entry.slots.remove(&self.id);
        }
    }
}
impl Drop for Slot {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn enforces_windows_and_releases_slots() {
        let limiter = KeyRateLimiter::default();
        let mut slot = limiter.admit("key", 1000);
        let limits = Limits {
            rpm: 1,
            max_concurrent: 1,
            ..Limits::default()
        };
        assert_eq!(
            limiter.peek("key", limits, 1000).unwrap().kind,
            "concurrency"
        );
        slot.release();
        slot.release();
        assert_eq!(
            limiter.peek("key", limits, 1000).unwrap().retry_after_sec,
            60
        );
        assert!(limiter.peek("key", limits, 61_000).is_none());
        let _leaked = limiter.admit("key", 100_000);
        assert_eq!(limiter.usage("key", 100_000 + SLOT_TTL).in_flight, 0);
    }
}
