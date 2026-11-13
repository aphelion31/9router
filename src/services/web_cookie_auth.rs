use serde_json::Value;

pub fn strip_cookie_input_prefix(raw: &str) -> &str {
    let mut value = raw.trim();
    if value.len() >= 7
        && value
            .get(..6)
            .is_some_and(|s| s.eq_ignore_ascii_case("bearer"))
        && value.as_bytes()[6].is_ascii_whitespace()
    {
        value = value[6..].trim_start();
    }
    if value
        .get(..7)
        .is_some_and(|s| s.eq_ignore_ascii_case("cookie:"))
    {
        value = &value[7..];
    }
    value.trim()
}

pub fn parse_json_cookies_to_header(raw: &str) -> Result<Option<String>, String> {
    let raw = raw.trim();
    if !raw.starts_with('[') {
        return Ok(None);
    }
    let Ok(Value::Array(cookies)) = serde_json::from_str::<Value>(raw) else {
        return Ok(None);
    };
    let mut result = Vec::new();
    for (index, entry) in cookies.iter().enumerate() {
        if !entry.is_object() {
            return Err(format!(
                "Invalid cookie JSON at index {index}: expected an object"
            ));
        }
        let name = entry["name"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                format!("Invalid cookie JSON at index {index}: missing required field 'name'")
            })?;
        let value = entry["value"].as_str().ok_or_else(|| {
            format!("Invalid cookie JSON at index {index}: missing required field 'value'")
        })?;
        result.push(format!("{name}={value}"));
    }
    Ok(Some(result.join("; ")))
}

pub fn normalize_session_cookie_header(raw: &str, default_name: &str) -> Result<String, String> {
    let raw = strip_cookie_input_prefix(raw);
    if raw.is_empty() {
        return Ok(String::new());
    }
    if let Some(json) = parse_json_cookies_to_header(raw)? {
        return Ok(json);
    }
    if raw.contains('=') {
        return Ok(raw.to_owned());
    }
    Ok(format!("{default_name}={raw}"))
}

pub fn extract_cookie_value<'a>(raw: &'a str, name: &str) -> &'a str {
    let raw = strip_cookie_input_prefix(raw);
    if raw.contains(';') {
        for part in raw.split(';') {
            let part = part.trim_start();
            if let Some(value) = part.strip_prefix(name).and_then(|s| s.strip_prefix('=')) {
                return value.split_whitespace().next().unwrap_or("");
            }
        }
        return "";
    }
    raw.strip_prefix(name)
        .and_then(|s| s.strip_prefix('='))
        .unwrap_or(raw)
}

pub fn build_grok_cookie_header(raw: &str) -> String {
    let sso = extract_cookie_value(raw, "sso");
    if sso.is_empty() {
        return String::new();
    }
    let mut parts = vec![format!("sso={sso}")];
    for name in ["sso-rw", "cf_clearance", "__cf_bm"] {
        if raw.split(';').any(|part| {
            part.trim_start()
                .strip_prefix(name)
                .is_some_and(|s| s.starts_with('='))
        }) {
            let value = extract_cookie_value(raw, name);
            if !value.is_empty() {
                parts.push(format!("{name}={value}"));
            }
        }
    }
    parts.join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn normalizes_cookie_inputs() {
        assert_eq!(
            normalize_session_cookie_header("Bearer Cookie: token", "sso").unwrap(),
            "sso=token"
        );
        assert_eq!(
            normalize_session_cookie_header(r#"[{"name":"sso","value":"abc"}]"#, "sso").unwrap(),
            "sso=abc"
        );
        assert!(normalize_session_cookie_header(r#"[{"name":"sso"}]"#, "sso").is_err());
        assert_eq!(
            build_grok_cookie_header("foo=x; sso=abc; sso-rw=def; cf_clearance=clear"),
            "sso=abc; sso-rw=def; cf_clearance=clear"
        );
        assert_eq!(extract_cookie_value("foo=x; other=y", "sso"), "");
    }
}
