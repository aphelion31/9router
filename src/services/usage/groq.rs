use crate::services::usage_shared::unix_millis_iso;
use serde_json::{Value, json};

fn duration_ms(s: &str) -> Option<f64> {
    let mut total = 0.0;
    let mut found = false;
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
            i += 1;
        }
        let amount = s[start..i].parse::<f64>().ok()?;
        let rest = &s[i..];
        let (unit, len) = if rest.starts_with("ms") {
            (1.0, 2)
        } else if rest.starts_with('s') {
            (1000.0, 1)
        } else if rest.starts_with('m') {
            (60000.0, 1)
        } else if rest.starts_with('h') {
            (3600000.0, 1)
        } else {
            continue;
        };
        i += len;
        total += amount * unit;
        found = true;
    }
    found.then_some(total)
}

fn quota(headers: &reqwest::header::HeaderMap, kind: &str) -> Option<Value> {
    let number = |key: String| {
        headers
            .get(key)?
            .to_str()
            .ok()?
            .parse::<f64>()
            .ok()
            .filter(|n| n.is_finite())
    };
    let limit = number(format!("x-ratelimit-limit-{kind}"))?;
    let remaining = number(format!("x-ratelimit-remaining-{kind}"))?;
    let reset = headers
        .get(format!("x-ratelimit-reset-{kind}"))
        .and_then(|s| s.to_str().ok())
        .and_then(duration_ms)
        .and_then(|ms| {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_millis() as i64;
            unix_millis_iso(now.checked_add(ms as i64)?)
        });
    Some(json!({"used":(limit-remaining).max(0.0),"total":limit,"resetAt":reset,"unlimited":false}))
}

pub async fn get_usage(client: &reqwest::Client, url: &str, api_key: Option<&str>) -> Value {
    let Some(key) = api_key.map(str::trim).filter(|s| !s.is_empty()) else {
        return json!({"message":"Groq API key not available. Add a key to view usage."});
    };
    let response = match client
        .get(url)
        .bearer_auth(key)
        .header("accept", "application/json")
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => return json!({"message":format!("Groq error: {error}")}),
    };
    let status = response.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return json!({"plan":"Groq","message":"Groq authentication failed. Check the API key."});
    }
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        let detail: String = body.chars().take(120).collect();
        return json!({"plan":"Groq","message":format!("Groq usage API error ({}){}",status.as_u16(),if detail.is_empty() {String::new()} else {format!(": {detail}")})});
    }
    let mut quotas = serde_json::Map::new();
    if let Some(q) = quota(response.headers(), "requests") {
        quotas.insert("Requests".into(), q);
    }
    if let Some(q) = quota(response.headers(), "tokens") {
        quotas.insert("Tokens".into(), q);
    }
    let _ = response.text().await;
    if quotas.is_empty() {
        return json!({"plan":"Groq","message":"Groq connected. No rate-limit data reported for this key yet.","quotas":{}});
    }
    json!({"plan":"Groq","quotas":quotas})
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    #[tokio::test]
    async fn reads_limits_from_headers() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 2048];
            let n = stream.read(&mut request).await.unwrap();
            assert!(
                String::from_utf8_lossy(&request[..n])
                    .to_lowercase()
                    .contains("authorization: bearer key")
            );
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nx-ratelimit-limit-requests: 100\r\nx-ratelimit-remaining-requests: 70\r\nx-ratelimit-reset-requests: 2m59.56s\r\nConnection: close\r\n\r\n").await.unwrap();
        });
        let result = get_usage(
            &reqwest::Client::new(),
            &format!("http://{addr}"),
            Some("key"),
        )
        .await;
        task.await.unwrap();
        assert_eq!(result["quotas"]["Requests"]["used"], 30.0);
        assert!(result["quotas"]["Requests"]["resetAt"].is_string());
        assert_eq!(duration_ms("2m59.56s"), Some(179560.0));
    }
}
