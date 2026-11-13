use anyhow::{Context, bail};
use base64::{Engine as _, engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD}};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const UC_ORIGIN: &str = "https://uncensored.com";
const UC_CLERK_FAPI: &str = "https://clerk.uncensored.com";
const UC_CLERK_JS_VERSION: &str = "5.127.1";
const UC_IMAGE_URL: &str = "https://internal.chatuncensored.ai/v2/image-gen";
const UC_VIDEO_URL: &str = "https://internal.chatuncensored.ai/image_to_video";

pub fn parse_cookie_jar(header: &str) -> BTreeMap<String, String> {
    let mut jar = BTreeMap::new();
    for part in header.split(';') {
        if let Some((k, v)) = part.split_once('=') {
            let name = k.trim();
            if !name.is_empty() {
                jar.insert(name.to_string(), v.trim().to_string());
            }
        }
    }
    jar
}

pub fn decode_jwt_payload(token: &str) -> Option<Value> {
    let part = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(part.trim_end_matches('=')).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn aspect_for(size: Option<&str>) -> (&'static str, &'static str, &'static str) {
    let raw = size.unwrap_or("").trim();
    match raw {
        "1:1" => ("1:1", "1024", "1024"),
        "16:9" => ("16:9", "1024", "576"),
        "9:16" => ("9:16", "576", "1024"),
        "4:3" => ("4:3", "1024", "768"),
        "3:4" => ("3:4", "768", "1024"),
        "3:2" => ("3:2", "1152", "768"),
        "2:3" => ("2:3", "768", "1152"),
        "4:5" => ("4:5", "896", "1120"),
        "5:4" => ("5:4", "1120", "896"),
        "21:9" => ("21:9", "1344", "576"),
        _ => {
            if let Some((w_str, h_str)) = raw.split_once('x')
                && let (Ok(w), Ok(h)) = (w_str.parse::<f64>(), h_str.parse::<f64>())
                && w > 0.0
                && h > 0.0
            {
                let ratio = w / h;
                let candidates = [
                    ("1:1", "1024", "1024", 1.0),
                    ("16:9", "1024", "576", 16.0 / 9.0),
                    ("9:16", "576", "1024", 9.0 / 16.0),
                    ("4:3", "1024", "768", 4.0 / 3.0),
                    ("3:4", "768", "1024", 3.0 / 4.0),
                ];
                let mut best = candidates[0];
                let mut best_diff = f64::INFINITY;
                for c in candidates {
                    let diff = (c.3 - ratio).abs();
                    if diff < best_diff {
                        best_diff = diff;
                        best = c;
                    }
                }
                return (best.0, best.1, best.2);
            }
            ("1:1", "1024", "1024")
        }
    }
}

pub async fn mint_session(
    client: &reqwest::Client,
    clerk_base: Option<&str>,
    cookie_header: &str,
) -> anyhow::Result<(String, String)> {
    let jar = parse_cookie_jar(cookie_header);
    let client_jwt = jar
        .get("__client")
        .context("UC credential missing __client cookie")?;
    let clerk = clerk_base.unwrap_or(UC_CLERK_FAPI).trim_end_matches('/');

    let sid = if let Some(sid) = decode_jwt_payload(client_jwt)
        .and_then(|v| v.get("sid").and_then(Value::as_str).map(str::to_string))
        .filter(|s| !s.is_empty())
    {
        sid
    } else {
        let res = client
            .get(format!(
                "{clerk}/v1/client?_clerk_js_version={UC_CLERK_JS_VERSION}"
            ))
            .header("Cookie", cookie_header)
            .header("Origin", UC_ORIGIN)
            .header("Referer", format!("{UC_ORIGIN}/"))
            .header("Accept", "application/json")
            .send()
            .await?;
        if !res.status().is_success() {
            bail!("UC Clerk /v1/client HTTP {}", res.status());
        }
        let body: Value = serde_json::from_slice(&res.bytes().await?)?;
        let sessions = body
            .pointer("/response/sessions")
            .or_else(|| body.pointer("/client/sessions"))
            .or_else(|| body.get("sessions"))
            .and_then(Value::as_array)
            .context("UC Clerk /v1/client missing sessions")?;
        sessions
            .iter()
            .find(|s| s.get("status").and_then(Value::as_str) == Some("active"))
            .or_else(|| sessions.first())
            .and_then(|s| s.get("id"))
            .and_then(Value::as_str)
            .context("UC Clerk session id not found")?
            .to_string()
    };

    let tok_res = client
        .post(format!(
            "{clerk}/v1/client/sessions/{sid}/tokens?_clerk_js_version={UC_CLERK_JS_VERSION}"
        ))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Origin", UC_ORIGIN)
        .header("Referer", format!("{UC_ORIGIN}/"))
        .header("Cookie", cookie_header)
        .body("")
        .send()
        .await?;
    if !tok_res.status().is_success() {
        bail!("UC Clerk token mint HTTP {}", tok_res.status());
    }
    let tok_json: Value = serde_json::from_slice(&tok_res.bytes().await?)?;
    let jwt = tok_json
        .get("jwt")
        .and_then(Value::as_str)
        .context("UC Clerk token mint returned no jwt")?
        .to_string();
    let uid = decode_jwt_payload(&jwt)
        .and_then(|v| v.get("uid").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_default();
    Ok((jwt, uid))
}

pub async fn generate_media(
    client: &reqwest::Client,
    clerk_base: Option<&str>,
    api_base: Option<&str>,
    cookie_header: &str,
    kind: &str,
    prompt: &str,
    model: &str,
    size: Option<&str>,
) -> anyhow::Result<Value> {
    if prompt.trim().is_empty() {
        bail!("prompt is required for uc-web");
    }
    let (jwt, _uid) = mint_session(client, clerk_base, cookie_header).await?;
    let (aspect_ratio, width, height) = aspect_for(size);

    let (url, payload) = if kind == "video" {
        let target = api_base
            .map(|b| format!("{}/image_to_video", b.trim_end_matches('/')))
            .unwrap_or_else(|| UC_VIDEO_URL.to_string());
        let video_model = if model.is_empty() { "default" } else { model };
        (
            target,
            json!({
                "prompt": prompt,
                "media_blob_name": Value::Null,
                "num_frames": 81,
                "frames_per_second": 16,
                "num_inference_steps": 30,
                "guide_scale": 5,
                "shift": 5,
                "aspect_ratio": aspect_ratio,
                "pro_mode": video_model == "wan-3.0-uncensored-pro",
                "turbo": video_model == "turbo",
                "resolution": "480p",
                "sora_resolution": "480p",
                "end_frame_blob_name": Value::Null,
                "model": video_model,
                "seconds": 5,
                "video_to_video_duration": 5,
                "vdiscount": false,
            }),
        )
    } else {
        let target = api_base
            .map(|b| format!("{}/v2/image-gen", b.trim_end_matches('/')))
            .unwrap_or_else(|| UC_IMAGE_URL.to_string());
        let image_model = if model.is_empty() {
            "seedream-v4.5"
        } else {
            model
        };
        (
            target,
            json!({
                "prompt": prompt,
                "mode": "dev",
                "model_version": image_model,
                "m_n_user": true,
                "moderationMode": "SUPER_LIGHT",
                "country": "US",
                "vdiscount": false,
                "aspect_ratio": aspect_ratio,
                "imageWidth": width,
                "imageHeight": height,
            }),
        )
    };

    let res = client
        .post(&url)
        .header("Authorization", format!("Bearer {jwt}"))
        .header("Origin", UC_ORIGIN)
        .header("Referer", format!("{UC_ORIGIN}/"))
        .header("Content-Type", "application/json")
        .body(payload.to_string())
        .send()
        .await?;
    if !res.status().is_success() {
        bail!("UC media request HTTP {}", res.status());
    }
    let body: Value = serde_json::from_slice(&res.bytes().await?)?;
    let result_url = body
        .get("url")
        .and_then(Value::as_str)
        .context("UC response missing result url")?
        .to_string();
    let dl = client
        .get(&result_url)
        .header("Referer", format!("{UC_ORIGIN}/"))
        .send()
        .await?;
    if !dl.status().is_success() {
        bail!("UC result download HTTP {}", dl.status());
    }
    let mime = dl
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .split(';')
        .next()
        .unwrap_or("application/octet-stream")
        .to_string();
    let bytes = dl.bytes().await?;
    Ok(json!({
        "b64": STANDARD.encode(&bytes),
        "mime": mime,
        "url": result_url,
    }))
}

pub async fn execute(
    _client: &reqwest::Client,
    _request: &super::ExecutorRequest,
) -> anyhow::Result<reqwest::Response> {
    bail!("uc-web is a media provider — use /v1/images/generations or /v1/videos/generations")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aspect_mapping_handles_ratios_and_pixel_dimensions() {
        assert_eq!(aspect_for(Some("16:9")), ("16:9", "1024", "576"));
        assert_eq!(aspect_for(Some("1920x1080")), ("16:9", "1024", "576"));
        assert_eq!(aspect_for(None), ("1:1", "1024", "1024"));
    }
}
