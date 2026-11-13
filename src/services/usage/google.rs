use crate::services::usage_shared::{
    normalize_cloud_code_project_id, parse_reset_time, to_finite_number,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

type WeeklyResult = serde_json::Map<String, Value>;
static WEEKLY: OnceLock<Mutex<HashMap<String, (Instant, WeeklyResult)>>> = OnceLock::new();
static WEEKLY_LOCKS: OnceLock<Mutex<HashMap<String, std::sync::Weak<tokio::sync::Mutex<()>>>>> =
    OnceLock::new();

async fn fetch_weekly(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    payload: Value,
) -> WeeklyResult {
    let key = format!("{token}::{payload}");
    let lock = {
        let mut locks = WEEKLY_LOCKS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap();
        locks.retain(|_, v| v.strong_count() > 0);
        if let Some(lock) = locks.get(&key).and_then(|v| v.upgrade()) {
            lock
        } else {
            let lock = std::sync::Arc::new(tokio::sync::Mutex::new(()));
            locks.insert(key.clone(), std::sync::Arc::downgrade(&lock));
            lock
        }
    };
    let _guard = lock.lock().await;
    let cache = WEEKLY.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some((_, result)) = cache
        .lock()
        .unwrap()
        .get(&key)
        .filter(|(at, _)| at.elapsed() < Duration::from_secs(180))
    {
        return result.clone();
    }
    let result = match post(client, url, token, payload, true).await {
        Ok(r) if r.status().is_success() => {
            let data = serde_json::from_str::<Value>(&r.text().await.unwrap_or_default())
                .unwrap_or(Value::Null);
            weekly(&data)
        }
        _ => WeeklyResult::new(),
    };
    if !result.is_empty() {
        cache
            .lock()
            .unwrap()
            .insert(key, (Instant::now(), result.clone()));
    }
    result
}

fn metadata() -> Value {
    let platform = if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") { 2 } else { 1 }
    } else if cfg!(target_os = "linux") {
        if cfg!(target_arch = "aarch64") { 4 } else { 3 }
    } else if cfg!(target_os = "windows") {
        5
    } else {
        0
    };
    json!({"ideType":9,"platform":platform,"pluginType":2})
}

pub fn weekly(data: &Value) -> serde_json::Map<String, Value> {
    let mut result = serde_json::Map::new();
    let groups = data["groups"]
        .as_array()
        .or_else(|| data["quotaSummary"]["groups"].as_array());
    for group in groups.into_iter().flatten() {
        let name = group["displayName"].as_str().unwrap_or("").to_lowercase();
        let family = if name.contains("gemini") {
            Some(("gemini", "Gemini"))
        } else if name.contains("claude") || name.contains("gpt") {
            Some(("claude_gpt", "Claude & GPT"))
        } else {
            None
        };
        let Some((key, label)) = family else {
            continue;
        };
        for b in group["buckets"].as_array().into_iter().flatten() {
            let window = b["window"].as_str().unwrap_or("").to_lowercase();
            let text = format!(
                "{} {}",
                b["bucketId"].as_str().unwrap_or(""),
                b["displayName"].as_str().unwrap_or("")
            )
            .to_lowercase();
            let weekly = window == "weekly" || text.contains("weekly");
            let session = window == "5h"
                || window == "daily"
                || text.contains("5h")
                || text.contains("five hour")
                || text.contains("daily");
            if !weekly && !session || weekly && b["disabled"] == true {
                continue;
            }
            let fraction = if b["disabled"] == true {
                0.0
            } else {
                to_finite_number(&b["remainingFraction"], f64::NAN)
            };
            if !fraction.is_finite() {
                continue;
            }
            let suffix = if weekly { "weekly" } else { "session" };
            let k = format!("{key}_{suffix}");
            let title = if weekly { "Weekly" } else { "5h" };
            result.entry(k).or_insert(json!({"used":(1000.0-(1000.0*fraction).round()).max(0.0),"total":1000,"resetAt":parse_reset_time(&b["resetTime"]),"remainingPercentage":fraction*100.0,"unlimited":false,"displayName":format!("{label} ({title})")}));
        }
    }
    result
}
async fn post(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    body: Value,
    antigravity: bool,
) -> Result<reqwest::Response, reqwest::Error> {
    let mut req = client
        .post(url)
        .bearer_auth(token)
        .header("content-type", "application/json")
        .timeout(std::time::Duration::from_secs(10))
        .body(body.to_string());
    if antigravity {
        req = req
            .header("user-agent", "antigravity/hub/2.24.0 darwin/arm64")
            .header("x-client-name", "antigravity")
            .header("x-client-version", "2.24.0");
    }
    req.send().await
}
pub async fn gemini(
    client: &reqwest::Client,
    load: &str,
    quota: &str,
    token: &str,
    provider_data: &Value,
) -> Value {
    if token.is_empty() {
        return json!({"plan":"Free","message":"Gemini CLI access token not available."});
    }
    let mut project =
        normalize_cloud_code_project_id(&provider_data["projectId"]).map(str::to_owned);
    let mut plan = "Free".to_owned();
    if project.is_none() {
        if let Ok(r) = post(client, load, token, json!({"metadata":metadata()}), false).await {
            if r.status().is_success() {
                let v: Value = serde_json::from_str(&r.text().await.unwrap_or_default())
                    .unwrap_or(Value::Null);
                project = normalize_cloud_code_project_id(&v["cloudaicompanionProject"])
                    .map(str::to_owned);
                plan = v["currentTier"]["name"]
                    .as_str()
                    .unwrap_or("Free")
                    .to_owned();
            }
        }
    }
    let Some(project) = project else {
        return json!({"plan":plan,"message":"Gemini CLI project ID not available. Reconnect Gemini CLI, or configure a Google Cloud project with Gemini Code Assist access before checking quota."});
    };
    let r = match post(client, quota, token, json!({"project":project}), false).await {
        Ok(r) => r,
        Err(e) => return json!({"message":format!("Gemini CLI error: {e}")}),
    };
    if !r.status().is_success() {
        return json!({"plan":plan,"message":format!("Gemini CLI quota error ({}).",r.status().as_u16())});
    }
    let data: Value = match serde_json::from_str(&r.text().await.unwrap_or_default()) {
        Ok(v) => v,
        Err(e) => return json!({"message":format!("Gemini CLI error: {e}")}),
    };
    let mut quotas = serde_json::Map::new();
    for b in data["buckets"].as_array().into_iter().flatten() {
        let Some(id) = b["modelId"].as_str().filter(|s| !s.is_empty()) else {
            continue;
        };
        if b["remainingFraction"].is_null() {
            continue;
        }
        let fraction = to_finite_number(&b["remainingFraction"], 0.0);
        quotas.insert(id.into(),json!({"used":(1000.0-(1000.0*fraction).round()).max(0.0),"total":1000,"resetAt":parse_reset_time(&b["resetTime"]),"remainingPercentage":fraction*100.0,"unlimited":false}));
    }
    json!({"plan":plan,"quotas":quotas})
}
pub async fn antigravity(
    client: &reqwest::Client,
    load: &str,
    quota: &str,
    summary: &str,
    token: &str,
) -> Value {
    let sub = match post(
        client,
        load,
        token,
        json!({"metadata":metadata(),"mode":1}),
        true,
    )
    .await
    {
        Ok(r) if r.status().is_success() => {
            serde_json::from_str::<Value>(&r.text().await.unwrap_or_default())
                .unwrap_or(Value::Null)
        }
        _ => Value::Null,
    };
    let mut payload = json!({});
    if !sub["cloudaicompanionProject"].is_null() {
        payload["project"] = sub["cloudaicompanionProject"].clone();
    }
    let r = match post(client, quota, token, payload.clone(), true).await {
        Ok(r) => r,
        Err(e) => return json!({"message":format!("Antigravity error: {e}")}),
    };
    let status = r.status().as_u16();
    if status == 401 || status == 403 {
        return json!({"message":if status==401 {"Antigravity quota API authentication expired. Chat may still work."}else{"Antigravity quota API access forbidden. Chat may still work."},"quotas":{}});
    }
    if !r.status().is_success() {
        return json!({"message":format!("Antigravity error: Antigravity API error: {status}")});
    }
    let data: Value = match serde_json::from_str(&r.text().await.unwrap_or_default()) {
        Ok(v) => v,
        Err(e) => return json!({"message":format!("Antigravity error: {e}")}),
    };
    let mut quotas = serde_json::Map::new();
    let paid = sub["paidTier"]["id"]
        .as_str()
        .is_some_and(|s| s != "free-tier" && !s.is_empty());
    if paid {
        if let Some(models) = data["models"].as_object() {
            for (id, info) in models {
                if info["isInternal"] == true || !info["quotaInfo"].is_object() {
                    continue;
                }
                let valid = [
                    "gemini-4-argon-",
                    "gemini-3.8-flash-",
                    "gemini-3.7-flash-",
                    "gemini-3.6-flash-",
                    "gemini-3.1-pro-",
                    "gemini-pro-agent",
                    "claude-sonnet-5-5",
                    "claude-opus-5-5",
                    "claude-sonnet-4-6",
                    "claude-opus-4-6-thinking",
                    "gpt-oss-120b-medium",
                    "gemini-3.1-flash-image",
                ]
                .iter()
                .any(|p| id.starts_with(p));
                if !valid {
                    continue;
                }
                let q = &info["quotaInfo"];
                let fraction = to_finite_number(&q["remainingFraction"], 0.0);
                quotas.insert(id.clone(),json!({"used":1000.0-(1000.0*fraction).round(),"total":1000,"resetAt":parse_reset_time(&q["resetTime"]),"remainingPercentage":fraction*100.0,"unlimited":false,"displayName":info["displayName"].as_str().unwrap_or(id)}));
            }
        }
    }
    {
        let mut overlay = fetch_weekly(client, summary, token, payload).await;
        for (prefix, key) in [
            ("gemini-", "gemini_session"),
            ("claude-", "claude_gpt_session"),
        ] {
            let models: Vec<_> = quotas
                .iter()
                .filter(|(k, _)| k.starts_with(prefix) && !k.contains("image"))
                .map(|(_, v)| v)
                .collect();
            if !models.is_empty()
                && models.iter().all(|q| q["remainingPercentage"] == 0.0)
                && overlay[key]["remainingPercentage"].as_f64().unwrap_or(0.0) > 0.0
            {
                let reset = models.iter().filter_map(|q| q["resetAt"].as_str()).max();
                if let Some(q) = overlay.get_mut(key) {
                    q["used"] = q["total"].clone();
                    q["remainingPercentage"] = json!(0);
                    if let Some(reset) = reset {
                        q["resetAt"] = json!(reset);
                    }
                }
            }
        }
        quotas.extend(overlay);
    }
    json!({"plan":sub["currentTier"]["name"].as_str().unwrap_or("Unknown"),"quotas":quotas,"subscriptionInfo":sub})
}
#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    #[tokio::test]
    async fn gemini_quota() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let a = l.local_addr().unwrap();
        let t = tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut b = [0; 4096];
            let n = s.read(&mut b).await.unwrap();
            assert!(
                String::from_utf8_lossy(&b[..n])
                    .to_lowercase()
                    .contains("authorization: bearer token")
            );
            let body = r#"{"buckets":[{"modelId":"gemini","remainingFraction":0.5}]}"#;
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
        let v = gemini(
            &reqwest::Client::new(),
            "",
            &format!("http://{a}"),
            "token",
            &json!({"projectId":"p"}),
        )
        .await;
        t.await.unwrap();
        assert_eq!(v["quotas"]["gemini"]["used"], 500.0);
    }
}
