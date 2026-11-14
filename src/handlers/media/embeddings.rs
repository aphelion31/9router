// Port of embeddingsCore.js + embeddingProviders/*

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};

use crate::handlers::Credentials;
use crate::handlers::media::common::{
    RequestJsonExt, http_client, media_error, read_json, resolve_credentials, split_provider_model,
};
use crate::server::AppState;

fn default_embedding_url(provider: &str) -> Option<&'static str> {
    match provider {
        "openai" => Some("https://api.openai.com/v1/embeddings"),
        "openrouter" => Some("https://openrouter.ai/api/v1/embeddings"),
        "groq" => Some("https://api.groq.com/openai/v1/embeddings"),
        "together" => Some("https://api.together.xyz/v1/embeddings"),
        "fireworks" => Some("https://api.fireworks.ai/inference/v1/embeddings"),
        "deepinfra" => Some("https://api.deepinfra.com/v1/openai/embeddings"),
        "nvidia" => Some("https://integrate.api.nvidia.com/v1/embeddings"),
        "mistral" => Some("https://api.mistral.ai/v1/embeddings"),
        "voyage" => Some("https://api.voyageai.com/v1/embeddings"),
        "jina" => Some("https://api.jina.ai/v1/embeddings"),
        "cohere" => Some("https://api.cohere.com/v2/embed"),
        "nebius" => Some("https://api.studio.nebius.ai/v1/embeddings"),
        "qwen" => Some("https://dashscope.aliyuncs.com/compatible-mode/v1/embeddings"),
        "github" => Some("https://models.inference.ai.azure.com/embeddings"),
        "ollama-local" => Some("http://localhost:11434/v1/embeddings"),
        "selfhosted-embedding" => Some(""),
        _ => None,
    }
}

pub async fn create_embeddings(State(state): State<AppState>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    let input = &body["input"];
    if input.is_null() || (input.as_str().is_some_and(str::is_empty)) {
        return media_error(StatusCode::BAD_REQUEST, "Missing required field: input");
    }
    if !input.is_string() && !input.is_array() {
        return media_error(StatusCode::BAD_REQUEST, "input must be a string or array of strings");
    }

    let raw_model = body["model"].as_str().unwrap_or("openai/text-embedding-3-small");
    let (provider, model) = split_provider_model(raw_model, "openai");
    let creds: Option<Credentials> = resolve_credentials(&state, &provider);
    let override_url = creds
        .as_ref()
        .and_then(|c| c.provider_specific_data.get("baseUrl").and_then(Value::as_str))
        .map(|s| s.trim_end_matches('/').to_owned());

    let token = creds
        .as_ref()
        .and_then(|c| c.api_key.as_deref().or(c.access_token.as_deref()));

    if provider == "gemini" {
        let Some(key) = token else {
            return media_error(StatusCode::UNAUTHORIZED, "No Gemini credentials configured");
        };
        let m_path = if model.starts_with("models/") {
            model.clone()
        } else {
            format!("models/{model}")
        };
        let is_batch = input.is_array();
        let op = if is_batch { "batchEmbedContents" } else { "embedContent" };
        let base = override_url
            .as_deref()
            .unwrap_or("https://generativelanguage.googleapis.com/v1beta");
        let url = format!("{base}/{m_path}:{op}?key={key}");
        let dim = body["dimensions"].as_u64().filter(|&d| d > 0);
        let req_body = if let Some(arr) = input.as_array() {
            let reqs: Vec<Value> = arr
                .iter()
                .map(|t| {
                    let mut o = json!({
                        "model": m_path,
                        "content": { "parts": [{ "text": t.as_str().unwrap_or(&t.to_string()) }] }
                    });
                    if let Some(d) = dim {
                        o["outputDimensionality"] = json!(d);
                    }
                    o
                })
                .collect();
            json!({ "requests": reqs })
        } else {
            let mut o = json!({
                "model": m_path,
                "content": { "parts": [{ "text": input.as_str().unwrap_or("") }] }
            });
            if let Some(d) = dim {
                o["outputDimensionality"] = json!(d);
            }
            o
        };
        match http_client().post(url).json_body(&req_body).send().await {
            Ok(res) if res.status().is_success() => {
                let data = read_json(res).await.unwrap_or(Value::Null);
                let (prompt, completion) = crate::services::modality_usage::reported_tokens(&data["usage"]);
                crate::services::modality_usage::record_modality_usage(&state, &provider, &model, creds.as_ref().map(|c| c.connection_id.as_str()), crate::server::api_key_id(&state, &headers).as_deref(), "/v1/embeddings", prompt, completion, "reported");
                if data["object"] == "list" && data["data"].is_array() {
                    return (StatusCode::OK, Json(data)).into_response();
                }
                let items: Vec<Value> = if let Some(embs) = data["embeddings"].as_array() {
                    embs.iter()
                        .enumerate()
                        .map(|(i, e)| json!({ "object": "embedding", "index": i, "embedding": e["values"] }))
                        .collect()
                } else if data["embedding"]["values"].is_array() {
                    vec![json!({ "object": "embedding", "index": 0, "embedding": data["embedding"]["values"] })]
                } else {
                    vec![]
                };
                return (
                    StatusCode::OK,
                    Json(json!({
                        "object": "list",
                        "data": items,
                        "model": model,
                        "usage": { "prompt_tokens": 0, "total_tokens": 0 }
                    })),
                )
                    .into_response();
            }
            Ok(res) => {
                let st = StatusCode::from_u16(res.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
                return media_error(st, res.text().await.unwrap_or_default());
            }
            Err(e) => return media_error(StatusCode::BAD_GATEWAY, e.to_string()),
        }
    }

    let Some(default_url) = default_embedding_url(&provider) else {
        return media_error(
            StatusCode::BAD_REQUEST,
            format!("Provider '{provider}' does not support embeddings."),
        );
    };
    let target_url = if let Some(ov) = override_url {
        // Upstream openaiCompatNode: trim trailing "/" and "/embeddings", then append "/embeddings".
        let base = ov.trim_end_matches('/');
        format!("{}/embeddings", base.strip_suffix("/embeddings").unwrap_or(base))
    } else if default_url.is_empty() {
        return media_error(
            StatusCode::BAD_REQUEST,
            format!("[{provider}/{model}] Missing providerSpecificData.baseUrl"),
        );
    } else {
        default_url.to_owned()
    };

    if token.is_none() && provider != "ollama-local" && provider != "selfhosted-embedding" {
        // Upstream memberFailover: no account at all -> 400 "No credentials for provider: X".
        return media_error(
            StatusCode::BAD_REQUEST,
            format!("No credentials for provider: {provider}"),
        );
    }

    let mut req_body = json!({
        "model": model,
        "input": input,
        "encoding_format": body["encoding_format"].as_str().unwrap_or("float"),
    });
    if let Some(dim) = body["dimensions"].as_u64() {
        req_body["dimensions"] = json!(dim);
    }

    let mut req = http_client().post(target_url).json_body(&req_body);
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }

    match req.send().await {
        Ok(res) if res.status().is_success() => match read_json(res).await {
            Ok(data) => {
                let (prompt, completion) = crate::services::modality_usage::reported_tokens(&data["usage"]);
                crate::services::modality_usage::record_modality_usage(&state, &provider, &model, creds.as_ref().map(|c| c.connection_id.as_str()), crate::server::api_key_id(&state, &headers).as_deref(), "/v1/embeddings", prompt, completion, "reported");
                (
                StatusCode::OK,
                [("Access-Control-Allow-Origin", "*")],
                Json(data),
            )
                .into_response()
            },
            Err(e) => media_error(StatusCode::BAD_GATEWAY, e),
        },
        Ok(res) => {
            let st = StatusCode::from_u16(res.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            media_error(st, res.text().await.unwrap_or_default())
        }
        Err(e) => media_error(StatusCode::BAD_GATEWAY, e.to_string()),
    }
}
