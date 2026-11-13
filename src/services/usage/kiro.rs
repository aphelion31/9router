use crate::services::usage_shared::{parse_reset_time, to_finite_number};
use serde_json::{Value, json};

pub async fn get_usage(
    client: &reqwest::Client,
    cw: &str,
    q: &str,
    token: &str,
    data: &Value,
) -> Value {
    let auth = data["authMethod"].as_str().unwrap_or("builder-id");
    let profile = data["profileArn"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or(match auth {
            "api_key" => "",
            "google" | "github" => {
                "arn:aws:codewhisperer:us-east-1:699475941385:profile/EHGA3GRVQMUK"
            }
            _ => "arn:aws:codewhisperer:us-east-1:638616132270:profile/AAAACCCCXXXX",
        });
    let mut errors = Vec::new();
    let mut saw_auth = false;
    for attempt in 0..3 {
        let name = ["codewhisperer-get", "codewhisperer-post", "q-get"][attempt];
        let builder = match attempt {
            0 => client.get(format!("{cw}/getUsageLimits?isEmailRequired=true&origin=AI_EDITOR&resourceType=AGENTIC_REQUEST")).header("x-amz-user-agent", "aws-sdk-js/1.0.0 KiroIDE").header("user-agent", "aws-sdk-js/1.0.0 KiroIDE"),
            1 => {
                let mut body = json!({"origin":"AI_EDITOR","resourceType":"AGENTIC_REQUEST"});
                if !profile.is_empty() { body["profileArn"] = json!(profile); }
                client.post(cw).header("content-type","application/x-amz-json-1.0").header("x-amz-target","AmazonCodeWhispererService.GetUsageLimits").body(body.to_string())
            },
            _ => {
                let Ok(mut url) = reqwest::Url::parse(&format!("{q}/getUsageLimits")) else {
                    errors.push(format!("{name}:invalid URL"));
                    continue;
                };
                url.query_pairs_mut().append_pair("origin", "AI_EDITOR").append_pair("resourceType", "AGENTIC_REQUEST");
                if !profile.is_empty() { url.query_pairs_mut().append_pair("profileArn", profile); }
                client.get(url)
            }
        };
        let mut builder = builder
            .bearer_auth(token)
            .header("accept", "application/json");
        if auth == "api_key" {
            builder = builder.header("tokentype", "API_KEY");
        }
        if auth == "external_idp" {
            builder = builder.header("tokentype", "EXTERNAL_IDP");
        }
        let response = match builder.send().await {
            Ok(r) => r,
            Err(e) => {
                errors.push(format!("{name}:{e}"));
                continue;
            }
        };
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            saw_auth |= status.as_u16() == 401 || status.as_u16() == 403;
            errors.push(format!(
                "{name}:{}{}",
                status.as_u16(),
                if body.is_empty() {
                    String::new()
                } else {
                    format!(":{body}")
                }
            ));
            continue;
        }
        let data: Value = match serde_json::from_str(&body) {
            Ok(v) => v,
            Err(e) => {
                errors.push(format!("{name}:{e}"));
                continue;
            }
        };
        let reset = parse_reset_time(
            data.get("nextDateReset")
                .filter(|v| !v.is_null())
                .unwrap_or(&data["resetDate"]),
        );
        let mut quotas = serde_json::Map::new();
        if let Some(list) = data["usageBreakdownList"].as_array() {
            for item in list {
                let resource = item["resourceType"]
                    .as_str()
                    .unwrap_or("unknown")
                    .to_lowercase();
                let used = to_finite_number(&item["currentUsageWithPrecision"], 0.0);
                let total = to_finite_number(&item["usageLimitWithPrecision"], 0.0);
                quotas.insert(resource.clone(),json!({"used":used,"total":total,"remaining":total-used,"resetAt":reset,"unlimited":false}));
                if item["freeTrialInfo"].is_object() {
                    let trial = &item["freeTrialInfo"];
                    let used = to_finite_number(&trial["currentUsageWithPrecision"], 0.0);
                    let total = to_finite_number(&trial["usageLimitWithPrecision"], 0.0);
                    let trial_reset =
                        parse_reset_time(&trial["freeTrialExpiry"]).or_else(|| reset.clone());
                    quotas.insert(format!("{resource}_freetrial"),json!({"used":used,"total":total,"remaining":total-used,"resetAt":trial_reset,"unlimited":false}));
                }
            }
        }
        return json!({"plan":data["subscriptionInfo"]["subscriptionTitle"].as_str().filter(|s| !s.is_empty()).unwrap_or("Kiro"),"quotas":quotas});
    }
    let message = if saw_auth {
        match auth {"idc" => "Kiro quota API is unavailable for the current AWS IAM Identity Center session. Chat may still work. If this persists after renewing your session, reconnect Kiro.".to_owned(), "google" | "github" => "Kiro quota API authentication expired. Chat may still work.".to_owned(), _ => "Kiro quota API rejected the current token. Chat may still work.".to_owned()}
    } else if let Some(error) = errors.last() {
        format!("Unable to fetch Kiro usage right now. ({error})")
    } else {
        "Unable to fetch Kiro usage right now.".to_owned()
    };
    json!({"message":message,"quotas":{}})
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    #[tokio::test]
    async fn api_key_usage() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut b = [0; 2048];
            let n = stream.read(&mut b).await.unwrap();
            let req = String::from_utf8_lossy(&b[..n]).to_lowercase();
            assert!(req.contains("tokentype: api_key"));
            let body = r#"{"usageBreakdownList":[{"resourceType":"CREDIT","currentUsageWithPrecision":3,"usageLimitWithPrecision":10}]}"#;
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
            "",
            "secret",
            &json!({"authMethod":"api_key"}),
        )
        .await;
        task.await.unwrap();
        assert_eq!(result["quotas"]["credit"]["remaining"], 7.0);
    }
}
