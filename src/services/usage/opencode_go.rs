use crate::services::usage_shared::{parse_reset_time, to_finite_number};
use serde_json::{Value, json};

pub async fn get_usage(
    client: &reqwest::Client,
    url: &str,
    api_key: Option<&str>,
    zen: bool,
) -> Value {
    let plan = if zen { "OpenCode Zen" } else { "OpenCode Go" };
    let Some(key) = api_key.map(str::trim).filter(|s| !s.is_empty()) else {
        return json!({"message":format!("{plan} API key not available. Add a key to view usage.")});
    };
    let response = match client
        .get(url)
        .bearer_auth(key)
        .header("accept", "application/json")
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => return json!({"message":format!("{plan} error: {error}")}),
    };
    let status = response.status();
    if status.as_u16() == 401 {
        return json!({"plan":plan,"message":format!("{plan} authentication failed. Check the API key.")});
    }
    if status.as_u16() == 403 {
        let body: Value =
            serde_json::from_str(&response.text().await.unwrap_or_default()).unwrap_or(Value::Null);
        let message = if body["error"]["type"] == "EntitlementError" {
            format!(
                "{plan} {} required for this API key.",
                if zen { "billing" } else { "subscription" }
            )
        } else {
            format!("{plan} access forbidden for this API key.")
        };
        return json!({"plan":plan,"message":message});
    }
    if !status.is_success() {
        return json!({"plan":plan,"message":format!("{plan} usage API error ({}).",status.as_u16())});
    }
    let data: Value =
        serde_json::from_str(&response.text().await.unwrap_or_default()).unwrap_or(Value::Null);
    if !data["usage"].is_object() {
        return json!({"plan":plan,"message":format!("{plan} usage response did not contain quota data.")});
    }
    let mut quotas = serde_json::Map::new();
    for (period, name) in [
        ("rolling", "Rolling"),
        ("weekly", "Weekly"),
        ("monthly", "Monthly"),
    ] {
        let quota = &data["usage"][period];
        let percent = to_finite_number(&quota["percent"], f64::NAN);
        if !percent.is_finite() {
            continue;
        }
        let used = percent.clamp(0.0, 100.0);
        quotas.insert(name.to_owned(), json!({"used":used,"total":100,"remaining":100.0-used,"remainingPercentage":100.0-used,"resetAt":parse_reset_time(&quota["resetsAt"]),"unlimited":false}));
    }
    if quotas.is_empty() {
        return json!({"plan":plan,"message":format!("{plan} usage response did not contain valid quota data.")});
    }
    json!({"plan":plan,"quotas":quotas})
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    #[tokio::test]
    async fn parses_quota_and_authentication() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 2048];
            let n = stream.read(&mut request).await.unwrap();
            assert!(
                String::from_utf8_lossy(&request[..n])
                    .to_lowercase()
                    .contains("authorization: bearer key")
            );
            let body = r#"{"usage":{"weekly":{"percent":"25","resetsAt":1700000000}}}"#;
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
            &format!("http://{addr}"),
            Some("key"),
            false,
        )
        .await;
        task.await.unwrap();
        assert_eq!(result["quotas"]["Weekly"]["remaining"], 75.0);
        assert_eq!(
            result["quotas"]["Weekly"]["resetAt"],
            "2023-11-14T22:13:20.000Z"
        );
    }
}
