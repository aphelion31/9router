use anyhow::Context;

pub(super) fn url(endpoint: &str, model: &str, settings: Option<&serde_json::Value>) -> anyhow::Result<reqwest::Url> {
    let setting = |key: &str| settings.and_then(|v| v.get(key)).and_then(|v| v.as_str()).filter(|v| !v.is_empty());
    let model = setting("deployment").unwrap_or(model);
    let mut url = reqwest::Url::parse(endpoint).context("invalid Azure endpoint")?;
    url.path_segments_mut()
        .map_err(|_| anyhow::anyhow!("invalid Azure endpoint"))?
        .pop_if_empty()
        .extend(["openai", "deployments", model, "chat", "completions"]);
    let version = setting("apiVersion").map(str::to_owned)
        .or_else(|| std::env::var("AZURE_API_VERSION").ok().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| "2024-10-01-preview".into());
    url.query_pairs_mut().append_pair("api-version", &version);
    Ok(url)
}
