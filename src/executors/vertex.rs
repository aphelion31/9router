use anyhow::{Context, bail};

pub(super) fn url(
    endpoint: &str,
    model: &str,
    stream: bool,
    api_key: Option<&str>,
    access_token: Option<&str>,
) -> anyhow::Result<reqwest::Url> {
    let mut url = reqwest::Url::parse(endpoint).context("invalid Vertex endpoint")?;
    let project = std::env::var("GOOGLE_CLOUD_PROJECT")
        .ok()
        .filter(|s| !s.is_empty());
    if access_token.is_some() && project.is_none() {
        bail!("Vertex OAuth requires GOOGLE_CLOUD_PROJECT");
    }
    let location = std::env::var("GOOGLE_CLOUD_LOCATION").unwrap_or_else(|_| {
        if access_token.is_some() {
            "us-central1"
        } else {
            "global"
        }
        .into()
    });
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| anyhow::anyhow!("invalid Vertex endpoint"))?;
        segments.pop_if_empty();
        segments.push("v1");
        if let Some(project) = project.as_deref() {
            segments.extend(["projects", project, "locations", &location]);
        }
        segments.extend([
            "publishers",
            "google",
            "models",
            &format!(
                "{}:{}",
                model,
                if stream {
                    "streamGenerateContent"
                } else {
                    "generateContent"
                }
            ),
        ]);
    }
    if stream {
        url.query_pairs_mut().append_pair("alt", "sse");
    }
    if access_token.is_none() {
        if let Some(key) = api_key {
            if key.trim_start().starts_with('{') {
                bail!("Vertex service account and ADC credentials require token refresh support");
            }
            url.query_pairs_mut().append_pair("key", key);
        }
    }
    Ok(url)
}
