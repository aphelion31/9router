use serde_json::Value;

pub const ANTIGRAVITY_IDE_VERSION: &str = "2.24.0";

pub fn rewrite_antigravity_user_agent(user_agent: &str, version: &str) -> String {
    let Some(start) = user_agent.find("antigravity/") else {
        return user_agent.to_owned();
    };
    let end = user_agent[start..]
        .find(char::is_whitespace)
        .map_or(user_agent.len(), |n| start + n);
    format!(
        "{}antigravity/{}{}",
        &user_agent[..start],
        version,
        &user_agent[end..]
    )
}

pub fn apply_antigravity_ide_version_override(
    body: &[u8],
    user_agent: Option<&str>,
    url: &str,
) -> (Vec<u8>, Option<String>, bool) {
    if !url.contains(":generateContent") && !url.contains(":streamGenerateContent") {
        return (body.to_vec(), user_agent.map(str::to_owned), false);
    }
    let ua = user_agent.map(|s| rewrite_antigravity_user_agent(s, ANTIGRAVITY_IDE_VERSION));
    let changed = ua.as_deref() != user_agent;
    let Ok(mut parsed) = serde_json::from_slice::<Value>(body) else {
        return (body.to_vec(), ua, changed);
    };
    let Some(metadata) = parsed.get_mut("metadata").and_then(Value::as_object_mut) else {
        return (body.to_vec(), ua, changed);
    };
    let matches = metadata
        .get("ideName")
        .and_then(Value::as_str)
        .is_some_and(|s| s.eq_ignore_ascii_case("antigravity"))
        || metadata
            .get("ideType")
            .and_then(Value::as_str)
            .is_some_and(|s| s.eq_ignore_ascii_case("ANTIGRAVITY"))
        || metadata.contains_key("ideVersion");
    if !matches {
        return (body.to_vec(), ua, changed);
    }
    metadata.insert("ideVersion".into(), ANTIGRAVITY_IDE_VERSION.into());
    (
        serde_json::to_vec(&parsed).expect("JSON value serializes"),
        ua,
        true,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rewrites_generation_only() {
        let body = br#"{"metadata":{"ideName":"antigravity","ideVersion":"1"}}"#;
        let (out, ua, applied) = apply_antigravity_ide_version_override(
            body,
            Some("x antigravity/1.0 test"),
            "/v1:generateContent",
        );
        assert!(applied);
        assert_eq!(ua.as_deref(), Some("x antigravity/2.24.0 test"));
        assert_eq!(
            serde_json::from_slice::<Value>(&out).unwrap()["metadata"]["ideVersion"],
            "2.24.0"
        );
        assert!(!apply_antigravity_ide_version_override(body, None, "/models").2);
        assert_eq!(
            apply_antigravity_ide_version_override(
                b"bad",
                Some("antigravity/1"),
                "/v1:generateContent"
            )
            .0,
            b"bad"
        );
    }
}
