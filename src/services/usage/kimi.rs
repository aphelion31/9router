use crate::services::usage_shared::{parse_reset_time, to_finite_number};
use serde_json::{Value, json};
fn first<'a>(v: &'a Value, keys: &[&str]) -> &'a Value {
    keys.iter()
        .filter_map(|k| v.get(k))
        .find(|v| !v.is_null())
        .unwrap_or(&Value::Null)
}
fn quota(used: f64, total: f64, remaining: f64, reset: &Value) -> Value {
    let total = total.max(0.0);
    json!({"used":used.max(0.0),"total":total,"remainingPercentage":if total>0.0{remaining.max(0.0)/total*100.0}else{0.0},"resetAt":parse_reset_time(reset),"unlimited":false})
}
pub async fn get_usage(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    key: &str,
    psd: &Value,
) -> Value {
    if key.is_empty() && token.is_empty() {
        return json!({"message":"Kimi access token or API key not available."});
    }
    let mut req = client
        .get(url)
        .header("content-type", "application/json")
        .header("accept", "application/json");
    if !key.is_empty() {
        req = req.header("x-api-key", key);
    } else {
        req = req
            .bearer_auth(token)
            .header("x-msh-platform", "9router")
            .header("x-msh-version", env!("CARGO_PKG_VERSION"))
            .header("x-msh-device-name", "unknown")
            .header(
                "x-msh-device-model",
                format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
            )
            .header(
                "x-msh-device-id",
                psd["deviceId"].as_str().unwrap_or("9router"),
            );
    }
    let r = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            return json!({"message":format!("Kimi Coding connected. Unable to fetch usage: {e}")});
        }
    };
    let status = r.status();
    let text = r.text().await.unwrap_or_default();
    let data: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if !status.is_success() {
        let detail = &data["details"][0];
        let debug = first(detail, &["debug"]);
        let reason = first(debug, &["reason"])
            .as_str()
            .or_else(|| data["reason"].as_str())
            .unwrap_or("");
        let localized = debug["localizedMessage"]["message"]
            .as_str()
            .or_else(|| detail["localizedMessage"]["message"].as_str())
            .or_else(|| data["message"].as_str())
            .unwrap_or("");
        let message = if status.as_u16() == 401 {
            "Kimi authentication expired. Please re-authorize.".to_owned()
        } else if status.as_u16() == 403
            && (reason == "REASON_FEATURE_NO_PERMISSION"
                || format!("{} {localized} {text}", data["code"].as_str().unwrap_or(""))
                    .to_lowercase()
                    .contains("permission"))
        {
            if localized.is_empty() {
                "Kimi connected, but this account has no permission to view usage. Subscribe to Kimi Code to access quota.".to_owned()
            } else {
                localized.to_owned()
            }
        } else {
            let snippet: String = if localized.is_empty() {
                text.chars().take(100).collect()
            } else {
                localized.chars().take(100).collect()
            };
            format!(
                "Kimi Coding connected. API Error {}{}",
                status.as_u16(),
                if snippet.is_empty() {
                    String::new()
                } else {
                    format!(": {snippet}")
                }
            )
        };
        return json!({"plan":"Kimi Coding","message":message});
    }
    if data.is_null() {
        return json!({"plan":"Kimi Coding","message":"Kimi Coding connected. Invalid JSON response from API."});
    }
    let mut quotas = serde_json::Map::new();
    let usage = &data["usage"];
    let total = to_finite_number(first(usage, &["limit", "Limit"]), 0.0);
    let used = to_finite_number(first(usage, &["used", "Used"]), 0.0);
    let remaining = to_finite_number(first(usage, &["remaining", "Remaining"]), f64::NAN);
    if total > 0.0 {
        quotas.insert(
            "Weekly".into(),
            quota(
                used,
                total,
                if remaining.is_finite() {
                    remaining
                } else {
                    total - used
                },
                first(usage, &["resetTime", "ResetTime", "reset_at", "resetAt"]),
            ),
        );
    }
    for item in data["limits"].as_array().into_iter().flatten() {
        let d = &item["detail"];
        let total = to_finite_number(first(d, &["limit", "Limit"]), 0.0);
        if total <= 0.0 {
            continue;
        }
        let remaining = to_finite_number(first(d, &["remaining", "Remaining"]), total);
        quotas.insert(
            "Ratelimit".into(),
            quota(
                (total - remaining).max(0.0),
                total,
                remaining,
                first(d, &["resetTime", "reset_at", "resetAt"]),
            ),
        );
    }
    let level = data["user"]["membership"]["level"].as_str().unwrap_or("");
    let plan = match level {
        "LEVEL_BASIC" => "Moderato".into(),
        "LEVEL_INTERMEDIATE" => "Allegretto".into(),
        "LEVEL_ADVANCED" => "Allegro".into(),
        "LEVEL_STANDARD" => "Vivace".into(),
        "" => "Kimi Coding".into(),
        _ => level.strip_prefix("LEVEL_").unwrap_or(level).to_lowercase(),
    };
    if quotas.is_empty() {
        json!({"plan":plan,"message":"Kimi Coding connected. Usage tracked per request."})
    } else {
        json!({"plan":plan,"quotas":quotas})
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    #[tokio::test]
    async fn api_key_quota() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let a = l.local_addr().unwrap();
        let t = tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut b = [0; 2048];
            let n = s.read(&mut b).await.unwrap();
            assert!(
                String::from_utf8_lossy(&b[..n])
                    .to_lowercase()
                    .contains("x-api-key: key")
            );
            let body = r#"{"usage":{"limit":100,"used":30,"remaining":70}}"#;
            s.write_all(
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
        let v = get_usage(
            &reqwest::Client::new(),
            &format!("http://{a}"),
            "",
            "key",
            &Value::Null,
        )
        .await;
        t.await.unwrap();
        assert_eq!(v["quotas"]["Weekly"]["remainingPercentage"], 70.0);
    }
}
