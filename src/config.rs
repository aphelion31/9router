// Ported from open-sse/config/runtimeConfig.js and lib/dataDir.js.

pub const DEFAULT_PORT: u16 = 20127;
pub const DEFAULT_MAX_TOKENS: u32 = 64_000;
pub const DEFAULT_MIN_TOKENS: u32 = 32_000;
pub const TOKEN_SAVER_HEADER: &str = "x-9router-token-saver";

// Timeouts in ms (same defaults as runtimeConfig.js).
pub const STREAM_STALL_TIMEOUT_MS: u64 = 360 * 1000;
pub const STREAM_FIRST_CHUNK_TIMEOUT_MS: u64 = 200 * 1000;
pub const EMPTY_PEEK_DEADLINE_MS: u64 = 45 * 1000;
pub const COMBO_FIRST_RESPONSE_TIMEOUT_MS: u64 = 60 * 1000;
pub const COMBO_FIRST_RESPONSE_GRACE_MS: u64 = 5 * 60 * 1000;
pub const FETCH_CONNECT_TIMEOUT_MS: u64 = 60 * 1000;
pub const TOTAL_REQUEST_TIMEOUT_MS: u64 = 30 * 60 * 1000;

/// Listen port: `PORT` env, else 20127.
pub fn port() -> u16 {
    std::env::var("PORT")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(DEFAULT_PORT)
}
