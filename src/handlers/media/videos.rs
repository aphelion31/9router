// Port of videoCore.js + videoProviders/{openrouter,vertex}.js

use axum::{
    Json,
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use base64::Engine as _;
use serde_json::{Value, json};

use crate::handlers::Credentials;
use crate::handlers::media::common::{
    AttemptOutcome, for_each_account, http_client, media_error, split_provider_model, url_encode,
};
use crate::server::AppState;

pub fn sanitize_secrets(text: &str, creds: Option<&Credentials>) -> String {
    let mut out = text.to_owned();
    if let Some(c) = creds {
        for s in [c.access_token.as_deref(), c.refresh_token.as_deref(), c.api_key.as_deref()]
            .into_iter()
            .flatten()
        {
            if s.len() >= 8 {
                out = out.replace(s, "[redacted]");
            }
        }
    }
    out
}

fn default_video_base(provider: &str) -> Option<&'static str> {
    match provider {
        "xai" => Some("https://api.x.ai/v1/videos"),
        "openrouter" => Some("https://openrouter.ai/api/v1/videos"),
        "vertex" => Some("https://{location}-aiplatform.googleapis.com/v1"),
        _ => None,
    }
}

fn decode_vertex_job_id(id: &str) -> Option<String> {
    if id.is_empty() || id.len() > 1024 {
        return None;
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(id)
        .ok()?;
    let decoded = String::from_utf8(bytes).ok()?;
    if base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(decoded.as_bytes()) != id {
        return None;
    }
    let parts: Vec<&str> = decoded.split('/').collect();
    if parts.len() == 10
        && parts[0] == "projects"
        && parts[2] == "locations"
        && parts[4] == "publishers"
        && parts[6] == "models"
        && parts[8] == "operations"
        && parts.iter().all(|p| !p.is_empty() && *p != ".." && *p != ".")
    {
        Some(decoded)
    } else {
        None
    }
}

async fn handle_video_inner(
    state: AppState,
    action: Option<&str>,
    request_id: Option<&str>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let parsed_json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let raw_model = parsed_json["model"]
        .as_str()
        .or_else(|| headers.get("x-provider").and_then(|v| v.to_str().ok()))
        .unwrap_or("xai/grok-imagine-video");
    let (provider, model) = split_provider_model(raw_model, "xai");

    let Some(default_base) = default_video_base(&provider) else {
        return media_error(
            StatusCode::BAD_REQUEST,
            format!("Provider '{provider}' does not support video generation"),
        );
    };

    for_each_account(
        &state,
        &provider,
        &model,
        false,
        || {
            media_error(
                StatusCode::UNAUTHORIZED,
                format!("No credentials for video provider: {provider}"),
            )
        },
        |creds| {
            let state = &state;
            let headers = &headers;
            let body = &body;
            let parsed_json = &parsed_json;
            let provider = &provider;
            let model = &model;
            async move {
                let override_url = creds
                    .as_ref()
                    .and_then(|c| c.provider_specific_data.get("baseUrl").and_then(Value::as_str))
                    .map(|s| s.trim_end_matches('/').to_owned());

                let token = creds
                    .as_ref()
                    .and_then(|c| c.access_token.as_deref().or(c.api_key.as_deref()));

                if provider == "vertex" {
                    let Some(tok) = token else {
                        let msg = "Vertex video requires an OAuth access token".to_owned();
                        return AttemptOutcome::Failure {
                            status: StatusCode::UNAUTHORIZED,
                            message: msg.clone(),
                            response: media_error(StatusCode::UNAUTHORIZED, msg),
                        };
                    };
                    let location = creds
                        .as_ref()
                        .and_then(|c| c.provider_specific_data.get("location").and_then(Value::as_str))
                        .unwrap_or("us-central1");
                    let base = override_url
                        .unwrap_or_else(|| default_base.replace("{location}", location));

                    if let Some(job_id) = request_id {
                        let Some(op_name) = decode_vertex_job_id(job_id) else {
                            let msg = "Invalid Vertex video job id".to_owned();
                            return AttemptOutcome::Failure {
                                status: StatusCode::BAD_REQUEST,
                                message: msg.clone(),
                                response: media_error(StatusCode::BAD_REQUEST, msg),
                            };
                        };
                        let model_path = op_name.split("/operations/").next().unwrap_or("");
                        let url = format!("{base}/{model_path}:fetchPredictOperation");
                        let res = http_client()
                            .post(url)
                            .bearer_auth(tok)
                            .header("Content-Type", "application/json")
                            .body(serde_json::to_vec(&json!({ "operationName": op_name })).unwrap_or_default())
                            .send()
                            .await;
                        return match res {
                            Ok(r) if r.status().is_success() => {
                                let bytes = r.bytes().await.unwrap_or_default();
                                let data: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
                                if data["done"].as_bool() != Some(true) {
                                    return AttemptOutcome::Success(
                                        (
                                            StatusCode::OK,
                                            Json(json!({ "id": job_id, "status": "in_progress" })),
                                        )
                                            .into_response(),
                                    );
                                }
                                if !data["error"].is_null() {
                                    return AttemptOutcome::Success(
                                        (
                                            StatusCode::OK,
                                            Json(json!({ "id": job_id, "status": "failed", "error": data["error"] })),
                                        )
                                            .into_response(),
                                    );
                                }
                                let videos = data["response"]["videos"].as_array().cloned().unwrap_or_default();
                                let url_out = videos.first().and_then(|v| {
                                    v["gcsUri"].as_str().map(str::to_owned).or_else(|| {
                                        v["bytesBase64Encoded"].as_str().map(|b| {
                                            let mime = v["mimeType"].as_str().unwrap_or("video/mp4");
                                            format!("data:{mime};base64,{b}")
                                        })
                                    })
                                });
                                AttemptOutcome::Success(
                                    (
                                        StatusCode::OK,
                                        Json(json!({ "id": job_id, "status": "completed", "video": { "url": url_out } })),
                                    )
                                        .into_response(),
                                )
                            }
                            Ok(r) => {
                                let st = StatusCode::from_u16(r.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
                                let msg = sanitize_secrets(&r.text().await.unwrap_or_default(), creds.as_ref());
                                AttemptOutcome::Failure {
                                    status: st,
                                    message: msg.clone(),
                                    response: media_error(st, msg),
                                }
                            }
                            Err(e) => {
                                let msg = sanitize_secrets(&e.to_string(), creds.as_ref());
                                AttemptOutcome::Failure {
                                    status: StatusCode::BAD_GATEWAY,
                                    message: msg.clone(),
                                    response: media_error(StatusCode::BAD_GATEWAY, msg),
                                }
                            }
                        };
                    }

                    let Some(project_id) = creds
                        .as_ref()
                        .and_then(|c| c.provider_specific_data.get("projectId").and_then(Value::as_str))
                    else {
                        let msg = "Vertex video requires providerSpecificData.projectId".to_owned();
                        return AttemptOutcome::Failure {
                            status: StatusCode::BAD_REQUEST,
                            message: msg.clone(),
                            response: media_error(StatusCode::BAD_REQUEST, msg),
                        };
                    };
                    let url = format!(
                        "{base}/projects/{project_id}/locations/{location}/publishers/google/models/{model}:predictLongRunning"
                    );
                    let payload = json!({
                        "instances": [{ "prompt": parsed_json["prompt"] }],
                        "parameters": {}
                    });
                    return match http_client()
                        .post(url)
                        .bearer_auth(tok)
                        .header("Content-Type", "application/json")
                        .body(serde_json::to_vec(&payload).unwrap_or_default())
                        .send()
                        .await
                    {
                        Ok(r) if r.status().is_success() => {
                            let bytes = r.bytes().await.unwrap_or_default();
                            let data: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
                            let name = data["name"].as_str().unwrap_or("");
                            crate::services::modality_usage::record_modality_usage(
                                state,
                                provider,
                                model,
                                creds.as_ref().map(|c| c.connection_id.as_str()),
                                crate::server::api_key_id(state, headers).as_deref(),
                                "/v1/videos/generations",
                                0,
                                0,
                                "request-only",
                            );
                            let id = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(name.as_bytes());
                            AttemptOutcome::Success(
                                (StatusCode::OK, Json(json!({ "id": id, "status": "queued" }))).into_response(),
                            )
                        }
                        Ok(r) => {
                            let st = StatusCode::from_u16(r.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
                            let msg = sanitize_secrets(&r.text().await.unwrap_or_default(), creds.as_ref());
                            AttemptOutcome::Failure {
                                status: st,
                                message: msg.clone(),
                                response: media_error(st, msg),
                            }
                        }
                        Err(e) => {
                            let msg = sanitize_secrets(&e.to_string(), creds.as_ref());
                            AttemptOutcome::Failure {
                                status: StatusCode::BAD_GATEWAY,
                                message: msg.clone(),
                                response: media_error(StatusCode::BAD_GATEWAY, msg),
                            }
                        }
                    };
                }

                let base = override_url.as_deref().unwrap_or(default_base);
                let (method, url) = if let Some(id) = request_id {
                    (reqwest::Method::GET, format!("{base}/{}", url_encode(id)))
                } else if provider == "openrouter" {
                    if action != Some("generations") {
                        let msg = format!("OpenRouter video supports 'generations' only (got '{}')", action.unwrap_or(""));
                        return AttemptOutcome::Failure {
                            status: StatusCode::BAD_REQUEST,
                            message: msg.clone(),
                            response: media_error(StatusCode::BAD_REQUEST, msg),
                        };
                    }
                    (reqwest::Method::POST, base.to_owned())
                } else {
                    let act = action.unwrap_or("generations");
                    (reqwest::Method::POST, format!("{base}/{act}"))
                };

                let mut req = http_client().request(method.clone(), url).header("Accept", "application/json");
                if let Some(t) = token {
                    req = req.bearer_auth(t);
                }
                if method == reqwest::Method::POST {
                    if let Some(ct) = headers.get("content-type").and_then(|v| v.to_str().ok()) {
                        req = req.header("Content-Type", ct);
                    }
                    if let Some(idem) = headers.get("idempotency-key").and_then(|v| v.to_str().ok()) {
                        req = req.header("Idempotency-Key", idem);
                    }
                    req = req.body(body.clone());
                }

                match req.send().await {
                    Ok(res) => {
                        let st = StatusCode::from_u16(res.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
                        let rct = res
                            .headers()
                            .get("content-type")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("application/json")
                            .to_owned();
                        let txt = res.text().await.unwrap_or_default();
                        if !st.is_success() {
                            let msg = sanitize_secrets(&txt, creds.as_ref());
                            return AttemptOutcome::Failure {
                                status: st,
                                message: msg.clone(),
                                response: media_error(st, msg),
                            };
                        }
                        if request_id.is_none() {
                            crate::services::modality_usage::record_modality_usage(
                                state,
                                provider,
                                model,
                                creds.as_ref().map(|c| c.connection_id.as_str()),
                                crate::server::api_key_id(state, headers).as_deref(),
                                "/v1/videos/generations",
                                0,
                                0,
                                "request-only",
                            );
                        }
                        let mut h = HeaderMap::new();
                        h.insert("Content-Type", rct.parse().unwrap());
                        h.insert("Access-Control-Allow-Origin", "*".parse().unwrap());
                        AttemptOutcome::Success((st, h, txt).into_response())
                    }
                    Err(e) => {
                        let msg = sanitize_secrets(&e.to_string(), creds.as_ref());
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

pub async fn video_generations(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    handle_video_inner(state, Some("generations"), None, headers, body).await
}

pub async fn video_edits(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    handle_video_inner(state, Some("edits"), None, headers, body).await
}

pub async fn video_extensions(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    handle_video_inner(state, Some("extensions"), None, headers, body).await
}

pub async fn video_status(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    handle_video_inner(state, None, Some(&id), headers, Bytes::new()).await
}
