use serde_json::Value;

pub async fn get_usage(client: &reqwest::Client, url: &str, api_key: Option<&str>) -> Value {
    super::opencode_go::get_usage(client, url, api_key, true).await
}
