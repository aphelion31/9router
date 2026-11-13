use crate::services::usage_shared::{parse_reset_time, to_finite_number, unix_millis_iso};
use serde_json::{Value, json};
fn varint(b: &[u8], p: &mut usize) -> Option<u64> {
    let mut n = 0u64;
    for shift in (0..70).step_by(7) {
        let x = *b.get(*p)?;
        *p += 1;
        if shift >= 64 && x & 127 != 0 {
            return None;
        }
        if shift < 64 {
            n |= ((x & 127) as u64) << shift;
        }
        if x & 128 == 0 {
            return Some(n);
        }
    }
    None
}
fn fields(mut b: &[u8]) -> Option<Vec<(u64, u8, Vec<u8>, u64)>> {
    let mut out = Vec::new();
    while !b.is_empty() {
        let mut p = 0;
        let tag = varint(b, &mut p)?;
        if tag >> 3 == 0 {
            return None;
        }
        let wire = (tag & 7) as u8;
        let mut num = 0;
        let bytes = match wire {
            0 => {
                num = varint(b, &mut p)?;
                vec![]
            }
            1 => {
                let v = b.get(p..p + 8)?.to_vec();
                p += 8;
                v
            }
            2 => {
                let len = usize::try_from(varint(b, &mut p)?).ok()?;
                let end = p.checked_add(len)?;
                let v = b.get(p..end)?.to_vec();
                p = end;
                v
            }
            5 => {
                let v = b.get(p..p + 4)?.to_vec();
                p += 4;
                v
            }
            _ => return None,
        };
        out.push((tag >> 3, wire, bytes, num));
        b = &b[p..];
    }
    Some(out)
}
pub fn decode_frame(buffer: &[u8]) -> Option<(f64, Option<String>)> {
    let b = if buffer.len() >= 5 && matches!(buffer[0], 0 | 1 | 128 | 129) {
        let mut pos = 0;
        loop {
            let flag = *buffer.get(pos)?;
            let len = u32::from_be_bytes(buffer.get(pos + 1..pos + 5)?.try_into().ok()?) as usize;
            let start = pos + 5;
            let end = start.checked_add(len)?;
            let body = buffer.get(start..end)?;
            if flag & 128 == 0 {
                break body;
            }
            pos = end;
        }
    } else {
        buffer
    };
    let top = fields(b)?;
    let nested = top.iter().find(|x| x.0 == 1 && x.1 == 2)?;
    let entries = fields(&nested.2)?;
    let ratio = match entries.iter().find(|x| x.0 == 1) {
        None => 0.0,
        Some(x) if x.1 == 5 => f32::from_le_bytes(x.2.as_slice().try_into().ok()?) as f64,
        Some(x) if x.1 == 1 => f64::from_le_bytes(x.2.as_slice().try_into().ok()?),
        _ => return None,
    };
    if !ratio.is_finite() || ratio < 0.0 {
        return None;
    }
    let reset = entries.iter().find(|x| x.0 == 5 && x.1 == 2).and_then(|x| {
        let t = fields(&x.2)?;
        let sec = t
            .iter()
            .find(|x| x.0 == 1 && x.1 == 0)
            .map(|x| x.3)
            .unwrap_or(0);
        let ns = t
            .iter()
            .find(|x| x.0 == 2 && x.1 == 0)
            .map(|x| x.3)
            .unwrap_or(0);
        unix_millis_iso(
            i64::try_from(sec)
                .ok()?
                .checked_mul(1000)?
                .checked_add(((ns as f64) / 1e6).round() as i64)?,
        )
    });
    Some(((ratio * 100.0).min(100.0), reset))
}
fn number(v: &Value) -> f64 {
    to_finite_number(if v.is_object() { &v["val"] } else { v }, f64::NAN)
}
fn first<'a>(v: &'a Value, keys: &[&str]) -> &'a Value {
    keys.iter()
        .filter_map(|k| v.get(k))
        .find(|v| !v.is_null())
        .unwrap_or(&Value::Null)
}
fn row(used: f64, total: f64, reset: &Option<String>) -> Value {
    json!({"used":used.max(0.0),"total":total.max(0.0),"remainingPercentage":if total>0.0{((total-used)/total*100.0).max(0.0)}else{0.0},"resetAt":reset,"unlimited":total<=0.0})
}
fn request(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    psd: &Value,
) -> reqwest::RequestBuilder {
    let mut r = client
        .get(url)
        .bearer_auth(token)
        .header("accept", "application/json")
        .header("x-xai-token-auth", "xai-grok-cli")
        .header("x-grok-client-mode", "headless");
    r = r
        .header("user-agent", "grok-shell/1.0.44 (linux; x86_64)")
        .header("x-grok-client-identifier", "grok-shell")
        .header("x-grok-client-version", "1.0.44");
    if let Some(v) = psd["email"].as_str() {
        r = r.header("x-email", v);
    }
    if let Some(v) = first(psd, &["userId", "principalId"]).as_str() {
        r = r.header("x-userid", v);
    }
    r
}
pub async fn get_usage(
    client: &reqwest::Client,
    billing_url: &str,
    user_url: &str,
    grpc_url: &str,
    token: &str,
    psd: &Value,
) -> Value {
    if token.is_empty() {
        return json!({"message":"Grok CLI access token not available."});
    }
    let (billing, user) = tokio::join!(
        request(client, billing_url, token, psd).send(),
        request(client, user_url, token, psd).send()
    );
    let billing = match billing {
        Ok(r) => r,
        Err(e) => return json!({"message":format!("Grok CLI usage error: {e}")}),
    };
    let status = billing.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return json!({"message":"Grok CLI authentication expired. Please re-authorize."});
    }
    let text = billing.text().await.unwrap_or_default();
    if !status.is_success() {
        let detail: String = text.chars().take(200).collect();
        return json!({"message":format!("Grok CLI billing API error ({}){}",status.as_u16(),if detail.is_empty(){String::new()}else{format!(": {detail}")})});
    }
    let root: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => return json!({"message":"Grok CLI billing response was not JSON."}),
    };
    let user = match user {
        Ok(r) if r.status().is_success() => {
            serde_json::from_str::<Value>(&r.text().await.unwrap_or_default())
                .unwrap_or(Value::Null)
        }
        _ => Value::Null,
    };
    let config = if root["config"].is_object() {
        &root["config"]
    } else {
        &root
    };
    let tier = first(&user, &["subscriptionTier", "subscription_tier"])
        .as_str()
        .or_else(|| user["subscription"]["tier"].as_str())
        .or_else(|| first(config, &["subscriptionTier", "subscription_tier"]).as_str())
        .unwrap_or("");
    let subscribed =
        !tier.is_empty() && !matches!(tier.to_lowercase().as_str(), "free" | "none" | "null");
    let mut plan = if !tier.is_empty() {
        tier.replace(['_', '-'], " ")
            .split_whitespace()
            .map(|w| {
                let mut c = w.chars();
                format!("{}{}", c.next().unwrap().to_uppercase(), c.as_str())
            })
            .collect::<Vec<_>>()
            .join(" ")
    } else if user["hasGrokCodeAccess"] == true {
        "Grok Code".into()
    } else {
        "Grok Build".into()
    };
    use base64::Engine;
    if let Some(payload) = token
        .split('.')
        .nth(1)
        .and_then(|s| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(s)
                .ok()
        })
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
    {
        if let Some(label) = match payload["tier"].as_i64() {
            Some(0) => Some("Free"),
            Some(1) => Some("SuperGrok"),
            Some(2) => Some("X Basic"),
            Some(3) => Some("X Premium"),
            Some(4) => Some("X Premium Plus"),
            Some(5) => Some("SuperGrok Heavy"),
            Some(6) => Some("SuperGrok Lite"),
            _ => None,
        } {
            plan = label.to_owned();
        }
    }
    let reset = parse_reset_time(first(
        config,
        &[
            "billingPeriodEnd",
            "billing_period_end",
            "resetAt",
            "resetsAt",
            "periodEnd",
        ],
    ))
    .or_else(|| parse_reset_time(&config["currentPeriod"]["end"]));
    let mut quotas = serde_json::Map::new();
    let total = number(first(config, &["monthlyLimit", "monthly_limit"]));
    let used = number(first(
        config,
        &["includedUsed", "included_used", "totalUsed", "total_used"],
    ));
    if total > 0.0 {
        quotas.insert(
            "Monthly included".into(),
            row(if used.is_finite() { used } else { 0.0 }, total, &reset),
        );
    }
    let cap = number(&config["onDemandCap"]);
    let used = number(&config["onDemandUsed"]);
    if cap > 0.0 {
        quotas.insert(
            "On-demand".into(),
            row(if used.is_finite() { used } else { 0.0 }, cap, &reset),
        );
    } else if !subscribed && cap == 0.0 && used.is_finite() {
        quotas.insert(
            "On-demand".into(),
            json!({"used":1,"total":1,"remainingPercentage":0,"resetAt":reset,"unlimited":false}),
        );
    }
    let prepaid = number(&config["prepaidBalance"]);
    if prepaid > 0.0 {
        quotas.insert("Prepaid".into(),json!({"used":0,"total":prepaid,"remainingPercentage":100,"resetAt":null,"unlimited":false}));
    }
    let pct = number(first(
        config,
        &["creditUsagePercent", "credit_usage_percent"],
    ));
    if pct.is_finite() && pct >= 0.0 {
        quotas.insert(
            "Weekly SuperGrok".into(),
            row(pct.clamp(0.0, 100.0), 100.0, &reset),
        );
    }
    for bag in [
        &root["credits"],
        &root["creditBalance"],
        &root["usage"],
        &config["credits"],
        &config["includedCredits"],
        &config["subscriptionCredits"],
    ] {
        if !bag.is_object() || quotas.contains_key("Credits") {
            continue;
        }
        let total = number(first(
            bag,
            &["total", "limit", "cap", "allocation", "amount"],
        ));
        let used = number(first(bag, &["used", "spent", "consumed"]));
        let remaining = number(first(bag, &["remaining", "balance", "left"]));
        let bag_reset =
            parse_reset_time(first(bag, &["resetAt", "resetsAt", "end"])).or_else(|| reset.clone());
        if total.is_finite() && total > 0.0 {
            let used = if used.is_finite() {
                used
            } else if remaining.is_finite() {
                (total - remaining).max(0.0)
            } else {
                0.0
            };
            quotas.insert("Credits".into(), row(used, total, &bag_reset));
        } else if remaining.is_finite() && remaining >= 0.0 {
            quotas.insert("Credits".into(), json!({"used":0,"total":if remaining>0.0{remaining}else{1.0},"remainingPercentage":if remaining>0.0{100}else{0},"resetAt":reset,"unlimited":false}));
        }
    }
    if quotas.is_empty() {
        if let Ok(r) = client
            .post(grpc_url)
            .bearer_auth(token)
            .header("content-type", "application/grpc-web+proto")
            .header("x-grpc-web", "1")
            .header("accept", "application/grpc-web+proto")
            .body(vec![0u8; 5])
            .send()
            .await
        {
            if r.status().is_success() {
                if let Ok(bytes) = r.bytes().await {
                    if let Some((pct, reset)) = decode_frame(&bytes) {
                        quotas.insert("Weekly SuperGrok".into(), row(pct.round(), 100.0, &reset));
                    }
                }
            }
        }
    }
    if quotas.is_empty() {
        return json!({"plan":plan,"message":if subscribed{"Subscription access is active; Grok does not expose a numeric included quota."}else{"Grok Build connected, but no credit allotment was returned. Free promo may be exhausted."},"quotas":{}});
    }
    json!({"plan":plan,"quotas":quotas})
}
#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    #[test]
    fn frame() {
        let p = [0, 0, 0, 0, 7, 10, 5, 13, 0, 0, 0, 63];
        assert_eq!(decode_frame(&p).unwrap().0, 50.0);
    }
    #[tokio::test]
    async fn billing() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let a = l.local_addr().unwrap();
        let t = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut s, _) = l.accept().await.unwrap();
                let mut b = [0; 2048];
                let _ = s.read(&mut b).await.unwrap();
                let body = r#"{"config":{"onDemandCap":{"val":100},"onDemandUsed":{"val":20}}}"#;
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
            }
        });
        let v = get_usage(
            &reqwest::Client::new(),
            &format!("http://{a}/billing"),
            &format!("http://{a}/user"),
            "",
            "key",
            &Value::Null,
        )
        .await;
        t.await.unwrap();
        assert_eq!(v["quotas"]["On-demand"]["remainingPercentage"], 80.0);
    }
}
