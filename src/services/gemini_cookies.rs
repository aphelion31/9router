use serde_json::Value;

pub fn normalize_gemini_cookie_input(raw: &str, cookie_name: &str) -> String {
    let raw = raw.trim();
    if raw.is_empty() {
        return String::new();
    }
    if raw.starts_with('{') {
        if let Ok(value) = serde_json::from_str::<Value>(raw) {
            let cookies = value
                .get("cookies")
                .filter(|v| v.is_object())
                .unwrap_or(&value);
            if let Some(map) = cookies.as_object() {
                let pairs: Vec<_> = map
                    .iter()
                    .filter_map(|(name, value)| {
                        value
                            .as_str()
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(|v| format!("{name}={v}"))
                    })
                    .collect();
                if !pairs.is_empty() {
                    return pairs.join("; ");
                }
            }
        }
    }
    if raw.contains('=') {
        raw.into()
    } else {
        format!("{cookie_name}={raw}")
    }
}

pub fn normalize_gemini_validation_cookie(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let raw = if raw
        .get(..7)
        .is_some_and(|s| s.eq_ignore_ascii_case("cookie:"))
    {
        raw[7..].trim_start()
    } else {
        raw
    };
    let cookie = normalize_gemini_cookie_input(raw, "__Secure-1PSID");
    let valid = cookie
        .split(';')
        .map(str::trim)
        .find(|part| {
            part.get(..15)
                .is_some_and(|s| s.eq_ignore_ascii_case("__secure-1psid="))
        })
        .is_some_and(|part| !part[15..].trim().is_empty());
    valid.then_some(cookie)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn normalizes_gemini_cookies() {
        assert_eq!(
            normalize_gemini_validation_cookie(" Cookie: __Secure-1PSID=abc; x=y ").as_deref(),
            Some("__Secure-1PSID=abc; x=y")
        );
        assert_eq!(
            normalize_gemini_validation_cookie("abc").as_deref(),
            Some("__Secure-1PSID=abc")
        );
        assert!(normalize_gemini_validation_cookie("x=y").is_none());
        assert!(normalize_gemini_validation_cookie("__Secure-1PSID=").is_none());
    }
}
