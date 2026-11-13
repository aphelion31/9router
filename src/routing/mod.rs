//! Combo member ordering, per-account cooldown, and upstream error classification.
#![allow(dead_code)]
pub mod classify;
pub mod combo;
pub mod cooldown;

#[cfg(test)]
mod tests {
    use super::classify::{LONG_COOLDOWN, TRANSIENT_COOLDOWN, Verdict, classify};
    use super::combo::plan;
    use super::cooldown::Cooldowns;
    use std::time::Duration;

    const NOW: u64 = 1_700_000_000_000;

    #[test]
    fn combo_fallback_order_kept_and_graced_member_skipped() {
        let grace = Cooldowns::default();
        let members: Vec<String> = ["a/m1", "b/m2", "c/m3"].map(String::from).to_vec();
        assert_eq!(
            plan(&grace, "combo", &members),
            vec!["a/m1", "b/m2", "c/m3"]
        );

        grace.set("combo\0b/m2", Duration::from_secs(300));
        assert_eq!(plan(&grace, "combo", &members), vec!["a/m1", "c/m3"]);
        // Grace is per combo: another combo still sees the member.
        assert_eq!(
            plan(&grace, "other", &members),
            vec!["a/m1", "b/m2", "c/m3"]
        );
    }

    #[test]
    fn expired_grace_rejoins_member() {
        let grace = Cooldowns::default();
        let members: Vec<String> = ["a/m1", "b/m2"].map(String::from).to_vec();
        grace.set("combo\0a/m1", Duration::ZERO);
        assert_eq!(plan(&grace, "combo", &members), vec!["a/m1", "b/m2"]);
    }

    #[test]
    fn cooled_account_is_skipped_and_backoff_doubles() {
        let cd = Cooldowns::default();
        assert!(!cd.is_cooling("acc-1"));
        cd.set("acc-1", LONG_COOLDOWN);
        assert!(cd.is_cooling("acc-1"));
        cd.clear("acc-1");
        assert!(!cd.is_cooling("acc-1"));

        assert_eq!(cd.backoff("acc-2"), Duration::from_secs(2));
        assert_eq!(cd.backoff("acc-2"), Duration::from_secs(4));
        assert_eq!(cd.backoff("acc-2"), Duration::from_secs(8));
    }

    #[test]
    fn opencode_429_quota_is_never_overload() {
        let quota = r#"{"error":{"message":"FreeUsageLimitError","resets_in_seconds":60}}"#;
        assert_eq!(
            classify("opencode", 429, quota, NOW),
            Verdict::Quota {
                reset_ms: NOW + 60_000
            }
        );
        let overloaded_with_reset = r#"{"error":{"message":"overloaded","resets_in_seconds":5}}"#;
        assert!(matches!(
            classify("opencode", 429, overloaded_with_reset, NOW),
            Verdict::Quota { .. }
        ));
    }

    #[test]
    fn opencode_overload_429_retries_other_429_backs_off() {
        let overloaded = r#"{"error":{"message":"Model is Overloaded, try later"}}"#;
        assert_eq!(
            classify("opencode", 429, overloaded, NOW),
            Verdict::Overload
        );
        assert_eq!(
            super::classify::classify_with_retry_after(
                "opencode",
                429,
                overloaded,
                Some(Duration::from_secs(10)),
                NOW
            ),
            Verdict::Quota {
                reset_ms: NOW + 10_000
            }
        );
        assert_eq!(classify("openai", 429, overloaded, NOW), Verdict::Backoff);
        assert_eq!(
            classify("opencode", 429, "rate limited", NOW),
            Verdict::Backoff
        );
    }

    #[test]
    fn retry_after_header_and_resets_in_seconds_set_cooldown() {
        use super::classify::classify_with_retry_after;
        assert_eq!(
            classify_with_retry_after(
                "openai",
                429,
                "rate limit",
                Some(Duration::from_secs(42)),
                NOW
            ),
            Verdict::Fixed(Duration::from_secs(42))
        );
        let body = r#"{"error":{"message":"rate limit","resets_in_seconds":17}}"#;
        assert_eq!(
            classify("openai", 429, body, NOW),
            Verdict::Fixed(Duration::from_secs(17))
        );
        // Capped at the max rate-limit cooldown.
        assert_eq!(
            classify_with_retry_after("openai", 429, "", Some(Duration::from_secs(9999)), NOW),
            Verdict::Fixed(super::classify::MAX_RATE_LIMIT_COOLDOWN)
        );
    }

    #[test]
    fn status_classes() {
        assert_eq!(
            classify("openai", 401, "", NOW),
            Verdict::Fixed(LONG_COOLDOWN)
        );
        assert_eq!(
            classify("openai", 503, "", NOW),
            Verdict::Fixed(TRANSIENT_COOLDOWN)
        );
        assert_eq!(classify("openai", 400, "bad", NOW), Verdict::RequestScoped);
        assert_eq!(classify("openai", 422, "bad", NOW), Verdict::RequestScoped);
    }
}
