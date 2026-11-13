use crate::services::{
    minimax_code_usage::{account_signature, parse_membership},
    usage_shared::parse_reset_time,
};
use serde_json::{Value, json};

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    #[tokio::test]
    async fn sends_signed_account_request() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = [0; 4096];
            let n = stream.read(&mut bytes).await.unwrap();
            let request = String::from_utf8_lossy(&bytes[..n]).to_lowercase();
            assert!(request.contains("authorization: bearer secret"));
            assert!(request.contains("x-signature:"));
            let body = r#"{"data":{"userInfo":{"realUserID":"123"}}}"#;
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
        let result = account(
            &reqwest::Client::new(),
            &format!("http://{addr}"),
            "/v1/api/user/info",
            "secret",
            "",
            "en",
            None,
        )
        .await
        .unwrap();
        task.await.unwrap();
        assert_eq!(result["data"]["userInfo"]["realUserID"], "123");
    }
}
async fn account(
    client: &reqwest::Client,
    base: &str,
    path: &str,
    token: &str,
    user: &str,
    lang: &str,
    body: Option<&Value>,
) -> Result<Value, String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let mut url = reqwest::Url::parse(&format!("{base}{path}")).map_err(|e| e.to_string())?;
    url.query_pairs_mut()
        .append_pair("device_platform", "mcode")
        .append_pair("biz_id", "3")
        .append_pair("app_id", "3001")
        .append_pair("version_code", "22201")
        .append_pair("unix", &now.to_string())
        .append_pair("timezone_offset", "0")
        .append_pair("sys_language", lang)
        .append_pair("lang", lang)
        .append_pair("device_id", "0")
        .append_pair("os_name", std::env::consts::OS)
        .append_pair("browser_name", "mcode")
        .append_pair("user_id", if user.is_empty() { "0" } else { user })
        .append_pair("client", "mcode");
    let path_query = format!("{}?{}", url.path(), url.query().unwrap_or(""));
    let serialized = body.map(Value::to_string);
    let (yy, ts, sig) = account_signature(&path_query, serialized.as_deref(), now);
    let mut req = if body.is_some() {
        client.post(url)
    } else {
        client.get(url)
    };
    req = req
        .bearer_auth(token)
        .header("accept", "application/json")
        .header("content-type", "application/json")
        .header("user-agent", "MiniMaxCode")
        .header("yy", yy)
        .header("x-timestamp", ts)
        .header("x-signature", sig);
    if let Some(body) = serialized {
        req = req.body(body);
    }
    let r = req.send().await.map_err(|e| e.to_string())?;
    if r.status().as_u16() == 401 || r.status().as_u16() == 403 {
        return Err(format!("sign-in refused (HTTP {})", r.status().as_u16()));
    }
    if !r.status().is_success() {
        return Err(format!("HTTP {}", r.status().as_u16()));
    }
    let v: Value = serde_json::from_str(&r.text().await.map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let code = v["statusInfo"]["code"]
        .as_i64()
        .or_else(|| v["base_resp"]["status_code"].as_i64())
        .unwrap_or(0);
    if code != 0 {
        return Err(format!("account status {code}"));
    }
    Ok(v)
}
pub async fn get_usage(
    client: &reqwest::Client,
    agent: &str,
    platform: &str,
    conn: &Value,
) -> Value {
    let mut leased = String::new();
    #[cfg(unix)]
    if conn["accessToken"].as_str().unwrap_or("").is_empty()
        && crate::services::minimax_code_lease::is_lease_connection(conn)
    {
        let dir = conn["providerSpecificData"]["mcodeDataDir"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .map(str::to_owned)
            .or_else(|| {
                std::env::var("MINIMAX_DESKTOP_DATA_DIR")
                    .ok()
                    .filter(|s| !s.trim().is_empty())
            })
            .or_else(|| std::env::var("HOME").ok().map(|h| format!("{h}/.minimax")));
        if let Some(dir) = dir {
            if let Ok((_, cap_path)) =
                crate::services::minimax_code_lease::resolve_lease_endpoint(&dir, false)
            {
                if let Ok(cap) = std::fs::read_to_string(cap_path) {
                    let cap = cap.trim();
                    if cap.len() == 43
                        && cap
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                    {
                        let request = json!({"version":1,"requestId":super::request_id().unwrap_or_default(),"capability":cap,"method":"lease","minValidityMs":300000});
                        if let Ok(reply) = crate::services::minimax_code_lease::broker_call(
                            &dir,
                            &request,
                            std::time::Duration::from_secs(10),
                        )
                        .await
                        {
                            if reply["ok"] == true {
                                leased = reply["result"]["accessToken"]
                                    .as_str()
                                    .unwrap_or("")
                                    .to_owned();
                            }
                        }
                    }
                }
            }
        }
    }
    let token = conn["accessToken"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or(&leased);
    let provider = conn["provider"].as_str().unwrap_or("");
    if token.is_empty() {
        return json!({"message":format!("Usage API not implemented for {provider}")});
    }
    let lang = if provider == "minimax-code" {
        "zh"
    } else {
        "en"
    };
    let mut user = conn["providerSpecificData"]["realUserID"]
        .as_str()
        .unwrap_or("")
        .to_owned();
    let mut display = user.clone();
    if user.is_empty() {
        let identity = match account(client, agent, "/v1/api/user/info", token, "", lang, None)
            .await
        {
            Ok(v) => v,
            Err(e) => {
                return json!({"message":format!("MiniMax Code sign-in expired — unauthorized, please re-authorize ({e})")});
            }
        };
        let u = identity
            .pointer("/data/userInfo")
            .or_else(|| identity.pointer("/data/user_info"))
            .or_else(|| identity.get("userInfo"))
            .unwrap_or(&Value::Null);
        user = u["realUserID"]
            .as_str()
            .or_else(|| u["real_user_id"].as_str())
            .unwrap_or("")
            .to_owned();
        if user.is_empty() {
            return json!({"message":"account: no user id in the reply"});
        }
        display = u["userEmail"]
            .as_str()
            .or_else(|| u["email"].as_str())
            .or_else(|| u["name"].as_str())
            .unwrap_or(&user)
            .to_owned();
    }
    let workspace = account(
        client,
        agent,
        "/matrix/api/v1/user/get_user_extra_info",
        token,
        &user,
        lang,
        Some(&json!({})),
    )
    .await;
    let mut membership = json!({});
    if let Ok(v) = workspace {
        let list = v["workspaces"]
            .as_array()
            .or_else(|| v["data"]["workspaces"].as_array());
        if let Some(w) = list
            .into_iter()
            .flatten()
            .find(|w| w["workspace_type"] == 0)
        {
            membership = parse_membership(w);
            if let Ok(v) = account(
                client,
                agent,
                "/matrix/api/v1/commerce/get_membership_info",
                token,
                &user,
                lang,
                Some(&json!({"workspace_id":w["workspace_id"]})),
            )
            .await
            {
                let m = parse_membership(&v);
                if let (Some(dst), Some(src)) = (membership.as_object_mut(), m.as_object()) {
                    dst.extend(src.clone());
                }
            }
        }
    } else if let Ok(v) = account(
        client,
        agent,
        "/matrix/api/v1/commerce/get_membership_info",
        token,
        &user,
        lang,
        Some(&json!({})),
    )
    .await
    {
        membership = parse_membership(&v);
    }
    let mut quotas = serde_json::Map::new();
    if let Some(balance) = membership["balance"].as_str() {
        quotas.insert("Credits".into(),json!({"used":0,"total":balance.parse::<f64>().unwrap_or(0.0),"isCreditBalance":true,"currency":"credits"}));
    }
    if membership["hasTokenPlan"] == true {
        if let Some(group) = membership["opGroupId"].as_str() {
            let url = format!("{platform}/v1/api/openplatform/coding_plan/remains");
            match client
                .get(url)
                .bearer_auth(token)
                .header("x-group-id", group)
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => {
                    let body: Value = serde_json::from_str(&r.text().await.unwrap_or_default())
                        .unwrap_or(Value::Null);
                    for m in body["model_remains"].as_array().into_iter().flatten() {
                        let name = m["model_name"].as_str().unwrap_or("");
                        for (prefix, label) in [
                            ("current_interval", "5 hours"),
                            ("current_weekly", "7 days"),
                        ] {
                            let status = m[format!("{prefix}_status")].as_i64().unwrap_or(0);
                            let pct = m[format!("{prefix}_remaining_percent")].as_f64();
                            if status == 3 || pct.is_none() && status != 2 {
                                continue;
                            }
                            let left = if status == 2 {
                                0.0
                            } else {
                                pct.unwrap_or(0.0).clamp(0.0, 100.0)
                            };
                            let title = if name.eq_ignore_ascii_case("general") {
                                label.to_owned()
                            } else {
                                format!("{name} · {label}")
                            };
                            let end = if prefix == "current_interval" {
                                &m["end_time"]
                            } else {
                                &m["weekly_end_time"]
                            };
                            quotas.insert(title.clone(),json!({"name":title,"used":100.0-left,"total":100,"remaining":left,"remainingPercentage":left,"resetAt":parse_reset_time(end)}));
                        }
                    }
                }
                _ => {
                    quotas.insert("M Plan windows".into(),json!({"name":"M Plan windows","used":0,"total":0,"resetAt":null,"message":"Unable to fetch M Plan windows"}));
                }
            }
        }
    }
    if quotas.is_empty() {
        quotas.insert("Plan".into(),json!({"name":"Plan","used":0,"total":0,"resetAt":null,"message":if membership["hasTokenPlan"]==false{"Free account (no M Plan)"}else{"No usage data returned"}}));
    }
    let plan = if membership["hasTokenPlan"] == true {
        membership["tier"]
            .as_str()
            .map(|s| format!("M Plan {s}"))
            .unwrap_or("M Plan".into())
    } else {
        "Free".into()
    };
    let mut result = json!({"quotas":quotas,"plan":plan,"user":display});
    if let Some(date) = parse_reset_time(&membership["expiresAt"]) {
        result["planExpiresAt"] = json!(date);
    }
    result
}
