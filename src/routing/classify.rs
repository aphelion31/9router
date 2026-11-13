//! Upstream error classification. Port of `accountFallback.js` and the OpenCode 429 rules
//! in `executors/opencode.js`.
use serde_json::Value;
use std::time::Duration;

pub const LONG_COOLDOWN: Duration = Duration::from_secs(120);
pub const TRANSIENT_COOLDOWN: Duration = Duration::from_secs(30);
pub const MAX_RATE_LIMIT_COOLDOWN: Duration = Duration::from_secs(30 * 60);
pub const OVERLOAD_MAX_ATTEMPTS: u32 = 8;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Verdict {
    /// Credential at fault: cool the account for a fixed duration.
    Fixed(Duration),
    /// OpenCode quota window: cool until the reported reset (epoch ms). Never retried.
    Quota { reset_ms: u64 },
    /// OpenCode transient "overloaded" 429: retry the same account with delay.
    Overload,
    /// Rate-limit style: cool with exponential backoff per account.
    Backoff,
    /// The request itself is bad: do not touch the account, return the error.
    RequestScoped,
}

const BACKOFF_TEXT: [&str; 4] = ["rate limit", "too many requests", "quota exceeded", "capacity"];

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn classify(provider: &str, status: u16, body: &str, now_ms: u64) -> Verdict {
    let lower = body.to_ascii_lowercase();
    if (400..500).contains(&status) && !matches!(status, 401 | 402 | 403 | 404 | 429) {
        return Verdict::RequestScoped;
    }
    if status == 429 && provider == "opencode" {
        if let Some(reset_ms) = quota_reset_ms(body, now_ms) {
            return Verdict::Quota { reset_ms };
        }
        if lower.contains("overload") {
            return Verdict::Overload;
        }
    }
    if status == 429 || lower.contains("overloaded") || BACKOFF_TEXT.iter().any(|t| lower.contains(t)) {
        return Verdict::Backoff;
    }
    match status {
        401..=404 => Verdict::Fixed(LONG_COOLDOWN),
        _ => Verdict::Fixed(TRANSIENT_COOLDOWN),
    }
}

/// `error.resets_at` (unix s, future only), else `error.resets_in_seconds`.
/// The `retry-after` header is not seen here (ceiling: classify takes the body only).
fn quota_reset_ms(body: &str, now_ms: u64) -> Option<u64> {
    let v: Value = serde_json::from_str(body).ok()?;
    let err = v.get("error")?;
    if let Some(at) = err.get("resets_at").and_then(Value::as_u64).filter(|s| *s > 0)
        && at * 1000 > now_ms
    {
        return Some(at * 1000);
    }
    let secs = err
        .get("resets_in_seconds")
        .and_then(Value::as_f64)
        .filter(|s| *s > 0.0)?;
    Some(now_ms + (secs * 1000.0) as u64)
}

/// Overloaded 429 retry delay: 800ms, doubling, capped at 4s.
pub fn overload_delay(attempt: u32) -> Duration {
    let ms = 800u64.saturating_mul(1u64 << (attempt.saturating_sub(1)).min(10));
    Duration::from_millis(ms.min(4000))
}
