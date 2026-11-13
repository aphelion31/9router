use crate::services::usage_shared::{to_finite_number, unix_millis_iso};
use serde_json::{Value, json};

pub fn parse_quota_response(body: &Value) -> Value {
    let data = &body["data"];
    let mut quotas = serde_json::Map::new();
    if let Some(limits) = data["limits"].as_array() {
        for limit in limits {
            let kind = limit["type"].as_str().unwrap_or("");
            if kind != "TOKENS_LIMIT" && kind != "CREDIT_LIMIT" {
                continue;
            }
            let used = to_finite_number(&limit["percentage"], 0.0);
            let remaining = (100.0 - used).max(0.0);
            let n = limit["number"]
                .as_i64()
                .map(|n| n.to_string())
                .unwrap_or_else(|| limit["number"].as_str().unwrap_or("undefined").to_owned());
            let key = match limit["unit"].as_i64() {
                Some(3) => format!("Session ({n}h)"),
                Some(6) => "Weekly (7d)".into(),
                _ if kind == "TOKENS_LIMIT" => "Tokens".into(),
                _ => format!("Limit ({n})"),
            };
            let millis = to_finite_number(&limit["nextResetTime"], 0.0);
            quotas.insert(key, json!({"used":used,"total":100,"remaining":remaining,"remainingPercentage":remaining,"resetAt":if millis > 0.0 {unix_millis_iso(millis as i64)} else {None},"unlimited":false}));
        }
    }
    let level = data["level"].as_str().unwrap_or("");
    let plan = if level.is_empty() {
        "Unknown".to_owned()
    } else {
        format!(
            "{}{}",
            level.chars().next().unwrap().to_uppercase(),
            level.chars().skip(1).collect::<String>().to_lowercase()
        )
    };
    json!({"plan":plan,"quotas":quotas})
}

pub async fn get_usage(client: &reqwest::Client, url: &str, api_key: Option<&str>) -> Value {
    let Some(key) = api_key.filter(|s| !s.is_empty()) else {
        return json!({"message":"GLM API key not available."});
    };
    let response = match client
        .get(url)
        .bearer_auth(key)
        .header("accept", "application/json")
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => return json!({"message":format!("GLM error: {error}")}),
    };
    let status = response.status();
    if status.as_u16() == 401 {
        return json!({"message":"GLM API key invalid or expired."});
    }
    if !status.is_success() {
        return json!({"message":format!("GLM quota API error ({}).",status.as_u16())});
    }
    match serde_json::from_str::<Value>(&response.text().await.unwrap_or_default()) {
        Ok(body) => parse_quota_response(&body),
        Err(error) => json!({"message":format!("GLM error: {error}")}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    #[tokio::test]
    async fn fetches_glm_limits() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 2048];
            let n = stream.read(&mut request).await.unwrap();
            assert!(
                String::from_utf8_lossy(&request[..n])
                    .to_lowercase()
                    .contains("authorization: bearer test")
            );
            let body = r#"{"data":{"level":"PRO","limits":[{"type":"TOKENS_LIMIT","unit":3,"number":5,"percentage":20,"nextResetTime":1700000000000}]}}"#;
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
            Some("test"),
        )
        .await;
        task.await.unwrap();
        assert_eq!(result["plan"], "Pro");
        assert_eq!(result["quotas"]["Session (5h)"]["remaining"], 80.0);
        assert_eq!(
            result["quotas"]["Session (5h)"]["resetAt"],
            "2023-11-14T22:13:20.000Z"
        );
    }
}
