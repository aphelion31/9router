use anyhow::Context;

pub(super) fn url(endpoint: &str, model: &str) -> anyhow::Result<reqwest::Url> {
    let mut url = reqwest::Url::parse(endpoint).context("invalid Azure endpoint")?;
    url.path_segments_mut()
        .map_err(|_| anyhow::anyhow!("invalid Azure endpoint"))?
        .pop_if_empty()
        .extend(["openai", "deployments", model, "chat", "completions"]);
    let version =
        std::env::var("AZURE_API_VERSION").unwrap_or_else(|_| "2024-10-01-preview".into());
    url.query_pairs_mut().append_pair("api-version", &version);
    Ok(url)
}
