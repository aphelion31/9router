use crate::services::usage_shared::to_finite_number;
use serde_json::{Value, json};

pub fn iflow() -> Value {
    json!({"message":"iFlow connected. Usage tracked per request."})
}

pub async fn vercel(client: &reqwest::Client, url: &str, api_key: Option<&str>) -> Value {
    let Some(key) = api_key.filter(|s| !s.is_empty()) else {
        return json!({"message":"Vercel AI Gateway API key not available."});
    };
    let response = match client
        .get(url)
        .bearer_auth(key)
        .header("accept", "application/json")
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => return json!({"message":format!("Vercel AI Gateway error: {error}")}),
    };
    let status = response.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return json!({"message":"Vercel AI Gateway API key invalid or expired."});
    }
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        let detail: String = body.chars().take(200).collect();
        return json!({"message":format!("Vercel AI Gateway credits API error ({}){}",status.as_u16(),if detail.is_empty() {String::new()} else {format!(": {detail}")})});
    }
    let data: Value = match serde_json::from_str(&body) {
        Ok(data) => data,
        Err(error) => return json!({"message":format!("Vercel AI Gateway error: {error}")}),
    };
    let balance = to_finite_number(&data["balance"], 0.0);
    let used = to_finite_number(&data["total_used"], 0.0);
    if balance <= 0.0 && used <= 0.0 {
        return json!({"plan":"Pay-as-you-go","message":"Vercel AI Gateway connected. No credit allocation found (BYOK or unfunded account).","quotas":{}});
    }
    json!({"plan":"Pay-as-you-go","quotas":{"Used (USD)":{"used":used,"total":0,"remaining":0,"remainingPercentage":100,"unlimited":true},"Remaining (USD)":{"used":balance,"total":5,"remaining":balance,"remainingPercentage":balance / 5.0 * 100.0,"unlimited":false}}})
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    #[tokio::test]
    async fn vercel_credits() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            let n = stream.read(&mut request).await.unwrap();
            assert!(
                String::from_utf8_lossy(&request[..n])
                    .to_lowercase()
                    .contains("authorization: bearer test")
            );
            let body = r#"{"balance":"2.5","total_used":"1"}"#;
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });
        let result = vercel(
            &reqwest::Client::new(),
            &format!("http://{addr}"),
            Some("test"),
        )
        .await;
        task.await.unwrap();
        assert_eq!(
            result["quotas"]["Remaining (USD)"]["remainingPercentage"],
            50.0
        );
    }
}
