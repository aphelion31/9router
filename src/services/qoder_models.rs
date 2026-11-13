use crate::executors::qoder::build_cosy_headers;
use ring::digest;
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const FETCH_TIMEOUT_SECS: u64 = 15;
const CACHE_TTL_MS: u128 = 60 * 60 * 1000;
const PAT_REFRESH_BUFFER_MS: u128 = 5 * 60 * 1000;
const PAT_DEFAULT_TTL_MS: u128 = 24 * 60 * 60 * 1000;
const QODER_IDE_VERSION: &str = "1.0.0";
const QODER_CLIENT_TYPE: &str = "5";

#[derive(Clone, Debug)]
struct PatJobEntry {
    access_token: String,
    user_id: String,
    expires_at_ms: u128,
}

#[derive(Clone, Debug)]
pub struct QoderCatalog {
    pub expires_at_ms: u128,
    pub models: Vec<Value>,
    pub raw_configs: HashMap<String, Value>,
    pub fetched: bool,
}

static PAT_JOB_CACHE: LazyLock<Mutex<HashMap<String, PatJobEntry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static CATALOG_CACHE: LazyLock<Mutex<HashMap<String, QoderCatalog>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

pub fn is_qoder_pat(token: &str) -> bool {
    token.starts_with("pt-")
}

pub fn qoder_region_of(provider_id: Option<&str>) -> &'static str {
    if provider_id == Some("qoder-cn") {
        "cn"
    } else {
        "intl"
    }
}

fn qoder_openapi_base(region: &str) -> &'static str {
    if region == "cn" {
        "https://openapi.qoder.com.cn"
    } else {
        "https://openapi.qoder.sh"
    }
}

pub fn qoder_inference_base(credentials: &Value, region: &str) -> &'static str {
    if region == "cn" {
        return "https://gateway.qoder.com.cn";
    }
    let raw = credentials
        .get("apiKey")
        .and_then(|v| v.as_str())
        .or_else(|| credentials.get("accessToken").and_then(|v| v.as_str()))
        .unwrap_or("");
    let access = credentials
        .get("accessToken")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !raw.starts_with("pt-") && (raw.starts_with("jt-") || access.starts_with("jt-")) {
        "https://api2.qoder.sh"
    } else {
        "https://api3.qoder.sh"
    }
}

fn cache_key(credentials: &Value) -> String {
    let psd = credentials
        .get("providerSpecificData")
        .and_then(|v| v.as_object());
    let seed = psd
        .and_then(|m| m.get("userId").and_then(|v| v.as_str()))
        .filter(|s| !s.is_empty())
        .or_else(|| {
            credentials
                .get("refreshToken")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
        })
        .or_else(|| {
            credentials
                .get("accessToken")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or("anonymous");
    let region = qoder_region_of(credentials.get("provider").and_then(|v| v.as_str()));
    let mut ctx = digest::Context::new(&digest::SHA256);
    ctx.update(format!("qoder:{region}:{seed}").as_bytes());
    ctx.finish()
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

async fn exchange_job_token_with_base(
    pat: &str,
    openapi_base: &str,
) -> Result<(String, u128), String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(FETCH_TIMEOUT_SECS))
        .build()
        .map_err(|e| e.to_string())?;
    let url = format!("{}/api/v1/jobToken/exchange", openapi_base.trim_end_matches('/'));
    let res = client
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .header("User-Agent", "qodercli/1.0.0")
        .header("Cosy-Version", QODER_IDE_VERSION)
        .header("Cosy-ClientType", QODER_CLIENT_TYPE)
        .body(json!({ "personal_token": pat }).to_string())
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        let status = res.status().as_u16();
        let text = res.text().await.unwrap_or_default();
        return Err(format!(
            "qoder PAT exchange failed: {status} {}",
            &text[..text.len().min(200)]
        ));
    }
    let bytes = res.bytes().await.map_err(|e| e.to_string())?;
    let data: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let token = data
        .get("token")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "qoder PAT exchange returned no job token".to_string())?
        .to_string();
    let mut expires_at = now_ms() + PAT_DEFAULT_TTL_MS;
    if let Some(secs) = data.get("expires_in").and_then(|v| v.as_u64()) {
        if secs > 0 {
            expires_at = now_ms() + (secs as u128);
        }
    }
    Ok((token, expires_at))
}

async fn fetch_user_id_with_base(job_token: &str, openapi_base: &str) -> String {
    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_secs(FETCH_TIMEOUT_SECS))
        .build()
    else {
        return String::new();
    };
    let url = format!("{}/api/v1/userinfo", openapi_base.trim_end_matches('/'));
    let Ok(res) = client
        .get(&url)
        .header("Authorization", format!("Bearer {job_token}"))
        .header("Accept", "application/json")
        .header("User-Agent", "qodercli/1.0.0")
        .send()
        .await
    else {
        return String::new();
    };
    if !res.status().is_success() {
        return String::new();
    }
    let Ok(bytes) = res.bytes().await else {
        return String::new();
    };
    let Ok(data) = serde_json::from_slice::<Value>(&bytes) else {
        return String::new();
    };
    data.get("id")
        .or_else(|| data.get("userId"))
        .or_else(|| data.get("user_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

pub async fn resolve_qoder_credentials(
    credentials: &Value,
    region_override: Option<&str>,
) -> Result<Value, String> {
    let region = region_override
        .unwrap_or_else(|| qoder_region_of(credentials.get("provider").and_then(|v| v.as_str())));
    resolve_qoder_credentials_with_base(credentials, region, qoder_openapi_base(region)).await
}

pub async fn resolve_qoder_credentials_with_base(
    credentials: &Value,
    region: &str,
    openapi_base: &str,
) -> Result<Value, String> {
    let raw = credentials
        .get("apiKey")
        .and_then(|v| v.as_str())
        .or_else(|| credentials.get("accessToken").and_then(|v| v.as_str()))
        .unwrap_or("");
    if !is_qoder_pat(raw) {
        return Ok(credentials.clone());
    }
    let key = format!("{region}:{raw}");
    let cached = {
        let guard = PAT_JOB_CACHE.lock().unwrap();
        guard
            .get(&key)
            .filter(|e| e.expires_at_ms > now_ms() + PAT_REFRESH_BUFFER_MS)
            .cloned()
    };
    let resolved = if let Some(entry) = cached {
        entry
    } else {
        let (job_token, expires_at_ms) = exchange_job_token_with_base(raw, openapi_base).await?;
        let user_id = fetch_user_id_with_base(&job_token, openapi_base).await;
        let entry = PatJobEntry {
            access_token: job_token,
            user_id,
            expires_at_ms,
        };
        PAT_JOB_CACHE.lock().unwrap().insert(key, entry.clone());
        entry
    };

    let mut out = credentials.as_object().cloned().unwrap_or_default();
    out.insert("accessToken".into(), Value::String(resolved.access_token));
    out.remove("apiKey");
    let mut psd = credentials
        .get("providerSpecificData")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    psd.entry("authMethod".to_string())
        .or_insert_with(|| Value::String("pat".into()));
    let existing_uid = psd
        .get("userId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    psd.insert(
        "userId".into(),
        Value::String(if !resolved.user_id.is_empty() {
            resolved.user_id
        } else {
            existing_uid
        }),
    );
    psd.entry("machineId".to_string())
        .or_insert_with(|| Value::String(String::new()));
    out.insert("providerSpecificData".into(), Value::Object(psd));
    Ok(Value::Object(out))
}

async fn fetch_qoder_catalog_raw(
    credentials: &Value,
    inference_base: &str,
) -> Option<(Vec<Value>, HashMap<String, Value>)> {
    let psd = credentials
        .get("providerSpecificData")
        .and_then(|v| v.as_object())?;
    let user_id = psd.get("userId").and_then(|v| v.as_str()).unwrap_or("");
    let auth_token = credentials
        .get("accessToken")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if user_id.is_empty() || auth_token.is_empty() {
        return None;
    }
    let name = credentials
        .get("displayName")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let email = credentials
        .get("email")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let machine_id = psd.get("machineId").and_then(|v| v.as_str()).unwrap_or("");

    let url = format!("{}/algo/api/v2/model/list", inference_base.trim_end_matches('/'));
    let cosy_headers = build_cosy_headers(&[], &url, user_id, auth_token, name, email, machine_id);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(FETCH_TIMEOUT_SECS))
        .build()
        .ok()?;
    let mut req = client
        .get(&url)
        .header("Accept", "application/json")
        .header("Accept-Encoding", "identity");
    for (k, v) in cosy_headers {
        req = req.header(&k, &v);
    }
    let res = req.send().await.ok()?;
    if !res.status().is_success() {
        return None;
    }
    let bytes = res.bytes().await.ok()?;
    let body: Value = serde_json::from_slice(&bytes).ok()?;
    let chat = body.get("chat")?.as_array()?;

    let mut models = Vec::new();
    let mut raw_configs = HashMap::new();
    for entry in chat {
        let Some(obj) = entry.as_object() else {
            continue;
        };
        let Some(key) = obj.get("key").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) else {
            continue;
        };
        raw_configs.insert(key.to_string(), entry.clone());
        if obj.get("enable").and_then(|v| v.as_bool()) == Some(false) {
            continue;
        }
        let display = obj
            .get("display_name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(key);
        let ctx = obj
            .get("max_input_tokens")
            .and_then(|v| v.as_u64())
            .filter(|&n| n > 0)
            .unwrap_or(131_072);
        models.push(json!({
            "id": key,
            "name": display,
            "contextLength": ctx,
            "isVL": obj.get("is_vl").and_then(|v| v.as_bool()).unwrap_or(false),
            "isReasoning": obj.get("is_reasoning").and_then(|v| v.as_bool()).unwrap_or(false),
            "maxOutputTokens": obj.get("max_output_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
            "description": obj.get("description").and_then(|v| v.as_str()).unwrap_or(""),
        }));
    }
    Some((models, raw_configs))
}

pub async fn resolve_qoder_models(
    credentials: &Value,
    force_refresh: bool,
    region_override: Option<&str>,
) -> Option<QoderCatalog> {
    let region = region_override
        .unwrap_or_else(|| qoder_region_of(credentials.get("provider").and_then(|v| v.as_str())));
    let mut resolved = resolve_qoder_credentials(credentials, Some(region)).await.ok()?;
    if resolved
        .get("provider")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .is_empty()
    {
        if let Some(obj) = resolved.as_object_mut() {
            obj.insert(
                "provider".into(),
                Value::String(if region == "cn" { "qoder-cn" } else { "qoder" }.into()),
            );
        }
    }
    let base = qoder_inference_base(&resolved, region);
    resolve_qoder_models_with_base(&resolved, force_refresh, base).await
}

pub async fn resolve_qoder_models_with_base(
    resolved_credentials: &Value,
    force_refresh: bool,
    inference_base: &str,
) -> Option<QoderCatalog> {
    let has_token = resolved_credentials
        .get("accessToken")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty());
    let has_uid = resolved_credentials
        .get("providerSpecificData")
        .and_then(|v| v.get("userId"))
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty());
    if !has_token || !has_uid {
        return None;
    }

    let key = cache_key(resolved_credentials);
    let now = now_ms();
    if !force_refresh {
        if let Some(cached) = CATALOG_CACHE.lock().unwrap().get(&key).cloned() {
            if cached.expires_at_ms > now {
                return Some(cached);
            }
        }
    }

    let (models, raw_configs) = fetch_qoder_catalog_raw(resolved_credentials, inference_base).await?;
    let entry = QoderCatalog {
        expires_at_ms: now_ms() + CACHE_TTL_MS,
        models,
        raw_configs,
        fetched: true,
    };
    CATALOG_CACHE.lock().unwrap().insert(key, entry.clone());
    Some(entry)
}

pub async fn get_qoder_model_config(
    credentials: &Value,
    model_key: &str,
    force_refresh: bool,
) -> Option<Value> {
    let catalog = resolve_qoder_models(credentials, force_refresh, None).await?;
    let config = catalog.raw_configs.get(model_key)?;
    let mut out: Map<String, Value> = config.as_object().cloned().unwrap_or_default();
    out.insert("key".into(), Value::String(model_key.to_string()));
    Some(Value::Object(out))
}

pub fn invalidate_qoder_catalog(credentials: &Value) {
    CATALOG_CACHE.lock().unwrap().remove(&cache_key(credentials));
}

pub fn clear_qoder_catalog() {
    CATALOG_CACHE.lock().unwrap().clear();
    PAT_JOB_CACHE.lock().unwrap().clear();
}

pub fn routable_qoder_models_from_catalog(catalog: &QoderCatalog) -> Vec<Value> {
    let mut pairs: Vec<(String, Value)> = catalog
        .raw_configs
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    routable_qoder_models(&catalog.models, &pairs)
}

pub fn routable_qoder_models(models: &[Value], raw_configs: &[(String, Value)]) -> Vec<Value> {
    let mut seen = HashSet::new();
    let keys: HashSet<&str> = models
        .iter()
        .filter_map(|m| m["id"].as_str())
        .chain(raw_configs.iter().map(|(k, _)| k.as_str()))
        .collect();
    let mut out = Vec::new();
    for (id, name) in [("smodel", "Sonus"), ("cmodel", "Cantus")] {
        if !keys.contains(id) {
            seen.insert(id.to_owned());
            out.push(json!({"id":id,"name":name,"hidden":false}));
        }
    }
    for model in models {
        if let Some(id) = model["id"].as_str().filter(|s| !s.is_empty()) {
            if seen.insert(id.to_owned()) {
                out.push(json!({"id":id,"name":model["name"].as_str().filter(|s| !s.is_empty()).unwrap_or(id),"hidden":false}));
            }
        }
    }
    for (id, cfg) in raw_configs {
        if !id.is_empty() && seen.insert(id.clone()) {
            out.push(json!({"id":id,"name":cfg["display_name"].as_str().filter(|s| !s.is_empty()).unwrap_or(id),"hidden":true}));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_static_and_hidden_models() {
        let out = routable_qoder_models(
            &[json!({"id":"smodel","name":"Live"})],
            &[("hidden".into(), json!({"display_name":"Hidden"}))],
        );
        assert_eq!(
            out,
            vec![
                json!({"id":"cmodel","name":"Cantus","hidden":false}),
                json!({"id":"smodel","name":"Live","hidden":false}),
                json!({"id":"hidden","name":"Hidden","hidden":true})
            ]
        );
        assert!(is_qoder_pat("pt-token"));
        assert!(!is_qoder_pat("jt-token"));
    }

    #[tokio::test]
    async fn pat_exchange_and_live_catalog_cache() {
        use axum::{Router, routing::{get, post}};
        clear_qoder_catalog();
        let app = Router::new()
            .route(
                "/api/v1/jobToken/exchange",
                post(|| async { axum::Json(json!({ "token": "jt-exchanged", "expires_in": 3600000 })) }),
            )
            .route(
                "/api/v1/userinfo",
                get(|| async { axum::Json(json!({ "id": "user-42" })) }),
            )
            .route(
                "/algo/api/v2/model/list",
                get(|| async {
                    axum::Json(json!({
                        "chat": [
                            { "key": "qmodel", "display_name": "QModel", "max_input_tokens": 200000, "enable": true },
                            { "key": "hidden-model", "display_name": "Hidden", "enable": false }
                        ]
                    }))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let base = format!("http://{addr}");

        let creds = json!({ "apiKey": "pt-secret", "provider": "qoder" });
        let resolved = resolve_qoder_credentials_with_base(&creds, "intl", &base)
            .await
            .unwrap();
        assert_eq!(resolved["accessToken"], "jt-exchanged");
        assert_eq!(resolved["providerSpecificData"]["userId"], "user-42");
        assert_eq!(qoder_inference_base(&resolved, "intl"), "https://api2.qoder.sh");

        let catalog = resolve_qoder_models_with_base(&resolved, false, &base)
            .await
            .unwrap();
        assert_eq!(catalog.models.len(), 1);
        assert_eq!(catalog.models[0]["id"], "qmodel");
        assert!(catalog.raw_configs.contains_key("hidden-model"));

        let routable = routable_qoder_models_from_catalog(&catalog);
        assert!(routable.iter().any(|m| m["id"] == "smodel" && m["hidden"] == false));
        assert!(routable.iter().any(|m| m["id"] == "hidden-model" && m["hidden"] == true));
    }
}
