use crate::services::usage_shared::to_finite_number;
use serde_json::{Value, json};

fn monthly_reset(created: &Value, now_ms: i64) -> Option<String> {
    let anchor = crate::services::usage_shared::parse_reset_time(created)?;
    let now = crate::services::usage_shared::unix_millis_iso(now_ms)?;
    let year: i32 = anchor.get(0..4)?.parse().ok()?;
    let month: i32 = anchor.get(5..7)?.parse().ok()?;
    let day: u32 = anchor.get(8..10)?.parse().ok()?;
    let current_year: i32 = now.get(0..4)?.parse().ok()?;
    let current_month: i32 = now.get(5..7)?.parse().ok()?;
    let elapsed = ((current_year - year) * 12 + current_month - month).max(0);
    for delta in elapsed..=elapsed + 1 {
        let index = year * 12 + month - 1 + delta;
        let y = index.div_euclid(12);
        let m = index.rem_euclid(12) + 1;
        let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
        let max = match m {
            2 => {
                if leap {
                    29
                } else {
                    28
                }
            }
            4 | 6 | 9 | 11 => 30,
            _ => 31,
        };
        let candidate = format!(
            "{y:04}-{m:02}-{:02}{}.000Z",
            day.min(max),
            anchor.get(10..19)?
        );
        if crate::services::usage_shared::reset_epoch_millis(&json!(candidate))? > now_ms {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn monthly_signup_reset_clamps_month_end() {
        let now = crate::services::usage_shared::reset_epoch_millis(&json!("2026-02-01T00:00:00Z"))
            .unwrap();
        assert_eq!(
            monthly_reset(&json!("2025-01-31T12:00:00Z"), now).as_deref(),
            Some("2026-02-28T12:00:00.000Z")
        );
    }
}
pub async fn qoder(client: &reqwest::Client, url: &str, token: &str) -> Value {
    if token.is_empty() {
        return json!({"message":"Qoder usage unavailable: no access token"});
    }
    let response = match client
        .get(url)
        .bearer_auth(token)
        .header("accept", "application/json")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return json!({"message":format!("Qoder connected. Unable to fetch usage: {e}")}),
    };
    if !response.status().is_success() {
        return json!({"message":format!("Qoder connected. Usage fetch returned {}.",response.status().as_u16())});
    }
    let data: Value = match serde_json::from_str(&response.text().await.unwrap_or_default()) {
        Ok(v) => v,
        Err(_) => return json!({"message":"Qoder connected. Usage response was not JSON."}),
    };
    let expires = crate::services::usage_shared::to_finite_number(&data["expiresAt"], 0.0);
    let never = expires == 253402214400000.0;
    let reset = if expires > 0.0 && !never {
        crate::services::usage_shared::unix_millis_iso(expires as i64)
    } else {
        None
    };
    let row = |v: &Value| json!({"total":to_finite_number(&v["total"],0.0),"used":to_finite_number(&v["used"],0.0),"remaining":to_finite_number(&v["remaining"],0.0),"unit":v["unit"].as_str().unwrap_or("credits"),"resetAt":reset,"neverExpires":never});
    let mut quotas = serde_json::Map::new();
    quotas.insert("user".into(), row(&data["userQuota"]));
    quotas.insert("organization".into(), row(&data["orgResourcePackage"]));
    let addon = &data["addOnQuota"];
    if to_finite_number(&addon["total"], 0.0) > 0.0 {
        let mut q = row(addon);
        q["resetAt"] = Value::Null;
        q["neverExpires"] = json!(false);
        q["detailUrl"] = addon["detailUrl"].clone();
        quotas.insert("addOn".into(), q);
    }
    json!({"quotas":quotas,"totalUsagePercentage":to_finite_number(&data["totalUsagePercentage"],0.0),"isQuotaExceeded":data["isQuotaExceeded"]==true,"expiresAt":if expires>0.0{Some(expires)}else{None}})
}

pub async fn ollama(client: &reqwest::Client, base: &str, key: &str) -> Value {
    if key.is_empty() {
        return json!({"message":"Ollama Cloud API key not available."});
    }
    let r = match client
        .get(format!("{base}/api/usage"))
        .bearer_auth(key)
        .header("accept", "application/json")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return json!({"message":format!("Ollama Cloud error: {e}")}),
    };
    let status = r.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return json!({"message":"Ollama Cloud API key invalid or expired."});
    }
    if !status.is_success() {
        return json!({"message":format!("Ollama Cloud usage API error ({}).",status.as_u16())});
    }
    let data: Value = match serde_json::from_str(&r.text().await.unwrap_or_default()) {
        Ok(v) => v,
        Err(_) => return json!({"message":"Ollama Cloud usage response was not JSON."}),
    };
    let me = match client
        .post(format!("{base}/api/me"))
        .bearer_auth(key)
        .header("accept", "application/json")
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => {
            serde_json::from_str::<Value>(&r.text().await.unwrap_or_default())
                .unwrap_or(Value::Null)
        }
        _ => Value::Null,
    };
    let raw = me["Plan"].as_str().unwrap_or("");
    let plan = if raw.is_empty() {
        "Ollama Cloud".to_owned()
    } else {
        let mut c = raw.chars();
        format!(
            "{}{}",
            c.next().unwrap().to_uppercase(),
            c.as_str().to_lowercase()
        )
    };
    let mut quotas = serde_json::Map::new();
    let reset = if raw.eq_ignore_ascii_case("free") {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        monthly_reset(&me["CreatedAt"], now)
    } else {
        None
    };
    for (k, label) in [
        ("session", "Session (5h)"),
        ("weekly", "Weekly (7d)"),
        ("monthly", "Monthly"),
    ] {
        let ratio = to_finite_number(&data["limits"][k]["usage"], f64::NAN);
        if ratio.is_finite() {
            let used = (ratio.clamp(0.0, 1.0) * 100.0).round();
            quotas.insert(label.into(),json!({"used":used,"total":100,"remainingPercentage":100.0-used,"resetAt":if k == "monthly" {reset.clone()} else {None},"unlimited":false}));
        }
    }
    if quotas.is_empty() {
        if let Ok(r) = client
            .get(format!("{base}/api/balance"))
            .bearer_auth(key)
            .header("accept", "application/json")
            .send()
            .await
        {
            if r.status().is_success() {
                let data: Value = serde_json::from_str(&r.text().await.unwrap_or_default())
                    .unwrap_or(Value::Null);
                let inc = &data["included"];
                for (k, label) in [("session", "Session (5h)"), ("weekly", "Weekly (7d)")] {
                    if inc[k].is_object() {
                        let rem =
                            to_finite_number(&inc[k]["remaining_percent"], 100.0).clamp(0.0, 100.0);
                        quotas.insert(label.into(),json!({"used":((100.0-rem)*100.0).round()/100.0,"total":100,"remainingPercentage":rem,"resetAt":inc[k]["resets_at"],"unlimited":false}));
                    }
                }
                if !inc["allowance_usd"].is_null() && !inc["balance_usd"].is_null() {
                    let total = to_finite_number(&inc["allowance_usd"], 0.0);
                    let balance = to_finite_number(&inc["balance_usd"], 0.0);
                    quotas.insert("Allowance ($)".into(),json!({"used":((total-balance)*100.0).round().max(0.0)/100.0,"total":total,"remainingPercentage":if total>0.0{(balance/total*10000.0).round()/100.0}else{0.0},"resetAt":inc["period"]["until"],"unlimited":false,"unit":"USD"}));
                }
                let purchased = to_finite_number(&data["purchased"]["balance_usd"], 0.0);
                if purchased > 0.0 {
                    quotas.insert("Purchased Credits".into(),json!({"used":0,"total":purchased,"remainingPercentage":100,"isCreditBalance":true,"currency":"$"}));
                }
            }
        }
    }
    if quotas.is_empty() {
        json!({"plan":plan,"message":"Ollama Cloud connected. No usage limits reported.","quotas":{}})
    } else {
        json!({"plan":plan,"quotas":quotas})
    }
}
