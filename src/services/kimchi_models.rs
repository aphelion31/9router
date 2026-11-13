use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

pub const KIMCHI_API: &str = "https://llm.kimchi.dev";
pub const KIMCHI_USER_AGENT: &str = "kimchi/0.1.40";

const FETCH_TIMEOUT: Duration = Duration::from_secs(20);
const CACHE_TTL: Duration = Duration::from_secs(5 * 60);

type Entry = (Instant, Vec<Value>);

fn cache() -> &'static Mutex<HashMap<String, Entry>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Entry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn build_kimchi_models_url(endpoint: Option<&str>) -> String {
    let endpoint = endpoint
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(KIMCHI_API);
    format!(
        "{}/v1/models/metadata?include_in_cli=true",
        endpoint.trim_end_matches('/')
    )
}

pub fn normalize_kimchi_model(item: &Value) -> Option<Value> {
    let mut model = item.as_object()?.clone();
    let id = ["slug", "id", "model", "name"]
        .iter()
        .find_map(|k| item[*k].as_str().filter(|s| !s.is_empty()))?
        .trim();
    if id.is_empty() {
        return None;
    }
    let name = ["display_name", "displayName", "name"]
        .iter()
        .find_map(|k| item[*k].as_str().filter(|s| !s.is_empty()))
        .unwrap_or(id)
        .trim();
    let provider = item["provider"].as_str().unwrap_or("");
    let modalities: Vec<&str> = item["input_modalities"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .filter(|v| *v == "text" || *v == "image")
                .collect()
        })
        .unwrap_or_default();
    let vision = modalities.contains(&"image");
    let kind = if vision { "imageToText" } else { "llm" };
    let reasoning = item["reasoning"].as_bool() == Some(true);
    let number = |paths: &[&Value]| {
        paths
            .iter()
            .find_map(|v| v.as_f64().or_else(|| v.as_str()?.parse().ok()).filter(|n| *n > 0.0))
            .map(|n| n as u64)
    };
    let context = number(&[
        &item["limits"]["context_window"],
        &item["contextLength"],
        &item["context_length"],
    ]);
    let max = number(&[
        &item["limits"]["max_output_tokens"],
        &item["maxOutputTokens"],
        &item["max_output_tokens"],
    ]);
    let mut capabilities = json!({"vision": vision, "reasoning": reasoning});
    if let Some(n) = context {
        capabilities["contextWindow"] = json!(n);
        model.insert("contextLength".into(), json!(n));
    }
    if let Some(n) = max {
        capabilities["maxOutput"] = json!(n);
        model.insert("maxOutputTokens".into(), json!(n));
    }
    if !provider.is_empty() {
        capabilities["upstreamProvider"] = json!(provider);
    }
    model.insert("id".into(), json!(id));
    model.insert("name".into(), json!(name));
    model.insert("provider".into(), json!(provider));
    model.insert("upstreamProvider".into(), json!(provider));
    model.insert("reasoning".into(), json!(reasoning));
    model.insert("inputModalities".into(), json!(modalities));
    model.insert("kind".into(), json!(kind));
    model.insert("type".into(), json!(kind));
    model.insert("capabilities".into(), capabilities);
    if provider == "anthropic" {
        model.insert(
            "compat".into(),
            json!({"supportsReasoningEffort":false,"cacheControlFormat":"anthropic"}),
        );
    }
    Some(Value::Object(model))
}

/// Fetches the Kimchi metadata catalog for `endpoint`, cached per endpoint and token for 5 minutes.
/// Returns `None` on transport failure, non-2xx, or an empty normalized list.
pub async fn resolve_kimchi_models(
    client: &reqwest::Client,
    endpoint: Option<&str>,
    token: &str,
) -> Option<Vec<Value>> {
    let url = build_kimchi_models_url(endpoint);
    let token = token.trim();
    if token.is_empty() {
        return None;
    }
    let key = format!("{url}\n{token}");
    if let Some((at, models)) = cache().lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        if at.elapsed() < CACHE_TTL {
            return Some(models.clone());
        }
    }

    let response = client
        .get(&url)
        .timeout(FETCH_TIMEOUT)
        .header("Accept", "application/json")
        .header("Authorization", format!("Bearer {token}"))
        .header("User-Agent", KIMCHI_USER_AGENT)
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let raw: Value = serde_json::from_str(&response.text().await.ok()?).ok()?;
    let models: Vec<Value> = raw["models"]
        .as_array()?
        .iter()
        .filter_map(normalize_kimchi_model)
        .collect();
    if models.is_empty() {
        return None;
    }
    cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, (Instant::now(), models.clone()));
    Some(models)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn normalizes_model_and_url() {
        assert_eq!(
            build_kimchi_models_url(Some(" https://example.com/// ")),
            "https://example.com/v1/models/metadata?include_in_cli=true"
        );
        let m = normalize_kimchi_model(&json!({"slug":" model ","provider":"anthropic","input_modalities":["text","image","audio"],"limits":{"context_window":100},"reasoning":true})).unwrap();
        assert_eq!(m["id"], "model");
        assert_eq!(m["kind"], "imageToText");
        assert_eq!(m["capabilities"]["vision"], true);
        assert_eq!(m["compat"]["cacheControlFormat"], "anthropic");
        assert_eq!(m["inputModalities"].as_array().unwrap().len(), 2);
        assert!(normalize_kimchi_model(&json!({"slug":" "})).is_none());
    }

    #[tokio::test]
    async fn fetches_with_auth_and_caches() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let body = r#"{"models":[{"slug":"m1","input_modalities":["text"]},{"slug":" "}]}"#;
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 8192];
            let n = sock.read(&mut buf).await.unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).into_owned();
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            sock.write_all(resp.as_bytes()).await.unwrap();
            req
        });
        let client = reqwest::Client::new();
        let models = resolve_kimchi_models(&client, Some(&base), "tok-kimchi-test")
            .await
            .unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0]["id"], "m1");
        let req = server.await.unwrap().to_ascii_lowercase();
        assert!(req.starts_with("get /v1/models/metadata?include_in_cli=true"));
        assert!(req.contains("authorization: bearer tok-kimchi-test"));
        assert!(req.contains("user-agent: kimchi/0.1.40"));
        // Server gone: only the cache can answer.
        let cached = resolve_kimchi_models(&client, Some(&base), "tok-kimchi-test").await;
        assert_eq!(cached.unwrap(), models);
    }
}
