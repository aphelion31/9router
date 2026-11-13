use crate::services::{
    cursor_models::build_cursor_headers,
    usage_shared::{parse_reset_time, to_finite_number},
};
use serde_json::{Value, json};
fn post(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    machine: Option<&str>,
) -> reqwest::RequestBuilder {
    let mut req = client.post(url);
    for (k, v) in build_cursor_headers(token, machine, true) {
        if k != "content-type" && k != "accept" {
            req = req.header(k, v);
        }
    }
    req.header("content-type", "application/json")
        .header("accept", "application/json")
        .body("{}")
}
pub async fn get_usage(
    client: &reqwest::Client,
    period: &str,
    plan_url: &str,
    auth_url: &str,
    token: &str,
    data: &Value,
) -> Value {
    let token = token.trim();
    if token.is_empty() {
        return json!({"message":"Cursor access token not available. Reconnect Cursor to view quota."});
    }
    if period.is_empty() {
        return json!({"message":"Cursor usage endpoint is not configured."});
    }
    let machine = data["machineId"].as_str();
    let r = match post(client, period, token, machine).send().await {
        Ok(r) => r,
        Err(e) => return json!({"message":format!("Cursor error: {e}")}),
    };
    let status = r.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return json!({"plan":"Cursor","message":"Cursor authentication failed. Reconnect the Cursor provider."});
    }
    let text = r.text().await.unwrap_or_default();
    let body: Value = if status.is_success() {
        serde_json::from_str(&text).unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    let mut quotas = serde_json::Map::new();
    let reset = parse_reset_time(&body["billingCycleEnd"]);
    let p = &body["planUsage"];
    let n = |key: &str| to_finite_number(&p[key], 0.0);
    let total = n("limit");
    let used = to_finite_number(
        p.get("includedSpend")
            .filter(|v| !v.is_null())
            .unwrap_or(&p["totalSpend"]),
        0.0,
    );
    if total > 0.0 {
        let remaining = to_finite_number(&p["remaining"], (total - used).max(0.0));
        let pct = if !p["totalPercentUsed"].is_null() {
            100.0 - n("totalPercentUsed").clamp(0.0, 100.0)
        } else {
            remaining / total * 100.0
        };
        quotas.insert("Plan (USD)".into(),json!({"used":(used/100.0*100.0).round()/100.0,"total":(total/100.0*100.0).round()/100.0,"remainingPercentage":pct,"resetAt":reset}));
    } else if !p["totalPercentUsed"].is_null() {
        let used = n("totalPercentUsed");
        quotas.insert("Included usage".into(),json!({"used":used,"total":100,"remainingPercentage":100.0-used.clamp(0.0,100.0),"resetAt":reset}));
    }
    for (key, label) in [
        ("autoPercentUsed", "Auto models (%)"),
        ("apiPercentUsed", "Named models (%)"),
    ] {
        if !p[key].is_null() {
            let used = n(key);
            quotas.insert(label.into(),json!({"used":used,"total":100,"remainingPercentage":100.0-used.clamp(0.0,100.0),"resetAt":reset}));
        }
    }
    let spend = &body["spendLimitUsage"];
    for (prefix, label) in [
        ("individual", "On-demand (USD)"),
        ("pooled", "Team on-demand (USD)"),
    ] {
        let total = to_finite_number(&spend[format!("{prefix}Limit")], 0.0);
        if total <= 0.0 {
            continue;
        }
        let used = to_finite_number(&spend[format!("{prefix}Used")], 0.0);
        let rem = to_finite_number(
            &spend[format!("{prefix}Remaining")],
            (total - used).max(0.0),
        );
        quotas.insert(label.into(),json!({"used":(used/100.0*100.0).round()/100.0,"total":(total/100.0*100.0).round()/100.0,"remainingPercentage":rem/total*100.0,"resetAt":reset}));
        break;
    }
    if quotas.is_empty() && !auth_url.is_empty() {
        let clean = token.split_once("::").map(|(_, v)| v).unwrap_or(token);
        let r = match client
            .get(auth_url)
            .bearer_auth(clean)
            .header("accept", "application/json")
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => return json!({"message":format!("Cursor error: {e}")}),
        };
        if r.status().is_success() {
            let a: Value =
                serde_json::from_str(&r.text().await.unwrap_or_default()).unwrap_or(Value::Null);
            let reset = parse_reset_time(&a["startOfMonth"]);
            if let Some(obj) = a.as_object() {
                for (k, v) in obj {
                    let total = to_finite_number(&v["maxRequestUsage"], 0.0);
                    if total <= 0.0 {
                        continue;
                    }
                    let used = to_finite_number(&v["numRequests"], 0.0);
                    quotas.insert(format!("{k} requests"),json!({"used":used,"total":total,"remainingPercentage":((total-used)/total*100.0).max(0.0),"resetAt":reset}));
                }
            }
        } else if !status.is_success() {
            let detail: String = text.chars().take(120).collect();
            return json!({"plan":"Cursor","message":format!("Cursor usage API error ({}){}",status.as_u16(),if detail.is_empty(){String::new()}else{format!(": {detail}")})});
        }
    }
    if quotas.is_empty() {
        return json!({"plan":"Cursor","message":body["displayMessage"].as_str().unwrap_or("Cursor connected. No quota data returned for this plan.")});
    }
    let mut plan = "Cursor".to_owned();
    if !plan_url.is_empty() {
        if let Ok(r) = post(client, plan_url, token, machine).send().await {
            if r.status().is_success() {
                let data: Value = serde_json::from_str(&r.text().await.unwrap_or_default())
                    .unwrap_or(Value::Null);
                if let Some(name) = data["planInfo"]["planName"]
                    .as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    plan = format!("Cursor {name}");
                }
            }
        }
    }
    json!({"plan":plan,"quotas":quotas})
}
#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    #[tokio::test]
    async fn usage() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let a = l.local_addr().unwrap();
        let t = tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut b = [0; 4096];
            let n = s.read(&mut b).await.unwrap();
            assert!(
                String::from_utf8_lossy(&b[..n])
                    .to_lowercase()
                    .contains("authorization: bearer key")
            );
            let body = r#"{"planUsage":{"limit":1000,"includedSpend":250}}"#;
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
            "",
            "key",
            &Value::Null,
        )
        .await;
        t.await.unwrap();
        assert_eq!(v["quotas"]["Plan (USD)"]["remainingPercentage"], 75.0);
    }
}
