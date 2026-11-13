use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::digest;
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const CURSOR_MODELS_URL: &str = "https://agent.api5.cursor.sh/agent.v1.AgentService/GetUsableModels";
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
const CACHE_TTL: Duration = Duration::from_secs(5 * 60);

fn catalog_cache() -> &'static Mutex<HashMap<String, (Instant, Vec<Value>)>> {
    static CACHE: OnceLock<Mutex<HashMap<String, (Instant, Vec<Value>)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn random_uuid_v4() -> String {
    let mut b = [0u8; 16];
    let _ = SystemRandom::new().fill(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        u32::from_be_bytes([b[0], b[1], b[2], b[3]]),
        u16::from_be_bytes([b[4], b[5]]),
        u16::from_be_bytes([b[6], b[7]]),
        u16::from_be_bytes([b[8], b[9]]),
        u64::from_be_bytes([0, 0, b[10], b[11], b[12], b[13], b[14], b[15]]),
    )
}

pub fn generate_hashed_64_hex(input: &str, salt: &str) -> String {
    let mut ctx = digest::Context::new(&digest::SHA256);
    ctx.update(input.as_bytes());
    ctx.update(salt.as_bytes());
    hex_encode(ctx.finish().as_ref())
}

pub fn generate_session_id(auth_token: &str) -> String {
    // UUID v5 DNS namespace: 6ba7b810-9dad-11d1-80b4-00c04fd430c8
    const DNS_NS: [u8; 16] = [
        0x6b, 0xa7, 0xb8, 0x10, 0x9d, 0xad, 0x11, 0xd1, 0x80, 0xb4, 0x00, 0xc0, 0x4f, 0xd4, 0x30,
        0xc8,
    ];
    let mut ctx = digest::Context::new(&digest::SHA1_FOR_LEGACY_USE_ONLY);
    ctx.update(&DNS_NS);
    ctx.update(auth_token.as_bytes());
    let hash = ctx.finish();
    let mut b = [0u8; 16];
    b.copy_from_slice(&hash.as_ref()[..16]);
    b[6] = (b[6] & 0x0f) | 0x50;
    b[8] = (b[8] & 0x3f) | 0x80;
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        u32::from_be_bytes([b[0], b[1], b[2], b[3]]),
        u16::from_be_bytes([b[4], b[5]]),
        u16::from_be_bytes([b[6], b[7]]),
        u16::from_be_bytes([b[8], b[9]]),
        u64::from_be_bytes([0, 0, b[10], b[11], b[12], b[13], b[14], b[15]]),
    )
}

pub fn generate_cursor_checksum(machine_id: &str) -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let timestamp = millis / 1_000_000;
    let mut bytes = [
        ((timestamp >> 40) & 0xff) as u8,
        ((timestamp >> 32) & 0xff) as u8,
        ((timestamp >> 24) & 0xff) as u8,
        ((timestamp >> 16) & 0xff) as u8,
        ((timestamp >> 8) & 0xff) as u8,
        (timestamp & 0xff) as u8,
    ];
    let mut t: u8 = 165;
    for (i, b) in bytes.iter_mut().enumerate() {
        *b = (*b ^ t).wrapping_add((i % 256) as u8);
        t = *b;
    }
    format!("{}{machine_id}", URL_SAFE_NO_PAD.encode(bytes))
}

pub fn build_cursor_headers(
    access_token: &str,
    machine_id: Option<&str>,
    ghost_mode: bool,
) -> HashMap<&'static str, String> {
    let clean_token = access_token
        .split_once("::")
        .map(|(_, tok)| tok)
        .unwrap_or(access_token);
    let owned_machine_id;
    let effective_machine_id = match machine_id.filter(|s| !s.is_empty()) {
        Some(m) => m,
        None => {
            owned_machine_id = generate_hashed_64_hex(clean_token, "machineId");
            &owned_machine_id
        }
    };
    let os = if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    let arch = if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x64"
    };
    let mut headers = HashMap::new();
    headers.insert("authorization", format!("Bearer {clean_token}"));
    headers.insert("accept", "application/proto".to_owned());
    headers.insert("content-type", "application/proto".to_owned());
    headers.insert("user-agent", "connect-es/1.6.1".to_owned());
    headers.insert("x-amzn-trace-id", format!("Root={}", random_uuid_v4()));
    headers.insert("x-client-key", generate_hashed_64_hex(clean_token, ""));
    headers.insert(
        "x-cursor-checksum",
        generate_cursor_checksum(effective_machine_id),
    );
    headers.insert("x-cursor-client-version", "3.12.17".to_owned());
    headers.insert(
        "x-cursor-client-commit",
        "0fb762053c34788bb7760d5673f8a6d4c8589d50".to_owned(),
    );
    headers.insert("x-cursor-client-type", "ide".to_owned());
    headers.insert("x-cursor-client-os", os.to_owned());
    headers.insert("x-cursor-client-arch", arch.to_owned());
    headers.insert("x-cursor-client-device-type", "desktop".to_owned());
    headers.insert("x-cursor-config-version", random_uuid_v4());
    headers.insert("x-cursor-timezone", "UTC".to_owned());
    headers.insert(
        "x-ghost-mode",
        if ghost_mode { "true" } else { "false" }.to_owned(),
    );
    headers.insert("x-request-id", random_uuid_v4());
    headers.insert("x-session-id", generate_session_id(clean_token));
    headers
}

fn cache_key(access_token: &str, machine_id: &str) -> String {
    generate_hashed_64_hex(&format!("cursor:{machine_id}:{access_token}"), "")
}

pub async fn resolve_cursor_models(
    client: &reqwest::Client,
    credentials: &Value,
    force_refresh: bool,
) -> Option<Value> {
    resolve_cursor_models_with_url(client, CURSOR_MODELS_URL, credentials, force_refresh).await
}

pub async fn resolve_cursor_models_with_url(
    client: &reqwest::Client,
    url: &str,
    credentials: &Value,
    force_refresh: bool,
) -> Option<Value> {
    let access_token = credentials["accessToken"].as_str().filter(|s| !s.is_empty())?;
    let machine_id = credentials["providerSpecificData"]["machineId"]
        .as_str()
        .filter(|s| !s.is_empty())?;
    let key = cache_key(access_token, machine_id);

    if !force_refresh {
        let guard = catalog_cache().lock().unwrap_or_else(|e| e.into_inner());
        if let Some((expires_at, models)) = guard.get(&key)
            && *expires_at > Instant::now()
        {
            return Some(json!({ "models": models }));
        }
    }

    let ghost_mode = credentials["providerSpecificData"]["ghostMode"]
        .as_bool()
        .unwrap_or(true);
    let headers = build_cursor_headers(access_token, Some(machine_id), ghost_mode);
    let mut req = client.post(url).timeout(FETCH_TIMEOUT);
    for (k, v) in headers {
        req = req.header(k, v);
    }

    let resp = req.send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let bytes = resp.bytes().await.ok()?;
    let mut models = parse_cursor_usable_models(&bytes).ok()?;
    if models.is_empty() {
        return None;
    }
    let _ = merge_cursor_model_flags(&mut models, &bytes);
    catalog_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, (Instant::now() + CACHE_TTL, models.clone()));
    Some(json!({ "models": models }))
}

fn varint(bytes: &[u8], offset: &mut usize) -> Result<u64, &'static str> {
    let mut value = 0u64;
    for shift in (0..70).step_by(7) {
        let b = *bytes.get(*offset).ok_or("truncated varint")?;
        *offset += 1;
        if shift == 63 && b > 1 {
            return Err("invalid varint");
        }
        value |= ((b & 127) as u64) << shift;
        if b & 128 == 0 {
            return Ok(value);
        }
    }
    Err("invalid varint")
}
#[derive(Clone, Copy)]
enum Field<'a> {
    Number(u64),
    Bytes(&'a [u8]),
    Other,
}
fn fields(bytes: &[u8]) -> Result<Vec<(u64, Field<'_>)>, &'static str> {
    let mut result = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let tag = varint(bytes, &mut offset)?;
        if tag >> 3 == 0 {
            return Err("invalid field");
        }
        let field = match tag & 7 {
            0 => Field::Number(varint(bytes, &mut offset)?),
            2 => {
                let len =
                    usize::try_from(varint(bytes, &mut offset)?).map_err(|_| "length overflow")?;
                let end = offset
                    .checked_add(len)
                    .filter(|n| *n <= bytes.len())
                    .ok_or("truncated field")?;
                let value = &bytes[offset..end];
                offset = end;
                Field::Bytes(value)
            }
            1 | 5 => {
                let len = if tag & 7 == 1 { 8 } else { 4 };
                offset = offset
                    .checked_add(len)
                    .filter(|n| *n <= bytes.len())
                    .ok_or("truncated field")?;
                Field::Other
            }
            _ => return Err("unsupported wire type"),
        };
        result.push((tag >> 3, field));
    }
    Ok(result)
}
fn first_string(fields: &[(u64, Field<'_>)], id: u64) -> String {
    match fields.iter().find(|(n, _)| *n == id).map(|(_, f)| f) {
        Some(Field::Bytes(v)) => String::from_utf8_lossy(v).into_owned(),
        _ => String::new(),
    }
}
fn first_bool(fields: &[(u64, Field<'_>)], id: u64) -> bool {
    matches!(
        fields.iter().find(|(n, _)| *n == id).map(|(_, f)| f),
        Some(Field::Number(1))
    )
}

pub fn parse_cursor_usable_models(payload: &[u8]) -> Result<Vec<Value>, &'static str> {
    let mut seen = HashSet::new();
    let mut models = Vec::new();
    for (number, entry) in fields(payload)? {
        if number != 1 {
            continue;
        }
        let Field::Bytes(entry) = entry else { continue };
        let detail = fields(entry)?;
        let id = first_string(&detail, 1).trim().to_owned();
        if id.is_empty() || !seen.insert(id.clone()) {
            continue;
        }
        let name = [4, 5, 3]
            .into_iter()
            .map(|n| first_string(&detail, n))
            .find(|s| !s.is_empty())
            .unwrap_or_else(|| id.clone());
        models.push(json!({"id":id,"name":name.trim()}));
    }
    Ok(models)
}
pub fn parse_cursor_available_models(payload: &[u8]) -> Result<Vec<Value>, &'static str> {
    let mut seen = HashSet::new();
    let mut models = Vec::new();
    for (number, entry) in fields(payload)? {
        if number != 2 {
            continue;
        }
        let Field::Bytes(entry) = entry else { continue };
        let detail = fields(entry)?;
        let id = first_string(&detail, 1).trim().to_owned();
        if id.is_empty() || !seen.insert(id.clone()) {
            continue;
        }
        models.push(json!({"id":id,"defaultOn":first_bool(&detail,2),"isChatOnly":first_bool(&detail,4),"supportsAgent":first_bool(&detail,5),"isHidden":first_bool(&detail,35)}));
    }
    Ok(models)
}
pub fn merge_cursor_model_flags(models: &mut [Value], payload: &[u8]) -> Result<(), &'static str> {
    let available = parse_cursor_available_models(payload)?;
    for model in models {
        if let Some(flags) = available.iter().find(|f| f["id"] == model["id"]) {
            if let (Some(model), Some(flags)) = (model.as_object_mut(), flags.as_object()) {
                model.extend(flags.clone());
            }
        }
    }
    Ok(())
}
pub fn resolve_auto_model_selection(models: &[Value]) -> Option<Value> {
    let usable: Vec<_> = models
        .iter()
        .filter(|m| m["supportsAgent"] == true && m["isChatOnly"] != true && m["isHidden"] != true)
        .collect();
    let preferred = usable
        .iter()
        .find(|m| m["defaultOn"] == true)
        .or_else(|| usable.first())?;
    Some(json!({"modelId":preferred["id"],"matchedBy":"auto-default"}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_models_and_flags_with_bounds_checks() {
        let payload = [
            0x0a, 0x0a, 0x0a, 0x03, b'a', b'b', b'c', 0x22, 0x03, b'A', b'B', b'C', 0x12, 0x07,
            0x0a, 0x03, b'a', b'b', b'c', 0x28, 0x01,
        ];
        let mut models = parse_cursor_usable_models(&payload).unwrap();
        merge_cursor_model_flags(&mut models, &payload).unwrap();
        assert_eq!(models[0]["name"], "ABC");
        assert_eq!(
            resolve_auto_model_selection(&models).unwrap()["modelId"],
            "abc"
        );
        assert!(parse_cursor_usable_models(&[0x0a, 0x10, 0x01]).is_err());
    }

    #[tokio::test]
    async fn live_discovery_and_cache() {
        use axum::{Router, routing::post};
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        catalog_cache().lock().unwrap_or_else(|e| e.into_inner()).clear();
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_clone = hits.clone();
        let payload: &'static [u8] = &[
            0x0a, 0x0a, 0x0a, 0x03, b'a', b'b', b'c', 0x22, 0x03, b'A', b'B', b'C', 0x12, 0x07,
            0x0a, 0x03, b'a', b'b', b'c', 0x28, 0x01,
        ];
        let app = Router::new().route(
            "/models",
            post(move || {
                hits_clone.fetch_add(1, Ordering::SeqCst);
                async move { payload }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let client = reqwest::Client::new();
        let creds = json!({
            "accessToken": "user::tok-1",
            "providerSpecificData": { "machineId": "mid-1", "ghostMode": true }
        });
        let url = format!("http://{addr}/models");
        let res1 = resolve_cursor_models_with_url(&client, &url, &creds, false)
            .await
            .unwrap();
        assert_eq!(res1["models"][0]["id"], "abc");
        assert_eq!(res1["models"][0]["supportsAgent"], true);
        assert_eq!(hits.load(Ordering::SeqCst), 1);

        // Cached hit
        let res2 = resolve_cursor_models_with_url(&client, &url, &creds, false)
            .await
            .unwrap();
        assert_eq!(res2["models"][0]["id"], "abc");
        assert_eq!(hits.load(Ordering::SeqCst), 1);

        // Force refresh
        let _ = resolve_cursor_models_with_url(&client, &url, &creds, true)
            .await
            .unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }
}
