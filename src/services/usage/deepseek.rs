use crate::services::usage_shared::to_finite_number;
use serde_json::{Value, json};

pub async fn get_usage(client: &reqwest::Client, url: &str, api_key: Option<&str>) -> Value {
    let Some(key) = api_key.map(str::trim).filter(|s| !s.is_empty()) else {
        return json!({"message":"DeepSeek API key not available. Add a key to view usage."});
    };
    let response = match client
        .get(url)
        .bearer_auth(key)
        .header("accept", "application/json")
        .header("content-type", "application/json")
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => return json!({"message":format!("DeepSeek error: {error}")}),
    };
    let status = response.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return json!({"plan":"DeepSeek","message":"DeepSeek authentication failed. Check the API key."});
    }
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        let detail: String = body.chars().take(120).collect();
        return json!({"plan":"DeepSeek","message":format!("DeepSeek balance API error ({}){}",status.as_u16(), if detail.is_empty() { String::new() } else { format!(": {detail}") })});
    }
    let Ok(data) = serde_json::from_str::<Value>(&body) else {
        return json!({"message":"DeepSeek balance response was not JSON."});
    };
    if !data.is_object() && !data.is_array() {
        return json!({"message":"DeepSeek balance response was not JSON."});
    }
    let mut quotas = serde_json::Map::new();
    if let Some(balances) = data.get("balance_infos").and_then(Value::as_array) {
        for balance in balances {
            let Some(currency) = balance
                .get("currency")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            else {
                continue;
            };
            let total = to_finite_number(
                balance
                    .get("total_balance")
                    .or_else(|| balance.get("totalBalance"))
                    .unwrap_or(&Value::Null),
                0.0,
            )
            .max(0.0);
            quotas.insert(format!("Balance ({})", currency.to_uppercase()), json!({"used":0,"total":total,"remainingPercentage":if total > 0.0 {100} else {0},"resetAt":null,"unlimited":false,"isCreditBalance":true,"currency":currency.to_uppercase()}));
        }
    }
    if quotas.is_empty() {
        return json!({"plan":"DeepSeek","message":"DeepSeek connected. No balance data returned."});
    }
    json!({"plan":if data["is_available"] == true || data["isAvailable"] == true {"DeepSeek"} else {"DeepSeek (Insufficient Balance)"},"quotas":quotas})
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    #[tokio::test]
    async fn fetches_and_normalizes_balance() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            let n = stream.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..n]).to_lowercase();
            assert!(request.contains("authorization: bearer test-key"));
            let body = r#"{"is_available":true,"balance_infos":[{"currency":"usd","total_balance":"4.5"}]}"#;
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
        let result = get_usage(
            &reqwest::Client::new(),
            &format!("http://{addr}/user/balance"),
            Some(" test-key "),
        )
        .await;
        task.await.unwrap();
        assert_eq!(result["quotas"]["Balance (USD)"]["total"], 4.5);
        assert_eq!(result["plan"], "DeepSeek");
    }
}
