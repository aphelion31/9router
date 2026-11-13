use anyhow::{Context, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::net::IpAddr;

const BASE_URL: &str = "https://veoaifree.com";
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/149.0.0.0 Safari/537.36";

pub fn map_aspect_ratio(size: Option<&str>, prefix: &str) -> String {
    let val = size.unwrap_or("").to_ascii_lowercase();
    if val.contains("9:16") || val.contains("portrait") {
        format!("{prefix}_PORTRAIT")
    } else if val.contains("1:1") || val.contains("square") {
        format!("{prefix}_SQUARE")
    } else {
        format!("{prefix}_LANDSCAPE")
    }
}

pub fn extract_nonce(html: &str) -> Option<String> {
    let marker = "nonce\":\"";
    let start = html.find(marker)? + marker.len();
    let rest = &html[start..];
    let end = rest.find('"')?;
    let hex = &rest[..end];
    if !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        Some(hex.to_string())
    } else {
        None
    }
}

pub fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_multicast()
                || v4.octets()[0] >= 224
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

pub async fn generate_image(
    client: &reqwest::Client,
    base_url: Option<&str>,
    prompt: &str,
    size: Option<&str>,
) -> anyhow::Result<Vec<Value>> {
    if prompt.trim().is_empty() {
        bail!("prompt is required for veoaifree-web");
    }
    let base = base_url.unwrap_or(BASE_URL).trim_end_matches('/');
    let html = client
        .get(base)
        .header("User-Agent", USER_AGENT)
        .send()
        .await?
        .text()
        .await?;
    let nonce = extract_nonce(&html)
        .context("failed to extract CSRF nonce from veoaifree.com")?;

    let aspect = map_aspect_ratio(size, "IMAGE_ASPECT_RATIO");
    let form = [
        ("action", "veo_video_generator"),
        ("nonce", nonce.as_str()),
        ("promptIMG", prompt),
        ("totalVariationsIMG", "1"),
        ("aspectRatioIMG", aspect.as_str()),
        ("actionType", "banan-image-generator"),
    ];
    let encoded_form = form
        .iter()
        .map(|(k, v)| format!("{}={}", form_encode(k), form_encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    let res_text = client
        .post(format!("{base}/wp-admin/admin-ajax.php"))
        .header("User-Agent", USER_AGENT)
        .header("Origin", base)
        .header("Referer", format!("{base}/"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(encoded_form)
        .send()
        .await?
        .text()
        .await?;
    let trimmed = res_text.trim();
    if trimmed.is_empty() || trimmed == "0" || trimmed.to_ascii_lowercase().contains("error") {
        bail!("veoaifree image generation failed: {trimmed}");
    }
    let items = trimmed
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|part| {
            if part.starts_with("http://") || part.starts_with("https://") {
                json!({ "url": part })
            } else {
                json!({ "b64": part, "mime": "image/png" })
            }
        })
        .collect();
    Ok(items)
}

fn form_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for b in input.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'*' => {
                out.push(b as char);
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn encode_mp4_b64(bytes: &[u8]) -> anyhow::Result<String> {
    if bytes.len() < 12 || &bytes[4..8] != b"ftyp" {
        bail!("artifact is not a valid MP4 file");
    }
    Ok(STANDARD.encode(bytes))
}

pub async fn execute(
    _client: &reqwest::Client,
    _request: &super::ExecutorRequest,
) -> anyhow::Result<reqwest::Response> {
    bail!("veoaifree-web is a media provider — use /v1/images/generations or /v1/videos/generations")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonce_and_aspect_helpers_work() {
        assert_eq!(
            extract_nonce(r#"{"ajax":"x","nonce":"deadbeef01"}"#).as_deref(),
            Some("deadbeef01")
        );
        assert_eq!(
            map_aspect_ratio(Some("9:16"), "VIDEO_ASPECT_RATIO"),
            "VIDEO_ASPECT_RATIO_PORTRAIT"
        );
        assert_eq!(
            map_aspect_ratio(Some("16:9"), "IMAGE_ASPECT_RATIO"),
            "IMAGE_ASPECT_RATIO_LANDSCAPE"
        );
        assert!(is_private_ip("127.0.0.1".parse().unwrap()));
        assert!(!is_private_ip("8.8.8.8".parse().unwrap()));
    }
}
