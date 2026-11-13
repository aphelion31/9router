use anyhow::Context;

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
