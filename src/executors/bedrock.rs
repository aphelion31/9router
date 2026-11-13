use anyhow::{Context, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use ring::{digest, hmac};
use serde_json::Value;

fn load_aws_profile_credentials_from_str(
    ini: &str,
    profile: &str,
) -> Option<(String, String, Option<String>)> {
    let target = format!("[{profile}]");
    let mut in_section = false;
    let mut key = None;
    let mut secret = None;
    let mut session = None;
    for raw in ini.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            in_section = line == target;
            continue;
        }
        if !in_section {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let k = k.trim();
            let v = v.trim().to_string();
            match k {
                "aws_access_key_id" => key = Some(v),
                "aws_secret_access_key" => secret = Some(v),
                "aws_session_token" => session = Some(v),
                _ => {}
            }
        }
    }
    Some((key?, secret?, session))
}

fn load_aws_profile_credentials(profile: &str) -> Option<(String, String, Option<String>)> {
    let path = std::env::var("AWS_SHARED_CREDENTIALS_FILE")
        .ok()
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|h| std::path::PathBuf::from(h).join(".aws/credentials"))
        })?;
    let text = std::fs::read_to_string(path).ok()?;
    load_aws_profile_credentials_from_str(&text, profile)
}

pub(crate) fn parse_bedrock_error(status: u16, body: &str) -> (u16, String) {
    let parsed: Option<Value> = serde_json::from_str(body).ok();
    let msg = parsed
        .as_ref()
        .and_then(|v| {
            v.get("message")
                .or_else(|| v.get("Message"))
                .and_then(Value::as_str)
        })
        .unwrap_or(body);
    let mapped_status = if msg.contains("ThrottlingException") || status == 429 {
        429
    } else if msg.contains("AccessDenied") || msg.contains("UnrecognizedClient") {
        401
    } else if msg.contains("ValidationException") {
        400
    } else {
        status
    };
    (mapped_status, msg.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fragmented_frame_and_crc() {
        let mut headers = Vec::new();
        for (name, value) in [(":message-type", "event"), (":event-type", "chunk")] {
            headers.push(name.len() as u8);
            headers.extend_from_slice(name.as_bytes());
            headers.push(7);
            headers.extend_from_slice(&(value.len() as u16).to_be_bytes());
            headers.extend_from_slice(value.as_bytes());
        }
        let payload = serde_json::json!({"bytes": STANDARD.encode(br#"{"type":"message_stop"}"#)})
            .to_string();
        let length = 16 + headers.len() + payload.len();
        let mut frame = Vec::new();
        frame.extend_from_slice(&(length as u32).to_be_bytes());
        frame.extend_from_slice(&(headers.len() as u32).to_be_bytes());
        frame.extend_from_slice(&crc32(&frame).to_be_bytes());
        frame.extend_from_slice(&headers);
        frame.extend_from_slice(payload.as_bytes());
        frame.extend_from_slice(&crc32(&frame).to_be_bytes());
        let mut decoder = Decoder::new(false);
        assert!(decoder.push(&frame[..10]).unwrap().is_empty());
        assert!(
            String::from_utf8(decoder.push(&frame[10..]).unwrap())
                .unwrap()
                .contains("event: message_stop")
        );
        decoder.finish().unwrap();
        frame[8] ^= 1;
        assert!(Decoder::new(false).push(&frame).is_err());
    }

    #[test]
    fn ini_profile_and_error_parsing_work() {
        let ini = "[default]\naws_access_key_id = AKIA1\naws_secret_access_key = SEC1\n[dev]\naws_access_key_id = AKIA2\naws_secret_access_key = SEC2\naws_session_token = TOK2\n";
        let (k, s, t) = load_aws_profile_credentials_from_str(ini, "dev").unwrap();
        assert_eq!(k, "AKIA2");
        assert_eq!(s, "SEC2");
        assert_eq!(t.as_deref(), Some("TOK2"));
        assert_eq!(
            parse_bedrock_error(400, r#"{"message":"ThrottlingException: rate"}"#).0,
            429
        );
    }
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb88320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}

pub(crate) struct Decoder {
    buffer: Vec<u8>,
    terminal: bool,
    xai: bool,
}
impl Decoder {
    pub(crate) fn new(xai: bool) -> Self {
        Self {
            buffer: Vec::new(),
            terminal: false,
            xai,
        }
    }
    pub(crate) fn push(&mut self, bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
        self.buffer.extend_from_slice(bytes);
        let mut out = Vec::new();
        while self.buffer.len() >= 12 {
            let read = |slice: &[u8]| u32::from_be_bytes(slice.try_into().unwrap()) as usize;
            let len = read(&self.buffer[0..4]);
            let headers = read(&self.buffer[4..8]);
            if crc32(&self.buffer[..8]) != read(&self.buffer[8..12]) as u32 {
                bail!("Bedrock EventStream prelude CRC mismatch");
            }
            if !(16..=16 * 1024 * 1024).contains(&len) || headers > 128 * 1024 || headers > len - 16
            {
                bail!("invalid Bedrock EventStream frame bounds");
            }
            if self.buffer.len() < len {
                break;
            }
            let frame = self.buffer.drain(..len).collect::<Vec<_>>();
            if crc32(&frame[..len - 4]) != read(&frame[len - 4..]) as u32 {
                bail!("Bedrock EventStream message CRC mismatch");
            }
            let mut i = 12;
            let end = 12 + headers;
            let mut event_type = String::new();
            let mut message_type = String::new();
            while i < end {
                let n = frame[i] as usize;
                i += 1;
                if i + n + 3 > end {
                    bail!("invalid Bedrock EventStream header");
                }
                let name = std::str::from_utf8(&frame[i..i + n])?;
                i += n;
                if frame[i] != 7 {
                    bail!("unsupported Bedrock EventStream header type");
                }
                i += 1;
                let n = u16::from_be_bytes([frame[i], frame[i + 1]]) as usize;
                i += 2;
                if i + n > end {
                    bail!("invalid Bedrock EventStream header length");
                }
                let value = std::str::from_utf8(&frame[i..i + n])?;
                i += n;
                if name == ":event-type" {
                    event_type = value.to_owned();
                }
                if name == ":message-type" {
                    message_type = value.to_owned();
                }
            }
            let payload: Value = serde_json::from_slice(&frame[end..len - 4])?;
            if message_type == "exception" || message_type == "error" {
                bail!(
                    "Bedrock stream error: {}",
                    payload
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("upstream error")
                );
            }
            if event_type != "chunk" {
                continue;
            }
            let encoded = payload
                .get("bytes")
                .and_then(Value::as_str)
                .context("Bedrock chunk missing bytes")?;
            let inner: Value = serde_json::from_slice(&STANDARD.decode(encoded)?)?;
            if self.xai {
                let choices = inner
                    .get("choices")
                    .and_then(Value::as_array)
                    .context("Bedrock chunk missing choices")?;
                if choices
                    .iter()
                    .any(|choice| choice.get("finish_reason").is_some_and(|v| !v.is_null()))
                {
                    self.terminal = true;
                }
                out.extend_from_slice(format!("data: {inner}\n\n").as_bytes());
                continue;
            }
            let kind = inner
                .get("type")
                .and_then(Value::as_str)
                .context("Bedrock chunk missing type")?;
            if kind == "message_stop" {
                self.terminal = true;
            }
            out.extend_from_slice(format!("event: {kind}\ndata: {inner}\n\n").as_bytes());
        }
        Ok(out)
    }
    pub(crate) fn finish(&self) -> anyhow::Result<()> {
        if !self.buffer.is_empty() {
            bail!("Bedrock stream ended mid-frame");
        }
        if !self.terminal {
            bail!("Bedrock stream ended before message_stop");
        }
        Ok(())
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn sha(bytes: &[u8]) -> String {
    hex(digest::digest(&digest::SHA256, bytes).as_ref())
}
fn sign(key: &[u8], data: &str) -> Vec<u8> {
    hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), data.as_bytes())
        .as_ref()
        .to_vec()
}

fn timestamp() -> anyhow::Result<(String, String)> {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let days = (seconds / 86400) as i64 + 719468;
    let era = days / 146097;
    let doe = days - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    let date = format!("{year:04}{month:02}{day:02}");
    let t = seconds % 86400;
    Ok((
        date.clone(),
        format!("{date}T{:02}{:02}{:02}Z", t / 3600, t / 60 % 60, t % 60),
    ))
}

pub(super) fn prepare(
    endpoint: Option<&str>,
    xai: bool,
    model: &str,
    body: &mut Value,
    stream: bool,
) -> anyhow::Result<(reqwest::Url, Vec<(String, String)>, Vec<u8>)> {
    let family = if xai { "xai." } else { "anthropic." };
    if !model
        .split(['.', '/'])
        .collect::<Vec<_>>()
        .windows(2)
        .any(|parts| parts[0].eq_ignore_ascii_case(family.trim_end_matches('.')))
    {
        bail!("Bedrock expects a {family} model ID");
    }
    let region = std::env::var("AWS_REGION")
        .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
        .unwrap_or_else(|_| "us-east-1".into());
    if region.is_empty()
        || !region
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        bail!("invalid AWS region");
    }
    let (env_key, env_secret, env_session) = (
        std::env::var("AWS_ACCESS_KEY_ID").ok(),
        std::env::var("AWS_SECRET_ACCESS_KEY").ok(),
        std::env::var("AWS_SESSION_TOKEN").ok(),
    );
    let (key, secret, session) = match (env_key, env_secret) {
        (Some(k), Some(s)) if !k.is_empty() && !s.is_empty() => (k, s, env_session),
        _ => {
            let profile = std::env::var("AWS_PROFILE").unwrap_or_else(|_| "default".into());
            load_aws_profile_credentials(&profile)
                .context("missing AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY and ~/.aws/credentials profile")?
        }
    };
    let base = endpoint
        .map(str::to_owned)
        .unwrap_or_else(|| format!("https://bedrock-runtime.{region}.amazonaws.com"));
    let mut url = reqwest::Url::parse(&base).context("invalid Bedrock endpoint")?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("Bedrock URL must use HTTP or HTTPS");
    }
    url.path_segments_mut()
        .map_err(|_| anyhow::anyhow!("invalid Bedrock URL"))?
        .pop_if_empty()
        .extend([
            "model",
            model,
            if stream {
                "invoke-with-response-stream"
            } else {
                "invoke"
            },
        ]);
    if url.query().is_some() {
        bail!("Bedrock endpoint must not contain a query");
    }
    let obj = body
        .as_object_mut()
        .context("Bedrock request body must be an object")?;
    obj.remove("model");
    obj.remove("stream");
    if !xai {
        obj.insert("anthropic_version".into(), "bedrock-2023-05-31".into());
    }
    let payload = serde_json::to_vec(body)?;
    let (date, amz_date) = timestamp()?;
    let host = match url.port() {
        Some(port) => format!("{}:{port}", url.host_str().context("missing Bedrock host")?),
        None => url.host_str().context("missing Bedrock host")?.to_owned(),
    };
    let mut headers: Vec<(String, String)> = vec![
        (
            "accept".into(),
            if stream {
                "application/vnd.amazon.eventstream"
            } else {
                "application/json"
            }
            .into(),
        ),
        ("content-type".into(), "application/json".into()),
        ("host".into(), host),
        ("x-amz-date".into(), amz_date.clone()),
    ];
    if let Some(token) = session {
        headers.push(("x-amz-security-token".into(), token));
    }
    let canonical_headers: String = headers
        .iter()
        .map(|(k, v)| format!("{k}:{}\n", v.trim()))
        .collect();
    let signed_headers = headers
        .iter()
        .map(|(k, _)| k.as_str())
        .collect::<Vec<_>>()
        .join(";");
    let canonical_path = url.path().replace('%', "%25");
    let canonical = format!(
        "POST\n{canonical_path}\n\n{canonical_headers}\n{signed_headers}\n{}",
        sha(&payload)
    );
    let scope = format!("{date}/{region}/bedrock/aws4_request");
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        sha(canonical.as_bytes())
    );
    let date_key = sign(format!("AWS4{secret}").as_bytes(), &date);
    let region_key = sign(&date_key, &region);
    let service_key = sign(&region_key, "bedrock");
    let signing_key = sign(&service_key, "aws4_request");
    headers.push(("authorization".into(), format!("AWS4-HMAC-SHA256 Credential={key}/{scope}, SignedHeaders={signed_headers}, Signature={}", hex(&sign(&signing_key, &to_sign)))));
    Ok((url, headers, payload))
}
