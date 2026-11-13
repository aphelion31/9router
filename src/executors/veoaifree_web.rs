use anyhow::bail;

pub async fn execute(
    _client: &reqwest::Client,
    _request: &super::ExecutorRequest,
) -> anyhow::Result<reqwest::Response> {
    bail!("veoaifree-web is a media provider — use /v1/images/generations or /v1/videos/generations")
}
