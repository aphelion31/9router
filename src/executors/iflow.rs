use anyhow::Context;
use ring::{
    hmac,
    rand::{SecureRandom, SystemRandom},
};
use serde_json::{Value, json};
use std::time::{SystemTime, UNIX_EPOCH};

pub(super) fn prepare(
    body: &mut Value,
    stream: bool,
    api_key: Option<&str>,
    token: Option<&str>,
) -> anyhow::Result<Vec<(&'static str, String)>> {
    if stream && body.get("messages").is_some() && body.get("stream_options").is_none() {
        body.as_object_mut()
            .context("iFlow request body must be an object")?
            .insert("stream_options".into(), json!({"include_usage": true}));
    }
    let mut bytes = [0u8; 16];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("cannot generate iFlow session ID"))?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let session = format!(
        "session-{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    );
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_millis()
        .to_string();
    let key = api_key.or(token).unwrap_or("");
    let signature = if key.is_empty() {
        String::new()
    } else {
        let key = hmac::Key::new(hmac::HMAC_SHA256, key.as_bytes());
        hmac::sign(&key, format!("iFlow-Cli:{session}:{timestamp}").as_bytes())
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    };
    Ok(vec![
        ("user-agent", "iFlow-Cli".into()),
        ("session-id", session),
        ("x-iflow-timestamp", timestamp),
        ("x-iflow-signature", signature),
    ])
}
