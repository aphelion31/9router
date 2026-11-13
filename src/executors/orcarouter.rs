use reqwest::header::HeaderMap;

pub fn parse_retry_after_ms(headers: &HeaderMap, now_ms: u64) -> Option<u64> {
    if let Some(retry_after) = headers.get("retry-after").and_then(|v| v.to_str().ok()) {
        if let Ok(seconds) = retry_after.trim().parse::<u64>()
            && seconds > 0
        {
            return Some(seconds * 1000);
        }
    }
    if let Some(reset_after) = headers
        .get("x-ratelimit-reset-after")
        .and_then(|v| v.to_str().ok())
    {
        if let Ok(seconds) = reset_after.trim().parse::<u64>()
            && seconds > 0
        {
            return Some(seconds * 1000);
        }
    }
    if let Some(reset_ts) = headers
        .get("x-ratelimit-reset")
        .and_then(|v| v.to_str().ok())
    {
        if let Ok(ts_sec) = reset_ts.trim().parse::<u64>() {
            let ts_ms = ts_sec.saturating_mul(1000);
            if ts_ms > now_ms {
                return Some(ts_ms - now_ms);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;

    #[test]
    fn parses_retry_after_and_ratelimit_headers() {
        let mut h = HeaderMap::new();
        h.insert("retry-after", HeaderValue::from_static("12"));
        assert_eq!(parse_retry_after_ms(&h, 1_000_000), Some(12_000));

        let mut h2 = HeaderMap::new();
        h2.insert("x-ratelimit-reset-after", HeaderValue::from_static("5"));
        assert_eq!(parse_retry_after_ms(&h2, 1_000_000), Some(5_000));

        let mut h3 = HeaderMap::new();
        h3.insert("x-ratelimit-reset", HeaderValue::from_static("1050"));
        assert_eq!(parse_retry_after_ms(&h3, 1_000_000), Some(50_000));
    }
}
