use crate::services::usage_shared::{parse_reset_time, to_finite_number, unix_millis_iso};
use serde_json::{Value, json};
fn field<'a>(v: &'a Value, a: &str, b: &str) -> &'a Value {
    v.get(a)
        .filter(|v| !v.is_null())
        .or_else(|| v.get(b))
        .unwrap_or(&Value::Null)
}
pub async fn get_usage(client: &reqwest::Client, urls: &[&str], key: &str) -> Value {
    if key.is_empty() {
        return json!({"message":"MiniMax API key not available."});
    }
    let mut last = String::new();
    for (i, url) in urls.iter().enumerate() {
        let fallback = i + 1 < urls.len();
        let response = match client
            .get(*url)
            .bearer_auth(key)
            .header("accept", "application/json")
            .header("content-type", "application/json")
            .send()
            .await
        {
            Ok(v) => v,
            Err(e) => {
                last = e.to_string();
                continue;
            }
        };
        let status = response.status();
        let raw = response.text().await.unwrap_or_default();
        let data: Value = serde_json::from_str(&raw).unwrap_or(json!({}));
        let base = field(&data, "base_resp", "baseResp");
        let code = to_finite_number(field(base, "status_code", "statusCode"), 0.0);
        let msg = field(base, "status_msg", "statusMsg")
            .as_str()
            .unwrap_or("")
            .trim();
        let combined = format!("{msg} {raw}").to_lowercase();
        if status.as_u16() == 401
            || status.as_u16() == 403
            || code == 1004.0
            || [
                "token plan",
                "coding plan",
                "invalid api key",
                "invalid key",
                "unauthorized",
                "inactive",
            ]
            .iter()
            .any(|s| combined.contains(s))
        {
            return json!({"message":"MiniMax API key invalid or inactive. Use an active Token/Coding Plan key."});
        }
        if !status.is_success() {
            last = format!("MiniMax usage endpoint error ({})", status.as_u16());
            if fallback
                && (status.as_u16() == 404 || status.as_u16() == 405 || status.is_server_error())
            {
                continue;
            }
            return json!({"message":format!("MiniMax connected. {last}")});
        }
        if code != 0.0 {
            return json!({"message":format!("MiniMax connected. {}",if msg.is_empty(){"Upstream quota API error"}else{msg})});
        }
        let mut quotas = serde_json::Map::new();
        let models = field(&data, "model_remains", "modelRemains").as_array();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        for model in models.into_iter().flatten() {
            let raw = field(model, "model_name", "modelName")
                .as_str()
                .unwrap_or("")
                .trim();
            let name = if raw.is_empty() {
                "MiniMax".to_owned()
            } else if raw == "general" || raw == "MiniMax-M*" {
                "M-series".to_owned()
            } else {
                raw.replace(['_', '-'], " ")
                    .split_whitespace()
                    .map(|w| {
                        let mut c = w.chars();
                        format!(
                            "{}{}",
                            c.next().unwrap().to_uppercase(),
                            c.as_str().to_lowercase()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            for (
                suffix,
                total_key,
                total_camel,
                count_key,
                count_camel,
                pct_key,
                pct_camel,
                remain_key,
                remain_camel,
                end_key,
                end_camel,
            ) in [
                (
                    "5h",
                    "current_interval_total_count",
                    "currentIntervalTotalCount",
                    "current_interval_usage_count",
                    "currentIntervalUsageCount",
                    "current_interval_remaining_percent",
                    "currentIntervalRemainingPercent",
                    "remains_time",
                    "remainsTime",
                    "end_time",
                    "endTime",
                ),
                (
                    "7d",
                    "current_weekly_total_count",
                    "currentWeeklyTotalCount",
                    "current_weekly_usage_count",
                    "currentWeeklyUsageCount",
                    "current_weekly_remaining_percent",
                    "currentWeeklyRemainingPercent",
                    "weekly_remains_time",
                    "weeklyRemainsTime",
                    "weekly_end_time",
                    "weeklyEndTime",
                ),
            ] {
                let mut total =
                    to_finite_number(field(model, total_key, total_camel), 0.0).max(0.0);
                let pct = to_finite_number(field(model, pct_key, pct_camel), f64::NAN);
                if total <= 0.0 && !pct.is_finite() {
                    continue;
                }
                let pct = if pct.is_finite() {
                    Some(pct.clamp(0.0, 100.0))
                } else {
                    None
                };
                let count_remaining = url.contains("/coding_plan/remains");
                let mut count =
                    to_finite_number(field(model, count_key, count_camel), 0.0).max(0.0);
                if total <= 0.0 {
                    total = 100.0;
                    count = if count_remaining {
                        pct.unwrap()
                    } else {
                        100.0 - pct.unwrap()
                    };
                    count = count.round();
                }
                let used = if count_remaining {
                    (total - count).max(0.0)
                } else {
                    count.clamp(0.0, total)
                };
                let remaining = (total - used).max(0.0);
                let remains = to_finite_number(field(model, remain_key, remain_camel), 0.0);
                let reset = if remains > 0.0 {
                    unix_millis_iso(now.saturating_add(remains as i64))
                } else {
                    parse_reset_time(field(model, end_key, end_camel))
                };
                quotas.insert(format!("{name} ({suffix})"),json!({"used":used,"total":total,"remaining":remaining,"remainingPercentage":pct.unwrap_or(if total>0.0{remaining/total*100.0}else{0.0}),"resetAt":reset,"unlimited":false}));
            }
        }
        if quotas.is_empty() {
            return json!({"message":"MiniMax connected. No quota data was returned."});
        }
        return json!({"quotas":quotas});
    }
    json!({"message":if last.is_empty(){"MiniMax connected. Unable to fetch usage.".to_owned()}else{format!("MiniMax connected. Unable to fetch usage: {last}")}})
}
#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    #[tokio::test]
    async fn quota() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let a = l.local_addr().unwrap();
        let t = tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut b = [0; 2048];
            let n = s.read(&mut b).await.unwrap();
            assert!(
                String::from_utf8_lossy(&b[..n])
                    .to_lowercase()
                    .contains("authorization: bearer key")
            );
            let body = r#"{"model_remains":[{"model_name":"general","current_interval_remaining_percent":25}]}"#;
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
            &[&format!("http://{a}/coding_plan/remains")],
            "key",
        )
        .await;
        t.await.unwrap();
        assert_eq!(v["quotas"]["M-series (5h)"]["remainingPercentage"], 25.0);
    }
}
