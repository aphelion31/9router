use crate::services::usage_shared::parse_reset_time;
use serde_json::{Value, json};

fn snapshot(q: &Value, reset: &Option<String>) -> Value {
    if q.is_null() {
        return json!({"used":0,"total":0,"unlimited":true,"resetAt":reset});
    }
    let entitlement = q["entitlement"].as_f64().unwrap_or(0.0);
    let remaining = q["remaining"].as_f64().unwrap_or(0.0);
    json!({"used":entitlement-remaining,"total":q["entitlement"],"remaining":q["remaining"],"unlimited":q["unlimited"].as_bool().unwrap_or(false),"resetAt":reset})
}

pub async fn get_usage(
    client: &reqwest::Client,
    url: &str,
    access_token: Option<&str>,
) -> Result<Value, String> {
    let token = access_token.filter(|s| !s.is_empty()).ok_or("Failed to fetch GitHub usage: No GitHub access token available. Please re-authorize the connection.")?;
    let response = client
        .get(url)
        .header("authorization", format!("token {token}"))
        .header("accept", "application/json")
        .header("x-github-api-version", "2022-11-28")
        .header("user-agent", "GitHubCopilotChat/0.26.7")
        .header("editor-version", "vscode/1.100.0")
        .header("editor-plugin-version", "copilot-chat/0.26.7")
        .send()
        .await
        .map_err(|e| format!("Failed to fetch GitHub usage: {e}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "Failed to fetch GitHub usage: GitHub API error: {}",
            response.text().await.unwrap_or_default()
        ));
    }
    let body: Value = serde_json::from_str(
        &response
            .text()
            .await
            .map_err(|e| format!("Failed to fetch GitHub usage: {e}"))?,
    )
    .map_err(|e| format!("Failed to fetch GitHub usage: {e}"))?;
    if body["quota_snapshots"].is_object() {
        let q = &body["quota_snapshots"];
        let reset = parse_reset_time(&body["quota_reset_date"]);
        return Ok(
            json!({"plan":body["copilot_plan"],"resetDate":body["quota_reset_date"],"quotas":{"chat":snapshot(&q["chat"],&reset),"completions":snapshot(&q["completions"],&reset),"premium_interactions":snapshot(&q["premium_interactions"],&reset)}}),
        );
    }
    if body["monthly_quotas"].is_object() || body["limited_user_quotas"].is_object() {
        let reset = parse_reset_time(&body["limited_user_reset_date"]);
        let quota = |name: &str| json!({"used":body["limited_user_quotas"][name].as_f64().unwrap_or(0.0),"total":body["monthly_quotas"][name].as_f64().unwrap_or(0.0),"unlimited":false,"resetAt":reset});
        return Ok(
            json!({"plan":body.get("copilot_plan").filter(|v| !v.is_null()).unwrap_or(&body["access_type_sku"]),"resetDate":body["limited_user_reset_date"],"quotas":{"chat":quota("chat"),"completions":quota("completions")}}),
        );
    }
    Ok(json!({"message":"GitHub Copilot connected. Unable to parse quota data."}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    #[tokio::test]
    async fn parses_paid_snapshots() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            let n = stream.read(&mut request).await.unwrap();
            assert!(
                String::from_utf8_lossy(&request[..n])
                    .to_lowercase()
                    .contains("authorization: token secret")
            );
            let body = r#"{"copilot_plan":"individual","quota_reset_date":"2026-01-01","quota_snapshots":{"chat":{"entitlement":100,"remaining":30}}}"#;
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
            Some("secret"),
        )
        .await
        .unwrap();
        task.await.unwrap();
        assert_eq!(result["quotas"]["chat"]["used"], 70.0);
        assert_eq!(result["quotas"]["completions"]["unlimited"], true);
    }
}
