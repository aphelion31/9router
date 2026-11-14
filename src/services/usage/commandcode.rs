use crate::services::usage_shared::{parse_reset_time, to_finite_number};
use serde_json::{Value, json};
fn number(v: &Value, k: &str) -> f64 {
    to_finite_number(&v[k], 0.0)
}
async fn get(
    client: &reqwest::Client,
    base: &str,
    path: &str,
    key: &str,
    org: Option<&str>,
) -> Result<(u16, Value), String> {
    let mut url = reqwest::Url::parse(&format!("{base}{path}")).map_err(|e| e.to_string())?;
    if path == "/alpha/whoami" {
        url.query_pairs_mut().append_pair("limits", "1");
    }
    if let Some(org) = org {
        url.query_pairs_mut().append_pair("orgId", org);
    }
    let r = client
        .get(url)
        .bearer_auth(key)
        .header("accept", "application/json")
        .header("content-type", "application/json")
        .header("user-agent", "cli")
        .header("x-command-code-version", "1.88.0")
        .header("x-cli-environment", "production")
        .header("x-cli-surface", "cli")
        .header("x-taste-learning", "false")
        .header("x-session-id", crate::server::uuid_like())
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = r.status().as_u16();
    let body = serde_json::from_str(&r.text().await.unwrap_or_default()).unwrap_or(json!({}));
    Ok((status, body))
}
fn window(v: &Value) -> Option<Value> {
    if !v.is_object() {
        return None;
    }
    let used = number(v, "used");
    let total = number(v, "cap");
    if total <= 0.0 && used <= 0.0 {
        return None;
    }
    Some(
        json!({"used":used,"total":total,"percentage":if total>0.0{(used/total*100.0).round()}else{0.0},"colorPercentage":if total>0.0{((total-used)/total*100.0).round()}else{100.0},"percentageIsUsed":true,"remaining":(total-used).max(0.0),"unlimited":false,"resetAt":parse_reset_time(&v["resetAt"]),"emptyMessage":if used==0.0&&v["resetAt"].is_null(){Some("No usage in this window")}else{None}}),
    )
}
pub async fn get_usage(client: &reqwest::Client, base: &str, key: &str) -> Value {
    let key = key.trim();
    if key.is_empty() {
        return json!({"message":"Command Code API key not available. Add a key to view usage."});
    }
    let result=async{let(s,who)=get(client,base,"/alpha/whoami",key,None).await?;if s==401||s==403{return Ok(json!({"plan":"Command Code","message":"Command Code authentication failed. Check the API key."}));}
        if !(200..300).contains(&s){return Ok(json!({"plan":"Command Code","message":format!("Command Code usage API error ({s})")}));}
        let org=who["org"]["id"].as_str();let (credit,subs)=tokio::join!(get(client,base,"/alpha/billing/credits",key,org),get(client,base,"/alpha/billing/subscriptions",key,org));let(cs,credits)=credit?;let(ss,subs)=subs?;if cs==401||cs==403||ss==401||ss==403{return Ok(json!({"plan":"Command Code","message":"Command Code authentication failed. Check the API key."}));}
        if !(200..300).contains(&cs){return Ok(json!({"plan":"Command Code","message":format!("Command Code credits API error ({cs})")}));}
        if !(200..300).contains(&ss){return Ok(json!({"plan":"Command Code","message":format!("Command Code subscriptions API error ({ss})")}));}
        let raw=subs["data"]["planId"].as_str().unwrap_or("");let id=raw.trim().to_lowercase();let (plan,cap)=match id.as_str(){"go"|"individual-go"=>("Go",10.0),"go-v1"|"individual-go-v1"=>("Go",10.0),"goat"|"individual-goat"=>("GOAT",70.0),"pro"|"individual-pro"=>("Pro",30.0),"individual-pro-v1"=>("Pro",80.0),"provider"|"individual-provider"=>("Provider",15.0),"max"|"individual-max"=>("Max",150.0),"ultra"|"individual-ultra"=>("Ultra",300.0),"teams-pro"=>("Teams Pro",40.0),_=> (if raw.is_empty(){"Command Code"}else{raw},0.0)};let c=&credits["credits"];let remaining=number(c,"monthlyCredits")+number(c,"purchasedCredits")+number(c,"freeCredits");let used=if cap>0.0{(cap-remaining).max(0.0)}else{0.0};let mut quotas=serde_json::Map::new();quotas.insert("Credits".into(),json!({"name":"Monthly","used":used,"total":if cap>0.0{cap}else{remaining},"percentage":if cap>0.0{(used/cap*100.0).round()}else{0.0},"colorPercentage":if cap>0.0{(remaining/cap*100.0).round()}else{100.0},"percentageIsUsed":true,"remaining":remaining,"unlimited":cap<=0.0,"resetAt":parse_reset_time(&subs["data"]["currentPeriodEnd"])}));for (k,label,name) in [("fiveHour","Session (5h)","5-hour limit"),("weekly","Weekly","Weekly limit")]{if let Some(mut q)=window(&credits["windowLimits"][k]){q["name"]=json!(name);quotas.insert(label.into(),q);}}Ok(json!({"plan":plan,"quotas":quotas}))}.await;
    result.unwrap_or_else(|e: String| json!({"message":format!("Command Code error: {e}")}))
}
#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    #[tokio::test]
    async fn rejects_invalid_key() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let a = l.local_addr().unwrap();
        let t = tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut b = [0; 2048];
            let n = s.read(&mut b).await.unwrap();
            assert!(String::from_utf8_lossy(&b[..n]).contains("limits=1"));
            s.write_all(
                b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        });
        let v = get_usage(&reqwest::Client::new(), &format!("http://{a}"), "key").await;
        t.await.unwrap();
        assert_eq!(
            v["message"],
            "Command Code authentication failed. Check the API key."
        );
    }
}
