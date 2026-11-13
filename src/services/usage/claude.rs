use crate::services::usage_shared::{parse_reset_time, to_finite_number};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

static CACHE: OnceLock<Mutex<HashMap<String, (Instant, Value)>>> = OnceLock::new();
static COOLDOWN: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
static INFLIGHT: OnceLock<Mutex<HashMap<String, std::sync::Weak<tokio::sync::Mutex<()>>>>> =
    OnceLock::new();
fn cache() -> &'static Mutex<HashMap<String, (Instant, Value)>> {
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}
fn headers(request: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
    request
        .bearer_auth(token)
        .header("anthropic-beta", "oauth-2025-04-20")
        .header("anthropic-version", "2023-06-01")
        .header("user-agent", "claude-cli/2.1.280 (external, cli)")
}
fn grants(block: &Value) -> Value {
    let Some(all) = block["grants"]
        .as_array()
        .filter(|_| block["eligible"] == true)
    else {
        return Value::Null;
    };
    let usable: Vec<_> = all
        .iter()
        .filter(|g| {
            g["id"].as_str().is_some_and(|s| !s.is_empty())
                && g["paused"] != true
                && to_finite_number(&g["resets_left"], 0.0) > 0.0
        })
        .collect();
    let next = usable
        .iter()
        .copied()
        .find(|g| g["id"] == block["next_grant_id"])
        .or_else(|| usable.first().copied())
        .unwrap_or(&Value::Null);
    let items:Vec<_>=all.iter().filter(|g|g["id"].as_str().is_some_and(|s|!s.is_empty())).map(|g|json!({"id":g["id"],"label":g["label"].as_str().unwrap_or(""),"resetsLeft":to_finite_number(&g["resets_left"],0.0),"resetsTotal":to_finite_number(&g["resets_total"],0.0),"startsAt":g["starts_at"],"endsAt":g["ends_at"],"clears":g["clears"].as_array().cloned().unwrap_or_default(),"paused":g["paused"]==true,"usableNow":g["usable_now"]==true,"useRequiresLimit":g["use_requires_limit"]!=false})).collect();
    json!({"availableCount":usable.iter().map(|g|to_finite_number(&g["resets_left"],0.0)).sum::<f64>(),"nextGrantId":next["id"],"expiresAt":next["ends_at"],"clears":next["clears"].as_array().cloned().unwrap_or_default(),"cooldownUntil":block["cooldown_until"],"weeklyResetsAt":block["weekly_resets_at"],"grants":items})
}
fn parse(data: &Value) -> Value {
    let mut quotas = serde_json::Map::new();
    let mut add = |name: String, w: &Value| {
        if let Some(used) = w["utilization"].as_f64() {
            let remaining = (100.0 - used).max(0.0);
            quotas.insert(name,json!({"used":used,"total":100,"remaining":remaining,"remainingPercentage":remaining,"resetAt":parse_reset_time(&w["resets_at"]),"unlimited":false}));
        }
    };
    add("session (5h)".into(), &data["five_hour"]);
    add("weekly (7d)".into(), &data["seven_day"]);
    if let Some(obj) = data.as_object() {
        for (k, v) in obj {
            if let Some(name) = k.strip_prefix("seven_day_") {
                add(format!("weekly {name} (7d)"), v);
            }
        }
    }
    if let Some(limits) = data["limits"].as_array() {
        for l in limits {
            if l["kind"] != "weekly_scoped" {
                continue;
            }
            let name = l["scope"]["model"]["display_name"]
                .as_str()
                .unwrap_or("")
                .trim()
                .to_lowercase();
            if name.is_empty() {
                continue;
            }
            if let Some(percent) = l["percent"].as_f64() {
                add(
                    format!("weekly {name} (7d)"),
                    &json!({"utilization":percent.clamp(0.0,100.0),"resets_at":l["resets_at"]}),
                );
            }
        }
    }
    json!({"plan":"Claude Code","extraUsage":data["extra_usage"],"resetCredits":grants(&data["cedar_ember"]),"quotas":quotas})
}
pub async fn get_usage(
    client: &reqwest::Client,
    oauth: &str,
    settings: &str,
    org_url: &str,
    token: &str,
    force: bool,
) -> Value {
    let lock = {
        let mut locks = INFLIGHT
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap();
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(token).and_then(|lock| lock.upgrade()) {
            lock
        } else {
            let lock = std::sync::Arc::new(tokio::sync::Mutex::new(()));
            locks.insert(token.to_owned(), std::sync::Arc::downgrade(&lock));
            lock
        }
    };
    let _guard = lock.lock().await;
    let stale = cache().lock().unwrap().get(token).cloned();
    if !force {
        if let Some((at, value)) = &stale {
            if at.elapsed() < Duration::from_secs(300) {
                return value.clone();
            }
        }
    }
    let cooldown = COOLDOWN
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap()
        .get(token)
        .is_some_and(|t| t.elapsed() < Duration::from_secs(180));
    if !cooldown {
        match headers(client.get(format!("{oauth}?cedar_ember=1")), token)
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => {
                match serde_json::from_str::<Value>(&response.text().await.unwrap_or_default()){Ok(data)=>{let result=parse(&data);cache().lock().unwrap().insert(token.to_owned(),(Instant::now(),result.clone()));return result;},Err(e)=>return stale.map(|(_,v)|v).unwrap_or_else(||json!({"message":format!("Claude connected. Unable to fetch usage: {e}")}))}
            }
            Ok(response) => {
                if response.status().as_u16() == 429 {
                    COOLDOWN
                        .get()
                        .unwrap()
                        .lock()
                        .unwrap()
                        .insert(token.to_owned(), Instant::now());
                }
            }
            Err(e) => {
                return stale.map(|(_, v)| v).unwrap_or_else(
                    || json!({"message":format!("Claude connected. Unable to fetch usage: {e}")}),
                );
            }
        }
    }
    let result=async {let r=client.get(settings).bearer_auth(token).header("anthropic-version","2023-06-01").send().await.map_err(|e|e.to_string())?;if !r.status().is_success(){return Ok(json!({"message":"Claude connected. Usage API requires admin permissions."}));}let data:Value=serde_json::from_str(&r.text().await.map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;if let Some(org)=data["organization_id"].as_str(){let r=client.get(org_url.replace("{org_id}",org)).bearer_auth(token).header("anthropic-version","2023-06-01").send().await.map_err(|e|e.to_string())?;if r.status().is_success(){let usage:Value=serde_json::from_str(&r.text().await.map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;return Ok(json!({"plan":data["plan"].as_str().unwrap_or("Unknown"),"organization":data["organization_name"],"quotas":usage}));}}Ok(json!({"plan":data["plan"].as_str().unwrap_or("Unknown"),"organization":data["organization_name"],"message":"Claude connected. Usage details require admin access."}))}.await.unwrap_or_else(|e:String|json!({"message":format!("Claude connected. Unable to fetch usage: {e}")}));
    if result.get("quotas").is_some() {
        cache()
            .lock()
            .unwrap()
            .insert(token.to_owned(), (Instant::now(), result.clone()));
        return result;
    }
    if !force {
        return stale.map(|(_, v)| v).unwrap_or(result);
    }
    result
}
pub async fn consume_reset_grant(
    client: &reqwest::Client,
    profile_url: &str,
    reset_url: &str,
    token: &str,
    id: &str,
) -> Result<Value, String> {
    if token.is_empty() {
        return Err("No Claude access token available. Please re-authorize the connection.".into());
    }
    if id.is_empty()
        || id.len() > 40
        || !id
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_' || c == b'-')
    {
        return Err("Invalid reset grant id.".into());
    }
    let response = headers(client.get(profile_url), token)
        .header("content-type", "application/json")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = response.status();
    let profile: Value =
        serde_json::from_str(&response.text().await.unwrap_or_default()).unwrap_or(Value::Null);
    let org = profile["organization"]["uuid"]
        .as_str()
        .filter(|s| !s.is_empty());
    if !status.is_success() || org.is_none() {
        return Err(format!(
            "Cannot resolve Claude organization ({}).",
            status.as_u16()
        ));
    }
    let response = headers(
        client.post(reset_url.replace("{org_id}", org.unwrap())),
        token,
    )
    .header("content-type", "application/json")
    .body(
        json!({"program":"cedar_ember","grant_id":id,"request_id":super::request_id()?})
            .to_string(),
    )
    .send()
    .await
    .map_err(|e| e.to_string())?;
    let status = response.status();
    let data: Value =
        serde_json::from_str(&response.text().await.unwrap_or_default()).unwrap_or(Value::Null);
    cache().lock().unwrap().remove(token);
    Ok(
        json!({"ok":status.is_success()&&data["result"]=="reset","status":status.as_u16(),"result":data["result"],"reason":data["reason"],"resetsLeft":data["resets_left"],"message":data["error"]["message"]}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    #[tokio::test]
    async fn oauth_quota() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let a = l.local_addr().unwrap();
        let t = tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut b = [0; 2048];
            let n = s.read(&mut b).await.unwrap();
            assert!(String::from_utf8_lossy(&b[..n]).contains("cedar_ember=1"));
            let body = r#"{"five_hour":{"utilization":60},"cedar_ember":{"eligible":true,"grants":[{"id":"g1","resets_left":2}]}}"#;
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
        let result = get_usage(
            &reqwest::Client::new(),
            &format!("http://{a}"),
            "",
            "",
            "secret",
            true,
        )
        .await;
        t.await.unwrap();
        assert_eq!(result["quotas"]["session (5h)"]["remaining"], 40.0);
        assert_eq!(result["resetCredits"]["availableCount"], 2.0);
    }
}
