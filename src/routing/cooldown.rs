//! Per-key cooldowns (accounts and combo grace) with exponential backoff.
//! In-memory, single process.
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

const BACKOFF_BASE: Duration = Duration::from_secs(2);
const BACKOFF_MAX: Duration = Duration::from_secs(300);
const BACKOFF_MAX_LEVEL: u32 = 15;

#[derive(Default)]
struct State {
    until: HashMap<String, Instant>,
    level: HashMap<String, u32>,
}

#[derive(Default)]
pub struct Cooldowns(Mutex<State>);

impl Cooldowns {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn is_cooling(&self, key: &str) -> bool {
        self.lock().until.get(key).is_some_and(|t| Instant::now() < *t)
    }

    pub fn set(&self, key: &str, d: Duration) {
        self.lock().until.insert(key.to_string(), Instant::now() + d);
    }

    pub fn clear(&self, key: &str) {
        let mut s = self.lock();
        s.until.remove(key);
        s.level.remove(key);
    }

    /// Bump the key's backoff level and cool it for the new doubled duration.
    pub fn backoff(&self, key: &str) -> Duration {
        let mut s = self.lock();
        let level = {
            let l = s.level.entry(key.to_string()).or_insert(0);
            *l = (*l + 1).min(BACKOFF_MAX_LEVEL);
            *l
        };
        let d = (BACKOFF_BASE * 2u32.pow(level - 1)).min(BACKOFF_MAX);
        s.until.insert(key.to_string(), Instant::now() + d);
        d
    }
}

/// Process-wide state shared by all chat requests.
pub fn global() -> &'static Cooldowns {
    static G: OnceLock<Cooldowns> = OnceLock::new();
    G.get_or_init(Cooldowns::default)
}
