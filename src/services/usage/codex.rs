use crate::services::usage_shared::{parse_reset_time, to_finite_number};
use serde_json::{Value, json};

fn first<'a>(value: &'a Value, keys: &[&str]) -> &'a Value {
    keys.iter()
        .filter_map(|k| value.get(k))
        .find(|v| !v.is_null())
        .unwrap_or(&Value::Null)
}
fn body(snapshot: &Value) -> &Value {
    if snapshot["rate_limit"].is_object() {
        &snapshot["rate_limit"]
    } else {
        snapshot
    }
}
fn append(quotas: &mut serde_json::Map<String, Value>, prefix: &str, snap: &Value) {
    if !snap.is_object() {
        return;
    }
    for (keys, suffix) in [
        (&["primary_window", "primary"][..], "session"),
        (&["secondary_window", "secondary"][..], "weekly"),
    ] {
        let mut window = first(body(snap), keys);
        if window.is_null() {
            window = first(snap, keys);
        }
        if window.is_null() {
            continue;
        }
        let used = to_finite_number(first(window, &["used_percent", "percent_used"]), 0.0)
            .clamp(0.0, 100.0);
        let name = if prefix.is_empty() {
            suffix.to_owned()
        } else {
            format!("{prefix}_{suffix}")
        };
        quotas.insert(name,json!({"used":used,"total":100,"remaining":100.0-used,"resetAt":parse_reset_time(first(window,&["reset_at","resets_at","resetAt"])),"unlimited":false}));
    }
}
fn extra<'a>(data: &'a Value, kind: &str) -> &'a Value {
    let direct = if kind == "review" {
        first(data, &["code_review_rate_limit", "review_rate_limit"])
    } else {
        first(
            data,
            &["spark_rate_limit", "gpt_5_3_codex_spark_rate_limit"],
        )
    };
    if !direct.is_null() {
        return direct;
    }
    let by = &data["rate_limits_by_limit_id"];
    let direct = if kind == "review" {
        first(by, &["code_review", "codex_review", "review"])
    } else {
        first(by, &["gpt-5.3-codex-spark", "gpt_5_3_codex_spark", "spark"])
    };
    if !direct.is_null() {
        return direct;
    }
    data["additional_rate_limits"]
        .as_array()
        .and_then(|a| {
            a.iter().find(|entry| {
                first(entry, &["limit_name", "metered_feature", "id"])
                    .as_str()
                    .unwrap_or("")
                    .to_lowercase()
                    .contains(kind)
            })
        })
        .unwrap_or(&Value::Null)
}
pub async fn get_usage(client: &reqwest::Client, url: &str, token: &str) -> Result<Value, String> {
    let response = client
        .get(url)
        .bearer_auth(token)
        .header("accept", "application/json")
        .send()
        .await
        .map_err(|e| format!("Failed to fetch Codex usage: {e}"))?;
    if !response.status().is_success() {
        return Ok(
            json!({"message":format!("Codex connected. Usage API temporarily unavailable ({}).",response.status().as_u16())}),
        );
    }
    let data: Value = serde_json::from_str(
        &response
            .text()
            .await
            .map_err(|e| format!("Failed to fetch Codex usage: {e}"))?,
    )
    .map_err(|e| format!("Failed to fetch Codex usage: {e}"))?;
    let normal = first(&data, &["rate_limit", "rate_limits"]);
    let normal = if normal.is_null() {
        &data["rate_limits_by_limit_id"]["codex"]
    } else {
        normal
    };
    let review = extra(&data, "review");
    let spark = extra(&data, "spark");
    let mut quotas = serde_json::Map::new();
    append(&mut quotas, "", normal);
    append(&mut quotas, "review", review);
    append(&mut quotas, "spark", spark);
    Ok(
        json!({"plan":data["plan_type"].as_str().or_else(||data["summary"]["plan"].as_str()).unwrap_or("unknown"),"limitReached":body(normal)["limit_reached"].as_bool().unwrap_or(false),"reviewLimitReached":body(review)["limit_reached"].as_bool().unwrap_or(false),"sparkLimitReached":body(spark)["limit_reached"].as_bool().unwrap_or(false),"resetCredits":{"availableCount":to_finite_number(&data["rate_limit_reset_credits"]["available_count"],0.0).max(0.0)},"quotas":quotas}),
    )
}
pub async fn get_reset_credits(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    data: &Value,
) -> Result<Value, String> {
    if token.is_empty() {
        return Err("No Codex access token available. Please re-authorize the connection.".into());
    }
    let mut request = client
        .get(url)
        .bearer_auth(token)
        .header("accept", "application/json")
        .header("openai-beta", "codex-1")
        .header("originator", "codex_cli_rs");
    if let Some(id) = first(data, &["workspaceId", "accountId", "chatgptAccountId"]).as_str() {
        request = request.header("chatgpt-account-id", id);
    }
    let response = request.send().await.map_err(|e| e.to_string())?;
    let status = response.status();
    let data: Value =
        serde_json::from_str(&response.text().await.unwrap_or_default()).unwrap_or(Value::Null);
    if !status.is_success() {
        return Err(first(&data, &["message", "error", "detail"])
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| {
                format!("Codex reset credits API unavailable ({}).", status.as_u16())
            }));
    }
    let credits:Vec<_>=data["credits"].as_array().into_iter().flatten().map(|c|json!({"status":c["status"].as_str().unwrap_or("unknown"),"grantedAt":parse_reset_time(first(c,&["granted_at","grantedAt"])),"expiresAt":parse_reset_time(first(c,&["expires_at","expiresAt"]))})).collect();
    Ok(
        json!({"availableCount":to_finite_number(first(&data,&["available_count","availableCount"]),0.0).max(0.0),"credits":credits}),
    )
}
// Consumes a credit upstream. Call only from an explicitly requested reset action.
pub async fn consume_reset_credit(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    id: &str,
) -> Result<Value, String> {
    if token.is_empty() {
        return Err("No Codex access token available. Please re-authorize the connection.".into());
    }
    if id.is_empty() {
        return Err("A redeem request id is required to consume a Codex reset credit.".into());
    }
    let response = client
        .post(url)
        .bearer_auth(token)
        .header("accept", "application/json")
        .header("content-type", "application/json")
        .body(json!({"redeem_request_id":id}).to_string())
        .send()
        .await
        .map_err(|e| format!("Failed to consume Codex reset credit: {e}"))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|e| format!("Failed to consume Codex reset credit: {e}"))?;
    let data: Value = if text.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text)
            .map_err(|e| format!("Failed to consume Codex reset credit: {e}"))?
    };
    let code = data["code"].as_str();
    let count = to_finite_number(&data["windows_reset"], 0.0);
    Ok(
        json!({"ok":status.is_success()&&(code==Some("reset")||count>0.0),"noCredit":status.is_success()&&code==Some("no_credit"),"status":status.as_u16(),"code":code,"windowsReset":count,"message":data["message"],"raw":data}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    #[tokio::test]
    async fn quota_response() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let a = l.local_addr().unwrap();
        let t = tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut b = [0; 2048];
            let n = s.read(&mut b).await.unwrap();
            assert!(
                String::from_utf8_lossy(&b[..n])
                    .to_lowercase()
                    .contains("authorization: bearer token")
            );
            let body = r#"{"plan_type":"plus","rate_limit":{"primary_window":{"used_percent":25}},"rate_limit_reset_credits":{"available_count":2}}"#;
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
        let result = get_usage(&reqwest::Client::new(), &format!("http://{a}"), "token")
            .await
            .unwrap();
        t.await.unwrap();
        assert_eq!(result["quotas"]["session"]["remaining"], 75.0);
        assert_eq!(result["resetCredits"]["availableCount"], 2.0);
    }

    #[tokio::test]
    async fn reset_credit_consume_sends_redeem_id() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut chunk = [0; 2048];
                let n = stream.read(&mut chunk).await.unwrap();
                request.extend_from_slice(&chunk[..n]);
                if n == 0
                    || request
                        .windows(b"\"redeem_request_id\":\"test\"".len())
                        .any(|w| w == b"\"redeem_request_id\":\"test\"")
                {
                    break;
                }
            }
            assert!(String::from_utf8_lossy(&request).contains("\"redeem_request_id\":\"test\""));
            let body = r#"{"code":"reset","windows_reset":1}"#;
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
        let result = consume_reset_credit(
            &reqwest::Client::new(),
            &format!("http://{addr}"),
            "token",
            "test",
        )
        .await
        .unwrap();
        task.await.unwrap();
        assert_eq!(result["ok"], true);
        assert_eq!(result["windowsReset"], 1.0);
    }
}
