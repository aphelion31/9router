// Port of sttCore.js + codexStt.js + geminiLiveStt.js

use axum::{
    Json,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use base64::Engine as _;
use serde_json::{Value, json};

use crate::handlers::Credentials;
use crate::handlers::media::common::{
    RequestJsonExt, http_client, media_error, read_json, resolve_credentials, split_provider_model,
};
use crate::handlers::media::multipart::{MultipartBuilder, extract_boundary, parse_multipart};
use crate::server::AppState;

fn resolve_audio_content_type(ct: Option<&str>, filename: Option<&str>) -> String {
    if let Some(t) = ct {
        let lower = t.to_ascii_lowercase();
        if lower.starts_with("audio/") {
            return lower;
        }
    }
    let ext = filename
        .and_then(|n| n.rsplit_once('.').map(|x| x.1.to_ascii_lowercase()))
        .unwrap_or_default();
    match ext.as_str() {
        "mp3" => "audio/mpeg",
        "mp4" | "m4a" => "audio/mp4",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "flac" => "audio/flac",
        "webm" => "audio/webm",
        "aac" => "audio/aac",
        "opus" => "audio/opus",
        _ => "application/octet-stream",
    }
    .to_owned()
}

struct SttProviderCfg {
    base_url: &'static str,
    format: &'static str,
    auth_header: &'static str,
}

fn default_stt_config(provider: &str) -> Option<SttProviderCfg> {
    match provider {
        "openai" => Some(SttProviderCfg {
            base_url: "https://api.openai.com/v1/audio/transcriptions",
            format: "openai",
            auth_header: "bearer",
        }),
        "groq" => Some(SttProviderCfg {
            base_url: "https://api.groq.com/openai/v1/audio/transcriptions",
            format: "openai",
            auth_header: "bearer",
        }),
        "deepgram" => Some(SttProviderCfg {
            base_url: "https://api.deepgram.com/v1/listen",
            format: "deepgram",
            auth_header: "token",
        }),
        "elevenlabs" => Some(SttProviderCfg {
            base_url: "https://api.elevenlabs.io/v1/speech-to-text",
            format: "elevenlabs-stt",
            auth_header: "xi-api-key",
        }),
        "gemini" => Some(SttProviderCfg {
            base_url: "https://generativelanguage.googleapis.com/v1beta/models",
            format: "gemini-stt",
            auth_header: "query",
        }),
        "nvidia" => Some(SttProviderCfg {
            base_url: "https://integrate.api.nvidia.com/v1/audio/transcriptions",
            format: "nvidia-asr",
            auth_header: "bearer",
        }),
        "huggingface" => Some(SttProviderCfg {
            base_url: "https://router.huggingface.co/hf-inference/models",
            format: "huggingface-asr",
            auth_header: "bearer",
        }),
        "codex" => Some(SttProviderCfg {
            base_url: "https://chatgpt.com/backend-api/transcribe",
            format: "codex-transcribe",
            auth_header: "bearer",
        }),
        _ => None,
    }
}

fn apply_auth(req: reqwest::RequestBuilder, auth_header: &str, token: &str) -> reqwest::RequestBuilder {
    match auth_header {
        "token" => req.header("Authorization", format!("Token {token}")),
        "x-api-key" => req.header("x-api-key", token),
        "xi-api-key" => req.header("xi-api-key", token),
        "key" => req.header("Authorization", format!("Key {token}")),
        _ => req.bearer_auth(token),
    }
}

pub async fn audio_transcriptions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(boundary) = extract_boundary(&headers) else {
        return media_error(StatusCode::BAD_REQUEST, "Expected multipart/form-data with boundary");
    };
    let form = match parse_multipart(&body, &boundary) {
        Ok(f) => f,
        Err(e) => return media_error(StatusCode::BAD_REQUEST, e),
    };
    let Some(file) = form.get_file("file") else {
        return media_error(StatusCode::BAD_REQUEST, "Missing required field: file");
    };
    let raw_model = form.get_text("model").unwrap_or_else(|| "openai/whisper-1".into());
    let (provider, model) = split_provider_model(&raw_model, "openai");

    let Some(cfg) = default_stt_config(&provider) else {
        return media_error(
            StatusCode::BAD_REQUEST,
            format!("Provider '{provider}' does not support STT"),
        );
    };
    let creds: Option<Credentials> = resolve_credentials(&state, &provider);
    let override_url = creds
        .as_ref()
        .and_then(|c| c.provider_specific_data.get("baseUrl").and_then(Value::as_str))
        .map(|s| s.trim_end_matches('/').to_owned());
    let base_url = override_url.as_deref().unwrap_or(cfg.base_url);

    let token = creds
        .as_ref()
        .and_then(|c| c.api_key.as_deref().or(c.access_token.as_deref()));
    let Some(token) = token else {
        return media_error(
            StatusCode::UNAUTHORIZED,
            format!("No credentials for STT provider: {provider}"),
        );
    };

    let client = http_client();
    let audio_ct = resolve_audio_content_type(file.content_type.as_deref(), file.filename.as_deref());
    let filename = file.filename.as_deref().unwrap_or("audio.wav");

    match cfg.format {
        "deepgram" => {
            let mut url = format!("{base_url}?model={model}&smart_format=true&punctuate=true");
            if let Some(lang) = form.get_text("language").filter(|s| !s.trim().is_empty()) {
                url.push_str(&format!("&language={}", lang.trim()));
            } else {
                url.push_str("&detect_language=true");
            }
            let req = apply_auth(client.post(url), cfg.auth_header, token)
                .header("Content-Type", audio_ct)
                .body(file.data.clone());
            match req.send().await {
                Ok(res) if res.status().is_success() => {
                    let data: Value = read_json(res).await.unwrap_or(Value::Null);
                    let text = data["results"]["channels"][0]["alternatives"][0]["transcript"]
                        .as_str()
                        .unwrap_or("");
                    (StatusCode::OK, Json(json!({ "text": text }))).into_response()
                }
                Ok(res) => {
                    let st = StatusCode::from_u16(res.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
                    let msg = res.text().await.unwrap_or_default();
                    media_error(st, msg)
                }
                Err(e) => media_error(StatusCode::BAD_GATEWAY, e.to_string()),
            }
        }
        "gemini-stt" => {
            let b64 = base64::engine::general_purpose::STANDARD.encode(&file.data);
            let mut prompt = form
                .get_text("prompt")
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| {
                    "Generate a transcript of the speech. Return only the transcribed text, no commentary."
                        .into()
                });
            if let Some(lang) = form.get_text("language").filter(|s| !s.trim().is_empty()) {
                prompt.push_str(&format!(" Language: {}.", lang.trim()));
            }
            let url = format!("{base_url}/{model}:generateContent?key={token}");
            let res = client
                .post(url)
                .json_body(&json!({
                    "contents": [{ "parts": [{ "text": prompt }, { "inline_data": { "mime_type": audio_ct, "data": b64 } }] }]
                }))
                .send()
                .await;
            match res {
                Ok(r) if r.status().is_success() => {
                    let data: Value = read_json(r).await.unwrap_or(Value::Null);
                    let text = data["candidates"][0]["content"]["parts"]
                        .as_array()
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|p| p["text"].as_str())
                                .collect::<Vec<_>>()
                                .join("")
                        })
                        .unwrap_or_default();
                    (StatusCode::OK, Json(json!({ "text": text }))).into_response()
                }
                Ok(r) => {
                    let st = StatusCode::from_u16(r.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
                    media_error(st, r.text().await.unwrap_or_default())
                }
                Err(e) => media_error(StatusCode::BAD_GATEWAY, e.to_string()),
            }
        }
        "huggingface-asr" => {
            if model.contains("..") || model.contains("//") {
                return media_error(StatusCode::BAD_REQUEST, "Invalid model ID");
            }
            let url = format!("{base_url}/{model}");
            match apply_auth(client.post(url), cfg.auth_header, token)
                .header("Content-Type", audio_ct)
                .body(file.data.clone())
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => {
                    let data: Value = read_json(r).await.unwrap_or(Value::Null);
                    (StatusCode::OK, Json(json!({ "text": data["text"].as_str().unwrap_or("") }))).into_response()
                }
                Ok(r) => media_error(StatusCode::BAD_GATEWAY, r.text().await.unwrap_or_default()),
                Err(e) => media_error(StatusCode::BAD_GATEWAY, e.to_string()),
            }
        }
        _ => {
            // OpenAI / Groq / Nvidia / ElevenLabs / Codex multipart forwarding
            let mut builder = MultipartBuilder::new("----9router-stt-boundary")
                .file("file", filename, &audio_ct, &file.data);
            if cfg.format == "elevenlabs-stt" {
                builder = builder.field("model_id", &model);
                if let Some(lang) = form.get_text("language").filter(|s| !s.trim().is_empty()) {
                    builder = builder.field("language_code", lang.trim());
                }
            } else {
                builder = builder.field("model", &model);
                for k in ["language", "prompt", "response_format", "temperature"] {
                    if let Some(val) = form.get_text(k).filter(|s| !s.is_empty()) {
                        builder = builder.field(k, &val);
                    }
                }
            }
            let (ct, payload) = builder.finish();
            match apply_auth(client.post(base_url), cfg.auth_header, token)
                .header("Content-Type", ct)
                .body(payload)
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => {
                    let rct = r
                        .headers()
                        .get("content-type")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("application/json")
                        .to_owned();
                    let txt = r.text().await.unwrap_or_default();
                    let mut h = HeaderMap::new();
                    h.insert("Content-Type", rct.parse().unwrap());
                    h.insert("Access-Control-Allow-Origin", "*".parse().unwrap());
                    (StatusCode::OK, h, txt).into_response()
                }
                Ok(r) => {
                    let st = StatusCode::from_u16(r.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
                    media_error(st, r.text().await.unwrap_or_default())
                }
                Err(e) => media_error(StatusCode::BAD_GATEWAY, e.to_string()),
            }
        }
    }
}
