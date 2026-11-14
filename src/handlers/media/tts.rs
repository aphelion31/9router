// Port of ttsCore.js + ttsProviders/* (openai, openrouter, elevenlabs, gemini, xiaomi-mimo,
// selfhosted-tts, ollama-local, google-tts, edge-tts, local-device, generic formats).

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use base64::Engine as _;
use serde_json::{Value, json};

use crate::handlers::Credentials;
use crate::handlers::media::common::{
    AttemptOutcome, RequestJsonExt, for_each_account, http_client, media_error, read_json,
    split_provider_model, url_encode,
};
use crate::handlers::media::config::get_tts_voices_for_model;
use crate::server::AppState;

const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36";

pub struct TtsAudio {
    pub bytes: Vec<u8>,
    pub format: String,
}

fn base_url_override(creds: Option<&Credentials>) -> Option<String> {
    creds
        .and_then(|c| c.provider_specific_data.get("baseUrl").and_then(Value::as_str))
        .map(|s| s.trim_end_matches('/').to_owned())
}

fn pcm_to_wav(pcm: &[u8]) -> Vec<u8> {
    let data_size = pcm.len() as u32;
    let sample_rate: u32 = 24_000;
    let channels: u16 = 1;
    let bits: u16 = 16;
    let byte_rate = sample_rate * u32::from(channels) * u32::from(bits) / 8;
    let block_align = channels * bits / 8;
    let mut hdr = Vec::with_capacity(44 + pcm.len());
    hdr.extend_from_slice(b"RIFF");
    hdr.extend_from_slice(&(36 + data_size).to_le_bytes());
    hdr.extend_from_slice(b"WAVEfmt ");
    hdr.extend_from_slice(&16u32.to_le_bytes());
    hdr.extend_from_slice(&1u16.to_le_bytes());
    hdr.extend_from_slice(&channels.to_le_bytes());
    hdr.extend_from_slice(&sample_rate.to_le_bytes());
    hdr.extend_from_slice(&byte_rate.to_le_bytes());
    hdr.extend_from_slice(&block_align.to_le_bytes());
    hdr.extend_from_slice(&bits.to_le_bytes());
    hdr.extend_from_slice(b"data");
    hdr.extend_from_slice(&data_size.to_le_bytes());
    hdr.extend_from_slice(pcm);
    hdr
}

fn detect_audio_format(ct: &str, default_fmt: &str) -> String {
    if ct.contains("wav") {
        "wav".to_owned()
    } else if ct.contains("mpeg") || ct.contains("mp3") {
        "mp3".to_owned()
    } else if ct.contains("ogg") {
        "ogg".to_owned()
    } else if ct.contains("pcm") {
        "pcm".to_owned()
    } else {
        default_fmt.to_owned()
    }
}

async fn upstream_err(res: reqwest::Response) -> (StatusCode, String) {
    let status = StatusCode::from_u16(res.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let txt = res.text().await.unwrap_or_default();
    if let Ok(j) = serde_json::from_str::<Value>(&txt)
        && let Some(m) = j["error"]["message"]
            .as_str()
            .or_else(|| j["message"].as_str())
            .or_else(|| j["detail"]["message"].as_str())
            .or_else(|| j["detail"].as_str())
    {
        return (status, m.to_owned());
    }
    let msg = if txt.is_empty() {
        format!("Upstream error ({})", status.as_u16())
    } else {
        txt
    };
    (status, msg)
}

pub async fn synthesize_tts(
    provider: &str,
    model: &str,
    input: &str,
    voice: Option<&str>,
    response_format: &str,
    language: Option<&str>,
    style: Option<&str>,
    creds: Option<&Credentials>,
) -> Result<TtsAudio, (StatusCode, String)> {
    let text = input.trim();
    if text.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "Missing required field: input".into()));
    }
    let client = http_client();
    let api_key = creds.and_then(|c| c.api_key.as_deref().or(c.access_token.as_deref()));
    let override_url = base_url_override(creds);

    match provider {
        "openai" => {
            let key = api_key.ok_or((StatusCode::UNAUTHORIZED, "No OpenAI API key configured".into()))?;
            let (tts_model, v) = if let Some(vo) = voice {
                (if model.is_empty() { "tts-1" } else { model }, vo)
            } else if let Some((m, vo)) = model.split_once('/') {
                (m, vo)
            } else {
                ("tts-1", if model.is_empty() { "alloy" } else { model })
            };
            let base = override_url.as_deref().unwrap_or("https://api.openai.com");
            let res = client
                .post(format!("{base}/v1/audio/speech"))
                .bearer_auth(key)
                .json_body(&json!({ "model": tts_model, "voice": v, "input": text, "response_format": response_format }))
                .send()
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            if !res.status().is_success() {
                return Err(upstream_err(res).await);
            }
            let fmt = detect_audio_format(
                res.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or(""),
                response_format,
            );
            let bytes = res.bytes().await.map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?.to_vec();
            Ok(TtsAudio { bytes, format: fmt })
        }
        "openrouter" => {
            let key = api_key.ok_or((StatusCode::UNAUTHORIZED, "No OpenRouter API key configured".into()))?;
            let (tts_model, v) = if let Some(vo) = voice {
                (if model.is_empty() { "openai/gpt-4o-mini-tts" } else { model }, vo)
            } else if let Some((m, vo)) = model.rsplit_once('/')
                && m.contains('/')
            {
                (m, vo)
            } else {
                ("openai/gpt-4o-mini-tts", if model.is_empty() { "alloy" } else { model })
            };
            let url = override_url
                .as_deref()
                .map(|b| format!("{b}/api/v1/chat/completions"))
                .unwrap_or_else(|| "https://openrouter.ai/api/v1/chat/completions".to_owned());
            let res = client
                .post(url)
                .bearer_auth(key)
                .json_body(&json!({
                    "model": tts_model,
                    "modalities": ["text", "audio"],
                    "audio": { "voice": v, "format": "wav" },
                    "stream": true,
                    "messages": [{ "role": "user", "content": text }]
                }))
                .send()
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            if !res.status().is_success() {
                return Err(upstream_err(res).await);
            }
            let body = res.text().await.map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            let mut b64 = String::new();
            for line in body.lines() {
                let Some(data) = line.strip_prefix("data: ") else { continue };
                if data == "[DONE]" {
                    break;
                }
                if let Ok(j) = serde_json::from_str::<Value>(data)
                    && let Some(chunk) = j["choices"][0]["delta"]["audio"]["data"].as_str()
                {
                    b64.push_str(chunk);
                }
            }
            if b64.is_empty() {
                return Err((StatusCode::BAD_GATEWAY, "OpenRouter TTS returned no audio data".into()));
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&b64)
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            Ok(TtsAudio { bytes, format: "wav".into() })
        }
        "elevenlabs" => {
            let key = api_key.ok_or((StatusCode::UNAUTHORIZED, "ElevenLabs API key required".into()))?;
            let (model_id, voice_id) = if let Some((m, vo)) = model.split_once('/') {
                (m, vo)
            } else {
                ("eleven_flash_v2_5", voice.unwrap_or(if model.is_empty() { "21m00Tcm4TlvDq8ikWAM" } else { model }))
            };
            let base = override_url.as_deref().unwrap_or("https://api.elevenlabs.io");
            let res = client
                .post(format!("{base}/v1/text-to-speech/{voice_id}"))
                .header("xi-api-key", key)
                .json_body(&json!({
                    "text": text,
                    "model_id": model_id,
                    "voice_settings": { "stability": 0.5, "similarity_boost": 0.75 }
                }))
                .send()
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            if !res.status().is_success() {
                return Err(upstream_err(res).await);
            }
            let bytes = res.bytes().await.map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?.to_vec();
            Ok(TtsAudio { bytes, format: "mp3".into() })
        }
        "gemini" => {
            let key = api_key.ok_or((StatusCode::UNAUTHORIZED, "No Gemini API key configured".into()))?;
            let (model_id, voice_id) = if let Some((m, vo)) = model.split_once('/') {
                (m, vo)
            } else {
                ("gemini-2.5-flash-preview-tts", voice.unwrap_or(if model.is_empty() { "Kore" } else { model }))
            };
            let prompt = if text.contains(": ") {
                text.to_owned()
            } else if let Some(lang) = language {
                format!("Say in {lang}: {text}")
            } else {
                format!("Say: {text}")
            };
            let base = override_url
                .as_deref()
                .unwrap_or("https://generativelanguage.googleapis.com/v1beta/models");
            let res = client
                .post(format!("{base}/{model_id}:generateContent?key={key}"))
                .json_body(&json!({
                    "contents": [{ "parts": [{ "text": prompt }] }],
                    "generationConfig": {
                        "responseModalities": ["AUDIO"],
                        "speechConfig": { "voiceConfig": { "prebuiltVoiceConfig": { "voiceName": voice_id } } }
                    }
                }))
                .send()
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            if !res.status().is_success() {
                return Err(upstream_err(res).await);
            }
            let data: Value = read_json(res).await.map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            let b64 = data["candidates"][0]["content"]["parts"]
                .as_array()
                .and_then(|parts| parts.iter().find_map(|p| p["inlineData"]["data"].as_str()))
                .ok_or((StatusCode::BAD_GATEWAY, "Gemini TTS returned no audio".into()))?;
            let pcm = base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            Ok(TtsAudio { bytes: pcm_to_wav(&pcm), format: "wav".into() })
        }
        "xiaomi-mimo" => {
            let key = api_key.ok_or((StatusCode::UNAUTHORIZED, "xiaomi-mimo API key required".into()))?;
            let (model_id, voice_id) = if let Some((m, vo)) = model.split_once('/') {
                (m, vo)
            } else {
                ("mimo-v2.5-tts", voice.unwrap_or("mimo_default"))
            };
            let mut instr = Vec::new();
            if let Some(l) = language {
                instr.push(format!("Speak in {l}."));
            }
            if let Some(s) = style {
                instr.push(s.to_owned());
            }
            let mut messages = Vec::new();
            if !instr.is_empty() {
                messages.push(json!({ "role": "user", "content": instr.join(" ") }));
            }
            messages.push(json!({ "role": "assistant", "content": text }));
            let base = override_url.as_deref().unwrap_or("https://api.xiaomimimo.com");
            let res = client
                .post(format!("{base}/v1/chat/completions"))
                .bearer_auth(key)
                .json_body(&json!({
                    "model": model_id,
                    "stream": false,
                    "messages": messages,
                    "audio": { "format": "wav", "voice": voice_id }
                }))
                .send()
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            if !res.status().is_success() {
                return Err(upstream_err(res).await);
            }
            let data: Value = read_json(res).await.map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            let b64 = data["choices"][0]["message"]["audio"]["data"]
                .as_str()
                .ok_or((StatusCode::BAD_GATEWAY, "MiMo TTS returned no audio".into()))?;
            let fmt = data["choices"][0]["message"]["audio"]["format"]
                .as_str()
                .unwrap_or("wav")
                .to_owned();
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            Ok(TtsAudio { bytes, format: fmt })
        }
        "selfhosted-tts" | "ollama-local" => {
            let default_base = if provider == "ollama-local" {
                "http://localhost:11434"
            } else {
                "http://localhost:8880"
            };
            let raw = override_url.as_deref().unwrap_or(default_base);
            let base = raw
                .trim_end_matches('/')
                .trim_end_matches("/v1/audio/speech")
                .trim_end_matches("/v1");
            let (tts_model, v) = if let Some((m, vo)) = model.split_once('/') {
                (m, vo)
            } else if provider == "ollama-local" {
                (if model.is_empty() { "phonon:multilingual" } else { model }, voice.unwrap_or("default"))
            } else {
                (if model.is_empty() { "kokoro" } else { model }, voice.unwrap_or("af_heart"))
            };
            let mut req = client.post(format!("{base}/v1/audio/speech"));
            if let Some(k) = api_key {
                req = req.bearer_auth(k);
            }
            let res = req
                .json_body(&json!({
                    "model": tts_model,
                    "voice": v,
                    "input": text,
                    "response_format": if provider == "ollama-local" && response_format != "pcm" { "wav" } else { response_format }
                }))
                .send()
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            if !res.status().is_success() {
                return Err(upstream_err(res).await);
            }
            let fmt = detect_audio_format(
                res.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or(""),
                response_format,
            );
            let bytes = res.bytes().await.map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?.to_vec();
            Ok(TtsAudio { bytes, format: fmt })
        }
        "deepgram" => {
            let key = api_key.ok_or((StatusCode::UNAUTHORIZED, "deepgram API key required".into()))?;
            let m = if model.is_empty() { "aura-asteria-en" } else { model };
            let base = override_url.as_deref().unwrap_or("https://api.deepgram.com/v1/speak");
            let res = client
                .post(format!("{base}?model={m}"))
                .header("Authorization", format!("Token {key}"))
                .json_body(&json!({ "text": text }))
                .send()
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            if !res.status().is_success() {
                return Err(upstream_err(res).await);
            }
            let bytes = res.bytes().await.map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?.to_vec();
            Ok(TtsAudio { bytes, format: "mp3".into() })
        }
        "cartesia" => {
            let key = api_key.ok_or((StatusCode::UNAUTHORIZED, "cartesia API key required".into()))?;
            let (model_id, voice_id) = model.split_once('/').unwrap_or((if model.is_empty() { "sonic-2" } else { model }, voice.unwrap_or("")));
            let base = override_url.as_deref().unwrap_or("https://api.cartesia.ai/tts/bytes");
            let mut body = json!({
                "model_id": model_id,
                "transcript": text,
                "output_format": { "container": "mp3", "bit_rate": 128000, "sample_rate": 44100 }
            });
            if !voice_id.is_empty() {
                body["voice"] = json!({ "mode": "id", "id": voice_id });
            }
            let res = client
                .post(base)
                .header("X-API-Key", key)
                .header("Cartesia-Version", "2024-06-10")
                .json_body(&body)
                .send()
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            if !res.status().is_success() {
                return Err(upstream_err(res).await);
            }
            let bytes = res.bytes().await.map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?.to_vec();
            Ok(TtsAudio { bytes, format: "mp3".into() })
        }
        "inworld" => {
            let key = api_key.ok_or((StatusCode::UNAUTHORIZED, "inworld API key required".into()))?;
            let (model_id, voice_id) = model.split_once('/').unwrap_or((if model.is_empty() { "inworld-tts-1.5-mini" } else { model }, voice.unwrap_or("Alex")));
            let base = override_url.as_deref().unwrap_or("https://api.inworld.ai/tts/v1/voice");
            let res = client
                .post(base)
                .header("Authorization", format!("Basic {key}"))
                .json_body(&json!({
                    "text": text,
                    "voiceId": voice_id,
                    "modelId": model_id,
                    "audioConfig": { "audioEncoding": "MP3" }
                }))
                .send()
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            if !res.status().is_success() {
                return Err(upstream_err(res).await);
            }
            let data: Value = read_json(res).await.map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            let b64 = data["audioContent"].as_str().ok_or((StatusCode::BAD_GATEWAY, "Inworld TTS returned no audio".into()))?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            Ok(TtsAudio { bytes, format: "mp3".into() })
        }
        "google-tts" => {
            let lang = if model.is_empty() { "en" } else { model };
            let html = client
                .get("https://translate.google.com/")
                .header("User-Agent", UA)
                .send()
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?
                .text()
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            let f_sid = extract_between(&html, "\"FdrFJe\":\"", "\"")
                .ok_or((StatusCode::BAD_GATEWAY, "Failed to parse Google token".into()))?;
            let bl = extract_between(&html, "\"cfb2h\":\"", "\"")
                .ok_or((StatusCode::BAD_GATEWAY, "Failed to parse Google token".into()))?;
            let payload = json!([text, lang, null, "undefined", [0]]);
            let f_req = json!([[["jQ1olc", payload.to_string(), null, "generic"]]]).to_string();
            let qs = format!(
                "rpcids=jQ1olc&f.sid={}&bl={}&hl={}&soc-app=1&soc-platform=1&soc-device=1&rt=c",
                url_encode(f_sid),
                url_encode(bl),
                url_encode(lang),
            );
            let form_body = format!("f.req={}", url_encode(&f_req));
            let res = client
                .post(format!(
                    "https://translate.google.com/_/TranslateWebserverUi/data/batchexecute?{qs}"
                ))
                .header("Referer", "https://translate.google.com/")
                .header("Content-Type", "application/x-www-form-urlencoded")
                .body(form_body)
                .send()
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            if !res.status().is_success() {
                let st = StatusCode::from_u16(res.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
                return Err((st, format!("Google TTS failed: {}", res.status())));
            }
            let body = res.text().await.map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            let line4 = body.lines().nth(3).ok_or((StatusCode::BAD_GATEWAY, "Google TTS invalid response".into()))?;
            let split: Value = serde_json::from_str(line4).map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            let inner: Value = serde_json::from_str(split[0][2].as_str().unwrap_or("[]"))
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            let b64 = inner[0].as_str().ok_or((StatusCode::BAD_GATEWAY, "Google TTS returned empty audio".into()))?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            Ok(TtsAudio { bytes, format: "mp3".into() })
        }
        "local-device" => {
            let dir = std::env::temp_dir().join(format!("tts-{}", std::process::id()));
            std::fs::create_dir_all(&dir).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            let aiff = dir.join("out.aiff");
            let mp3 = dir.join("out.mp3");
            let mut cmd = tokio::process::Command::new("say");
            if !model.is_empty() && model != "default" {
                cmd.arg("-v").arg(model);
            }
            let st = cmd.arg("-o").arg(&aiff).arg(text).status().await
                .map_err(|e| (StatusCode::BAD_GATEWAY, format!("say failed: {e}")))?;
            if !st.success() {
                let _ = std::fs::remove_dir_all(&dir);
                return Err((StatusCode::BAD_GATEWAY, "say exited non-zero".into()));
            }
            let ff = tokio::process::Command::new("ffmpeg")
                .args(["-y", "-i"])
                .arg(&aiff)
                .args(["-codec:a", "libmp3lame", "-qscale:a", "4"])
                .arg(&mp3)
                .status()
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, format!("ffmpeg failed: {e}")))?;
            let bytes = if ff.success() {
                std::fs::read(&mp3).unwrap_or_default()
            } else {
                Vec::new()
            };
            let _ = std::fs::remove_dir_all(&dir);
            if bytes.is_empty() {
                return Err((StatusCode::BAD_GATEWAY, "local-device TTS produced empty audio".into()));
            }
            Ok(TtsAudio { bytes, format: "mp3".into() })
        }
        _ => Err((
            StatusCode::BAD_REQUEST,
            format!("Provider '{provider}' does not support TTS via this route."),
        )),
    }
}

fn extract_between<'a>(haystack: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let i = haystack.find(start)? + start.len();
    let j = haystack[i..].find(end)?;
    Some(&haystack[i..i + j])
}

pub async fn audio_speech(State(state): State<AppState>, request_headers: HeaderMap, Json(body): Json<Value>) -> Response {
    let input = body["input"].as_str().unwrap_or("");
    if input.trim().is_empty() {
        return media_error(StatusCode::BAD_REQUEST, "Missing required field: input");
    }
    let raw_model = body["model"].as_str().unwrap_or("openai/tts-1");
    let (provider, model) = split_provider_model(raw_model, "openai");
    let voice = body["voice"].as_str();
    let response_format = body["response_format"].as_str().unwrap_or("mp3");
    let language = body["language"].as_str();
    let style = body["style"].as_str();
    let allow_noauth = matches!(
        provider.as_str(),
        "selfhosted-tts" | "ollama-local" | "google-tts" | "edge-tts" | "local-device"
    );

    for_each_account(
        &state,
        &provider,
        &model,
        allow_noauth,
        || {
            media_error(
                StatusCode::UNAUTHORIZED,
                format!("No credentials for TTS provider: {provider}"),
            )
        },
        |creds| {
            let state = &state;
            let request_headers = &request_headers;
            let provider = &provider;
            let model = &model;
            async move {
                match synthesize_tts(
                    provider,
                    model,
                    input,
                    voice,
                    response_format,
                    language,
                    style,
                    creds.as_ref(),
                )
                .await
                {
                    Ok(audio) => {
                        crate::services::modality_usage::record_modality_usage(
                            state,
                            provider,
                            model,
                            creds.as_ref().map(|c| c.connection_id.as_str()),
                            crate::server::api_key_id(state, request_headers).as_deref(),
                            "/v1/audio/speech",
                            crate::services::modality_usage::estimate_tokens_from_text(input),
                            0,
                            "estimated",
                        );
                        if response_format == "json" {
                            let b64 = base64::engine::general_purpose::STANDARD.encode(&audio.bytes);
                            return AttemptOutcome::Success(
                                (
                                    StatusCode::OK,
                                    [("Access-Control-Allow-Origin", "*")],
                                    Json(json!({ "audio": b64, "format": audio.format })),
                                )
                                    .into_response(),
                            );
                        }
                        let mut headers = HeaderMap::new();
                        headers.insert("Content-Type", format!("audio/{}", audio.format).parse().unwrap());
                        headers.insert("Access-Control-Allow-Origin", "*".parse().unwrap());
                        AttemptOutcome::Success((StatusCode::OK, headers, audio.bytes).into_response())
                    }
                    Err((status, msg)) => AttemptOutcome::Failure {
                        status,
                        message: msg.clone(),
                        response: media_error(status, msg),
                    },
                }
            }
        },
    )
    .await
}

pub async fn audio_voices(
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let provider = params.get("provider").map(String::as_str).unwrap_or("openai");
    let model = params.get("model").map(String::as_str).unwrap_or("tts-1");
    let voices = get_tts_voices_for_model(provider, model).unwrap_or(&[]);
    (StatusCode::OK, Json(json!({ "provider": provider, "model": model, "voices": voices }))).into_response()
}
