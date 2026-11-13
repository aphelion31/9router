use crate::services::usage_shared::{parse_reset_time, to_finite_number};
use serde_json::{Value, json};
fn first<'a>(v: &'a Value, keys: &[&str]) -> &'a Value {
    keys.iter()
        .filter_map(|k| v.get(k))
        .find(|v| !v.is_null())
        .unwrap_or(&Value::Null)
}
fn limit(v: &Value) -> (bool, f64) {
    if v == "unlimited" || v["unlimited"] == true {
        return (true, 0.0);
    }
    let raw = if v.is_object() {
        first(v, &["limited", "Limited"])
    } else {
        v
    };
    (false, to_finite_number(raw, 0.0).max(0.0))
}
fn quota(bucket: &Value, reset: &Option<String>) -> Value {
    let used = to_finite_number(&bucket["used"], 0.0).max(0.0);
    let (unlimited, total) = limit(&bucket["limit"]);
    let used = if total > 0.0 { used.min(total) } else { used };
    json!({"used":used,"total":total,"remainingPercentage":if unlimited{100.0}else if total>0.0{(total-used)/total*100.0}else{0.0},"resetAt":reset,"unlimited":unlimited})
}
pub async fn get_usage(client: &reqwest::Client, url: &str, token: &str, psd: &Value) -> Value {
    if token.trim().is_empty() {
        return json!({"message":"Zed access token not available. Re-connect Zed to view quota."});
    }
    let user = psd["userId"]
        .as_str()
        .map(str::to_owned)
        .or_else(|| psd["userId"].as_i64().map(|i| i.to_string()));
    let Some(user) = user else {
        return json!({"message":"Zed credential is missing user id. Re-connect Zed to view quota."});
    };
    let r = match client
        .get(url)
        .header("authorization", format!("{user} {}", token.trim()))
        .header("accept", "application/json")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return json!({"message":format!("Zed error: {e}")}),
    };
    let status = r.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return json!({"message":"Zed authentication failed. Sign in again from the dashboard or Zed editor."});
    }
    if !status.is_success() {
        return json!({"message":format!("Zed error: HTTP {}",status.as_u16())});
    }
    let data: Value = match serde_json::from_str(&r.text().await.unwrap_or_default()) {
        Ok(v) => v,
        Err(e) => return json!({"message":format!("Zed error: {e}")}),
    };
    let plan = &data["plan"];
    let id = first(plan, &["plan_v3", "plan_v2", "plan"])
        .as_str()
        .or_else(|| data["plan_v3"].as_str());
    let raw = id.unwrap_or("").trim();
    let mut label = match raw.to_lowercase().as_str() {
        "zed_free" => "Zed Free".into(),
        "zed_pro" => "Zed Pro".into(),
        "zed_pro_trial" => "Zed Pro Trial".into(),
        "zed_student" => "Zed Student".into(),
        "zed_business" => "Zed Business".into(),
        "" => "Zed".into(),
        _ => raw
            .replace('_', " ")
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
            .join(" "),
    };
    let trial = !first(plan, &["trial_started_at", "trialStartedAt"]).is_null();
    if trial && !label.to_lowercase().contains("trial") {
        label.push_str(" (Trial active)");
    }
    let overdue = plan["has_overdue_invoices"] == true || plan["hasOverdueInvoices"] == true;
    let reset = parse_reset_time(&plan["subscription_period"]["ended_at"])
        .or_else(|| parse_reset_time(&plan["subscriptionPeriod"]["endedAt"]));
    let usage = &plan["usage"];
    let edit = first(usage, &["edit_predictions", "editPredictions"]);
    let models = first(usage, &["model_requests", "modelRequests"]);
    let mut quotas = serde_json::Map::new();
    if !edit.is_null() {
        quotas.insert("Edit Predictions".into(), quota(edit, &reset));
    }
    let (unlimited, total) = limit(&models["limit"]);
    if !models.is_null() && (unlimited || total > 0.0) {
        quotas.insert("Hosted Model Requests".into(), quota(models, &reset));
    }
    let message = if overdue {
        Some(
            "This Zed account has overdue invoices. Usage may be blocked until billing is resolved.",
        )
    } else if !models.is_null() && !unlimited && total == 0.0 {
        Some(
            "Hosted AI models are billed per token (not request count). Edit Predictions are tracked below. Token spend is on dashboard.zed.dev.",
        )
    } else {
        None
    };
    json!({"plan":label,"quotas":quotas,"message":message,"hasOverdueInvoices":overdue,"trialStarted":trial,"planId":id,"resetAt":reset})
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
                    .contains("authorization: 123 token")
            );
            let body = r#"{"plan":{"plan_v3":"zed_pro","usage":{"edit_predictions":{"used":25,"limit":{"limited":100}}}}}"#;
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
            "token",
            &json!({"userId":123}),
        )
        .await;
        t.await.unwrap();
        assert_eq!(v["quotas"]["Edit Predictions"]["remainingPercentage"], 75.0);
    }
}
