use crate::services::usage_shared::parse_reset_time;
use base64::Engine;
use serde_json::{Value, json};

async fn account_usage(
    client: &reqwest::Client,
    sso: &reqwest::Client,
    psd: &Value,
) -> Option<Value> {
    let pass = psd["mimoPassToken"].as_str().filter(|s| !s.is_empty())?;
    let region = psd["region"].as_str().unwrap_or("sgp").to_ascii_lowercase();
    let (region, sid) = match region.as_str() {
        "cn" => ("cn", "mimopc"),
        "ams" => ("ams", "mimoams"),
        "ru" => ("ru", "mimoru"),
        "in" => ("in", "mimoin"),
        _ => ("sgp", "mimosgp"),
    };
    let mut cookie = format!("passToken={pass}");
    for (key, field) in [("userId", "mimoUserId"), ("cUserId", "mimoCUserId")] {
        if let Some(v) = psd[field].as_str().filter(|v| !v.is_empty()) {
            cookie.push_str(&format!("; {key}={v}"));
        }
    }
    let login = format!(
        "https://account.xiaomi.com/pass/serviceLogin?_locale=zh_CN&_snsNone=true&sid={sid}&_json=true"
    );
    let response = sso
        .get(login)
        .header("cookie", cookie)
        .header("user-agent", "MiClaw/1.0")
        .header("accept", "application/json")
        .send()
        .await
        .ok()?;
    let raw = response.text().await.ok()?;
    let raw = raw.strip_prefix("&&&START&&&").unwrap_or(&raw);
    let data: Value = serde_json::from_str(raw).ok()?;
    if data["code"] != 0 {
        return None;
    }
    let nonce = data["nonce"]
        .as_str()
        .map(str::to_owned)
        .or_else(|| data["nonce"].as_u64().map(|n| n.to_string()))?;
    let secret = data["ssecurity"].as_str()?;
    let signature = ring::digest::digest(
        &ring::digest::SHA1_FOR_LEGACY_USE_ONLY,
        format!("nonce={nonce}&{secret}").as_bytes(),
    );
    let signature = base64::engine::general_purpose::STANDARD.encode(signature.as_ref());
    let mut current = reqwest::Url::parse(data["location"].as_str()?).ok()?;
    current
        .query_pairs_mut()
        .append_pair("clientSign", &signature);
    let mut jar = std::collections::HashMap::<String, String>::new();
    for _ in 0..8 {
        let host = current.host_str()?;
        if current.scheme() != "https"
            || !(host == "xiaomi.com"
                || host.ends_with(".xiaomi.com")
                || host == "xiaomimimo.com"
                || host.ends_with(".xiaomimimo.com"))
        {
            return None;
        }
        let response = sso
            .get(current.clone())
            .header("user-agent", "MiClaw/1.0")
            .send()
            .await
            .ok()?;
        for value in response.headers().get_all(reqwest::header::SET_COOKIE) {
            if let Ok(v) = value.to_str() {
                if let Some((key, val)) = v.split(';').next().unwrap_or("").split_once('=') {
                    if !val.is_empty() {
                        jar.insert(key.to_owned(), val.to_owned());
                    }
                }
            }
        }
        if response.status().is_redirection() {
            let next = response
                .headers()
                .get(reqwest::header::LOCATION)?
                .to_str()
                .ok()?;
            current = current.join(next).ok()?;
        } else {
            break;
        }
    }
    let token = jar
        .get("serviceToken")
        .or_else(|| jar.get(&format!("{sid}_serviceToken")))?;
    let mut cookie = format!("serviceToken={token}");
    if let Some(id) = psd["mimoUserId"].as_str().filter(|s| !s.is_empty()) {
        cookie.push_str(&format!("; userId={id}"));
    }
    for (k, v) in &jar {
        if k.ends_with("_ph") || k.ends_with("_slh") {
            cookie.push_str(&format!("; {k}={v}"));
        }
    }
    let response=client.get(format!("https://mimo-server-{region}.xiaomimimo.com/api/user/usage")).header("cookie",cookie).header("user-agent","miNative PC/Normal Windows_NT/10.0.19045 SDKV/1.0.0 DEVT/PC DEVS/Windows APP/miaccount_desktop APPV/0.1.0").header("accept","application/json").send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let data: Value = serde_json::from_str(&response.text().await.ok()?).ok()?;
    if data["code"] != 0 {
        return None;
    }
    let data = &data["data"];
    let percent = data["percent"].as_f64()?.round().clamp(0.0, 100.0);
    let reset = if data["resetAt"].as_i64().is_some_and(|n| n > 0) {
        parse_reset_time(&data["resetAt"])
    } else {
        parse_reset_time(&data["resetDate"])
    };
    Some(
        json!({"plan":"Xiaomi MiMo Desktop","quotas":{"Weekly":{"used":100.0-percent,"total":100,"remainingPercentage":percent,"resetAt":reset,"unlimited":false}}}),
    )
}

pub async fn get_usage_with_sso(
    client: &reqwest::Client,
    sso: &reqwest::Client,
    url: &str,
    token: &str,
    psd: &Value,
) -> Value {
    if let Some(result) = account_usage(client, sso, psd).await {
        return result;
    }
    get_usage(client, url, token, psd).await
}
pub async fn get_usage(client: &reqwest::Client, url: &str, token: &str, psd: &Value) -> Value {
    let key = if token.is_empty() {
        psd["apiKey"].as_str().unwrap_or("")
    } else {
        token
    };
    if key.trim().is_empty() {
        return json!({"message":"Xiaomi MiMo Desktop not connected. Add credentials to view usage."});
    }
    let r = match client
        .get(url)
        .bearer_auth(key.trim())
        .header("x-mimo-source", "mimocode-cli")
        .header("accept", "application/json")
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return json!({"message":format!("Xiaomi MiMo Desktop usage error: {e}")}),
    };
    let status = r.status();
    if status.as_u16() == 401 {
        return json!({"plan":"Xiaomi MiMo Desktop","message":"Weekly quota requires Xiaomi account session. API key alone is insufficient."});
    }
    if !status.is_success() {
        return json!({"plan":"Xiaomi MiMo Desktop","message":format!("Usage API error ({})",status.as_u16())});
    }
    let data: Value =
        serde_json::from_str(&r.text().await.unwrap_or_default()).unwrap_or(Value::Null);
    if data["code"] != 0 || !data["data"].is_object() {
        return json!({"plan":"Xiaomi MiMo Desktop","message":"Usage endpoint returned unexpected response."});
    }
    let Some(percent) = data["data"]["percent"].as_f64() else {
        return json!({"plan":"Xiaomi MiMo Desktop","message":"Usage data missing percent field."});
    };
    let remaining = percent.round().clamp(0.0, 100.0);
    json!({"plan":"Xiaomi MiMo Desktop","quotas":{"Weekly":{"used":100.0-remaining,"total":100,"remainingPercentage":remaining,"resetAt":parse_reset_time(&data["data"]["resetDate"]),"unlimited":false}}})
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
            let body = r#"{"code":0,"data":{"percent":94,"resetDate":"2026-09-16"}}"#;
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
            "key",
            &Value::Null,
        )
        .await;
        t.await.unwrap();
        assert_eq!(v["quotas"]["Weekly"]["used"], 6.0);
    }
}
