use crate::services::usage_shared::{parse_reset_time, to_finite_number};
use serde_json::{Value, json};

fn timestamp(value: &Value) -> Option<f64> {
    crate::services::usage_shared::reset_epoch_millis(value).map(|n| n as f64)
}

pub async fn get_usage(
    client: &reqwest::Client,
    url: &str,
    provider: &str,
    token: Option<&str>,
) -> Value {
    let Some(token) = token.filter(|s| !s.is_empty()) else {
        return json!({"message":format!("CodeBuddy ({provider}) credential not available.")});
    };
    let response = match client
        .post(url)
        .bearer_auth(token)
        .header("content-type", "application/json")
        .header("accept", "application/json")
        .header("user-agent", "CLI/2.108.1 CodeBuddy/2.108.1")
        .header("x-product", "SaaS")
        .header("x-ide-type", "CLI")
        .header("x-ide-name", "CLI")
        .header("x-requested-with", "XMLHttpRequest")
        .header("x-codebuddy-request", "1")
        .body("{}")
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => return json!({"message":format!("CodeBuddy ({provider}) error: {error}")}),
    };
    let status = response.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return json!({"message":"CodeBuddy CN credential invalid or expired."});
    }
    if !status.is_success() {
        return json!({"message":format!("CodeBuddy CN quota API error ({}).",status.as_u16())});
    }
    let body: Value = match serde_json::from_str(&response.text().await.unwrap_or_default()) {
        Ok(body) => body,
        Err(error) => return json!({"message":format!("CodeBuddy ({provider}) error: {error}")}),
    };
    if body["code"] != 0 {
        return json!({"message":format!("CodeBuddy CN quota error: {}",body["msg"].as_str().unwrap_or("unknown"))});
    }
    let Some(accounts) = body["data"]["Response"]["Data"]["Accounts"]
        .as_array()
        .filter(|a| !a.is_empty())
    else {
        return json!({"message":"CodeBuddy CN connected. No credit package found."});
    };
    let is_refill = |a: &&Value| match (
        timestamp(&a["CycleEndTime"]),
        timestamp(&a["DeductionEndTime"]),
    ) {
        (Some(end), Some(expiry)) => expiry - end > 172800000.0,
        _ => false,
    };
    let sort = |a: &&Value, b: &&Value| {
        timestamp(&a["CycleEndTime"])
            .unwrap_or(f64::INFINITY)
            .total_cmp(&timestamp(&b["CycleEndTime"]).unwrap_or(f64::INFINITY))
    };
    let mut refills: Vec<_> = accounts.iter().filter(is_refill).collect();
    let mut bonuses: Vec<_> = accounts.iter().filter(|a| !is_refill(a)).collect();
    refills.sort_by(sort);
    bonuses.sort_by(sort);
    let mut quotas = serde_json::Map::new();
    let mut seen = std::collections::HashMap::<String, usize>::new();
    for account in &refills {
        let days = timestamp(&account["CycleStartTime"])
            .zip(timestamp(&account["CycleEndTime"]))
            .map(|(start, end)| (end - start) / 86400000.0)
            .unwrap_or(f64::INFINITY);
        let cadence = if days <= 1.5 {
            "Daily"
        } else if days <= 10.0 {
            "Weekly"
        } else {
            "Monthly"
        };
        let count = seen.entry(cadence.to_owned()).or_default();
        *count += 1;
        let name = if *count == 1 {
            cadence.to_owned()
        } else {
            format!("{cadence} {count}")
        };
        quotas.insert(name, json!({"used":to_finite_number(account.get("CycleCapacityUsedPrecise").filter(|v| !v.is_null()).unwrap_or(&account["CycleCapacityUsed"]),0.0),"total":to_finite_number(account.get("CycleCapacitySizePrecise").filter(|v| !v.is_null()).unwrap_or(&account["CycleCapacitySize"]),0.0),"resetAt":parse_reset_time(&account["CycleEndTime"]),"unlimited":false,"recurring":true}));
    }
    for (i, account) in bonuses.iter().enumerate() {
        quotas.insert(format!("Bonus Pack {}",i+1),json!({"used":to_finite_number(account.get("CapacityUsedPrecise").filter(|v| !v.is_null()).unwrap_or(&account["CapacityUsed"]),0.0),"total":to_finite_number(account.get("CapacitySizePrecise").filter(|v| !v.is_null()).unwrap_or(&account["CapacitySize"]),0.0),"resetAt":parse_reset_time(&account["CycleEndTime"]),"unlimited":false,"recurring":false}));
    }
    let base = refills.first().copied().unwrap_or(&accounts[0]);
    let plan = base["PackageName"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or_else(|| base["SubProductName"].as_str().filter(|s| !s.is_empty()))
        .unwrap_or("CodeBuddy");
    json!({"plan":plan,"quotas":quotas})
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    #[tokio::test]
    async fn separates_refill_and_bonus() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 2048];
            let n = stream.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..n]).to_lowercase();
            assert!(request.starts_with("post "));
            assert!(request.contains("authorization: bearer secret"));
            let body = r#"{"code":0,"data":{"Response":{"Data":{"Accounts":[{"CycleStartTime":1700000000000,"CycleEndTime":1700604800000,"DeductionEndTime":1703000000000,"CycleCapacityUsedPrecise":"2.5","CycleCapacitySize":100},{"CycleEndTime":1700700000000,"DeductionEndTime":1700700000000,"CapacitySize":20}]}}}}"#;
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
            "codebuddy-cn",
            Some("secret"),
        )
        .await;
        task.await.unwrap();
        assert_eq!(result["quotas"]["Weekly"]["used"], 2.5);
        assert_eq!(result["quotas"]["Bonus Pack 1"]["total"], 20.0);
    }
}
