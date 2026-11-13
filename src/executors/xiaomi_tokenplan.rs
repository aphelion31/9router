use anyhow::Context;

pub(super) fn endpoint(settings: Option<&serde_json::Value>) -> &'static str {
    match settings.and_then(|v| v.get("region")).and_then(|v| v.as_str()) {
        Some("cn") => "https://token-plan-cn.xiaomimimo.com/v1/chat/completions",
        Some("ams") => "https://token-plan-ams.xiaomimimo.com/v1/chat/completions",
        _ => "https://token-plan-sgp.xiaomimimo.com/v1/chat/completions",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_selection_defaults_to_singapore() {
        assert!(endpoint(Some(&serde_json::json!({"region": "cn"}))).contains("-cn."));
        assert!(endpoint(Some(&serde_json::json!({"region": "ams"}))).contains("-ams."));
        assert!(endpoint(Some(&serde_json::json!({"region": "invalid"}))).contains("-sgp."));
        assert!(endpoint(None).contains("-sgp."));
    }
}

/// Select the token-plan API path from the requested protocol.
pub(super) fn url(endpoint: &str, claude: bool) -> anyhow::Result<reqwest::Url> {
    let mut url = reqwest::Url::parse(endpoint).context("invalid Xiaomi token plan endpoint")?;
    if claude {
        let path = url.path().trim_end_matches('/');
        let root = path
            .strip_suffix("/v1/chat/completions")
            .or_else(|| path.strip_suffix("/v1"))
            .unwrap_or(path);
        url.set_path(&format!("{root}/anthropic/v1/messages"));
    }
    Ok(url)
}
