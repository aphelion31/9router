// Port of imageGenerationCore.js + imageProviders/*

use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use base64::Engine as _;
use serde_json::{Value, json};

use crate::handlers::media::common::{
    AttemptOutcome, RequestJsonExt, for_each_account, http_client, media_error, read_json,
    split_provider_model,
};
use crate::server::AppState;

fn default_image_url(provider: &str, model: &str) -> Option<String> {
    match provider {
        "openai" => Some("https://api.openai.com/v1/images/generations".into()),
        "xai" => Some("https://api.x.ai/v1/images/generations".into()),
        "together" => Some("https://api.together.xyz/v1/images/generations".into()),
        "fireworks" => Some(format!("https://api.fireworks.ai/inference/v1/workflows/{model}/text_to_image")),
        "nebius" => Some("https://api.studio.nebius.ai/v1/images/generations".into()),
        "openrouter" => Some("https://openrouter.ai/api/v1/chat/completions".into()),
        "gemini" | "nanobanana" => Some(format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent"
        )),
        "huggingface" => Some(format!(
            "https://router.huggingface.co/hf-inference/models/{model}"
        )),
        "black-forest-labs" => Some(format!("https://api.bfl.ai/v1/{model}")),
        "fal-ai" => Some(format!("https://fal.run/{model}")),
        "stability-ai" => Some(format!(
            "https://api.stability.ai/v2beta/stable-image/generate/{model}"
        )),
        "runwayml" => Some("https://api.dev.runwayml.com/v1/text_to_image".into()),
        "sdwebui" => Some("http://127.0.0.1:7860/sdapi/v1/txt2img".into()),
        "comfyui" => Some("http://127.0.0.1:8188/prompt".into()),
        _ => None,
    }
}

fn normalize_openai_shape(data: Value, _prompt: &str) -> Value {
    if data["created"].is_number() && data["data"].is_array() {
        return data;
    }
    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Gemini / NanoBanana inlineData
    if let Some(parts) = data["candidates"][0]["content"]["parts"].as_array() {
        let items: Vec<Value> = parts
            .iter()
            .filter_map(|p| {
                p["inlineData"]["data"]
                    .as_str()
                    .or_else(|| p["inline_data"]["data"].as_str())
                    .map(|b64| json!({ "b64_json": b64 }))
            })
            .collect();
        if !items.is_empty() {
            return json!({ "created": created, "data": items });
        }
    }
    // fal-ai / black-forest-labs / generic { images: [{ url }] }
    if let Some(imgs) = data["images"].as_array() {
        let items: Vec<Value> = imgs
            .iter()
            .filter_map(|img| {
                if let Some(u) = img["url"].as_str() {
                    Some(json!({ "url": u }))
                } else {
                    img.as_str().map(|b| json!({ "b64_json": b }))
                }
            })
            .collect();
        return json!({ "created": created, "data": items });
    }
    // OpenRouter chat completions image output
    if let Some(imgs) = data["choices"][0]["message"]["images"].as_array() {
        let items: Vec<Value> = imgs
            .iter()
            .filter_map(|i| {
                i["image_url"]["url"].as_str().map(|u| {
                    if let Some((_, b64)) = u.split_once(";base64,") {
                        json!({ "b64_json": b64 })
                    } else {
                        json!({ "url": u })
                    }
                })
            })
            .collect();
        return json!({ "created": created, "data": items });
    }
    data
}

pub async fn image_generations(State(state): State<AppState>, headers: axum::http::HeaderMap, Json(body): Json<Value>) -> Response {
    let prompt = body["prompt"].as_str().unwrap_or("");
    if prompt.trim().is_empty() {
        return media_error(StatusCode::BAD_REQUEST, "Missing required field: prompt");
    }

    let raw_model = body["model"].as_str().unwrap_or("openai/dall-e-3");
    let (provider, model) = split_provider_model(raw_model, "openai");
    let allow_noauth = matches!(provider.as_str(), "sdwebui" | "comfyui");
    let req_body = match provider.as_str() {
        "gemini" | "nanobanana" => json!({
            "contents": [{ "parts": [{ "text": prompt }] }],
            "generationConfig": { "responseModalities": ["IMAGE", "TEXT"] }
        }),
        "openrouter" => json!({
            "model": model,
            "modalities": ["image", "text"],
            "messages": [{ "role": "user", "content": prompt }]
        }),
        "huggingface" | "cloudflare-ai" => json!({ "prompt": prompt }),
        "fal-ai" => json!({ "prompt": prompt, "num_images": body["n"].as_u64().unwrap_or(1) }),
        "sdwebui" => json!({
            "prompt": prompt,
            "steps": body["steps"].as_u64().unwrap_or(20),
            "batch_size": body["n"].as_u64().unwrap_or(1)
        }),
        _ => {
            let mut o = json!({
                "model": model,
                "prompt": prompt,
                "n": body["n"].as_u64().unwrap_or(1),
            });
            if let Some(sz) = body["size"].as_str() {
                o["size"] = json!(sz);
            }
            if let Some(rf) = body["response_format"].as_str() {
                o["response_format"] = json!(rf);
            }
            o
        }
    };

    for_each_account(
        &state,
        &provider,
        &model,
        allow_noauth,
        || {
            media_error(
                StatusCode::UNAUTHORIZED,
                format!("No credentials for image provider: {provider}"),
            )
        },
        |creds| {
            let state = &state;
            let headers = &headers;
            let provider = &provider;
            let model = &model;
            let req_body = &req_body;
            async move {
                let override_url = creds
                    .as_ref()
                    .and_then(|c| c.provider_specific_data.get("baseUrl").and_then(Value::as_str))
                    .map(|s| s.trim_end_matches('/').to_owned());

                let token = creds
                    .as_ref()
                    .and_then(|c| c.api_key.as_deref().or(c.access_token.as_deref()));

                let target_url = if provider == "cloudflare-ai" {
                    let Some(acct) = creds
                        .as_ref()
                        .and_then(|c| c.provider_specific_data.get("accountId").and_then(Value::as_str))
                    else {
                        let msg = "cloudflare-ai requires providerSpecificData.accountId".to_owned();
                        return AttemptOutcome::Failure {
                            status: StatusCode::BAD_REQUEST,
                            message: msg.clone(),
                            response: media_error(StatusCode::BAD_REQUEST, msg),
                        };
                    };
                    override_url.unwrap_or_else(|| {
                        format!("https://api.cloudflare.com/client/v4/accounts/{acct}/ai/run/{model}")
                    })
                } else if let Some(ov) = override_url {
                    if ov.contains("/images/generations")
                        || ov.contains("/chat/completions")
                        || ov.contains(":generateContent")
                    {
                        ov
                    } else {
                        format!("{ov}/v1/images/generations")
                    }
                } else if let Some(u) = default_image_url(provider, model) {
                    u
                } else {
                    let msg = format!("Provider '{provider}' does not support image generation");
                    return AttemptOutcome::Failure {
                        status: StatusCode::BAD_REQUEST,
                        message: msg.clone(),
                        response: media_error(StatusCode::BAD_REQUEST, msg),
                    };
                };

                if token.is_none() && !allow_noauth {
                    let msg = format!("No credentials for image provider: {provider}");
                    return AttemptOutcome::Failure {
                        status: StatusCode::UNAUTHORIZED,
                        message: msg.clone(),
                        response: media_error(StatusCode::UNAUTHORIZED, msg),
                    };
                }

                let final_url = if (provider == "gemini" || provider == "nanobanana")
                    && !target_url.contains("key=")
                {
                    format!("{target_url}?key={}", token.unwrap_or_default())
                } else {
                    target_url
                };

                let mut req = http_client().post(final_url).json_body(req_body);
                if let Some(t) = token {
                    req = match provider.as_str() {
                        "gemini" | "nanobanana" => req,
                        "black-forest-labs" => req.header("x-key", t),
                        "fal-ai" => req.header("Authorization", format!("Key {t}")),
                        _ => req.bearer_auth(t),
                    };
                }

                match req.send().await {
                    Ok(res) if res.status().is_success() => {
                        let ct = res
                            .headers()
                            .get("content-type")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("")
                            .to_owned();
                        if ct.starts_with("image/") {
                            let bytes = res.bytes().await.unwrap_or_default();
                            crate::services::modality_usage::record_modality_usage(
                                state,
                                provider,
                                model,
                                creds.as_ref().map(|c| c.connection_id.as_str()),
                                crate::server::api_key_id(state, headers).as_deref(),
                                "/v1/images/generations",
                                0,
                                0,
                                "request-only",
                            );
                            let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                            let created = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_secs())
                                .unwrap_or(0);
                            return AttemptOutcome::Success(
                                (
                                    StatusCode::OK,
                                    [("Access-Control-Allow-Origin", "*")],
                                    Json(json!({ "created": created, "data": [{ "b64_json": b64 }] })),
                                )
                                    .into_response(),
                            );
                        }
                        match read_json(res).await {
                            Ok(data) => {
                                let (prompt_tokens, completion_tokens) =
                                    crate::services::modality_usage::reported_tokens(&data["usage"]);
                                crate::services::modality_usage::record_modality_usage(
                                    state,
                                    provider,
                                    model,
                                    creds.as_ref().map(|c| c.connection_id.as_str()),
                                    crate::server::api_key_id(state, headers).as_deref(),
                                    "/v1/images/generations",
                                    prompt_tokens,
                                    completion_tokens,
                                    if data["usage"].is_object() {
                                        "reported"
                                    } else {
                                        "request-only"
                                    },
                                );
                                let normalized = normalize_openai_shape(data, prompt);
                                AttemptOutcome::Success(
                                    (
                                        StatusCode::OK,
                                        [("Access-Control-Allow-Origin", "*")],
                                        Json(normalized),
                                    )
                                        .into_response(),
                                )
                            }
                            Err(e) => AttemptOutcome::Failure {
                                status: StatusCode::BAD_GATEWAY,
                                message: e.clone(),
                                response: media_error(StatusCode::BAD_GATEWAY, e),
                            },
                        }
                    }
                    Ok(res) => {
                        let st = StatusCode::from_u16(res.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
                        let msg = res.text().await.unwrap_or_default();
                        AttemptOutcome::Failure {
                            status: st,
                            message: msg.clone(),
                            response: media_error(st, msg),
                        }
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        AttemptOutcome::Failure {
                            status: StatusCode::BAD_GATEWAY,
                            message: msg.clone(),
                            response: media_error(StatusCode::BAD_GATEWAY, msg),
                        }
                    }
                }
            }
        },
    )
    .await
}
