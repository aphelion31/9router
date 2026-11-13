//! Ported `/v1/*` media, embeddings, search, fetch, systemone, ollama-chat, and compact routes.
use crate::providers;
use crate::server::AppState;
use axum::{
    Json, Router,
    body::Body,
    extract::{OriginalUri, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};
use std::collections::HashMap;

pub fn router() -> Router<AppState> {
    use crate::handlers::media;
    // Media endpoints use the full ports in handlers::media (provider routing, multipart, TTS voices).
    Router::new()
        .route("/v1/embeddings", post(media::embeddings::create_embeddings))
        .route("/v1/images/generations", post(media::images::image_generations))
        .route("/v1/videos/generations", post(media::videos::video_generations))
        .route("/v1/videos/edits", post(media::videos::video_edits))
        .route("/v1/videos/extensions", post(media::videos::video_extensions))
        .route("/v1/videos/{id}", get(media::videos::video_status))
        .route("/v1/audio/speech", post(media::tts::audio_speech))
        .route("/v1/audio/transcriptions", post(media::stt::audio_transcriptions))
        .route("/v1/audio/voices", get(audio_voices))
        .route("/v1/search", post(web_search))
        .route("/v1/web/fetch", post(web_fetch))
        .route("/v1/systemone", post(media::systemone::handle_systemone))
        .route("/v1/api/chat", post(ollama_chat))
        .route("/v1/responses/compact", post(responses_compact))
}

fn err(status: StatusCode, msg: &str, typ: &str) -> Response {
    (status, Json(json!({ "error": { "message": msg, "type": typ } }))).into_response()
}

fn active_credential(state: &AppState, provider: &str) -> Option<(String, Option<String>)> {
    let db = state.db.lock().ok()?;
    let raw: String = db
        .query_row(
            "SELECT data FROM providerConnections WHERE provider=?1 AND isActive=1 ORDER BY priority ASC LIMIT 1",
            [provider],
            |r| r.get(0),
        )
        .ok()?;
    let data: Value = crate::crypto::parse_data(&raw)?;
    let token = data["apiKey"]
        .as_str()
        .or_else(|| data["accessToken"].as_str())
        .filter(|s| !s.is_empty())?
        .to_string();
    let base_url = data["providerSpecificData"]["baseUrl"]
        .as_str()
        .or_else(|| data["baseUrl"].as_str())
        .map(str::to_string);
    Some((token, base_url))
}

fn provider_base_url(provider: &str, override_url: Option<String>) -> String {
    if let Some(u) = override_url.filter(|s| !s.is_empty()) {
        return u.trim_end_matches('/').to_string();
    }
    providers::lookup(provider)
        .and_then(|p| p.base_url.as_deref())
        .unwrap_or("https://api.openai.com/v1")
        .trim_end_matches('/')
        .to_string()
}

async fn forward_json(
    state: &AppState,
    method: reqwest::Method,
    url: &str,
    token: &str,
    body: Option<Value>,
) -> Response {
    let mut req = state
        .executor
        .0
        .request(method, url)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(b) = body {
        req = req.body(b.to_string());
    }
    match req.send().await {
        Ok(resp) => {
            let status = StatusCode::from_u16(resp.status().as_u16())
                .unwrap_or(StatusCode::BAD_GATEWAY);
            let ct = resp
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("application/json")
                .to_string();
            let bytes = resp.bytes().await.unwrap_or_default();
            let mut out = Response::new(Body::from(bytes));
            *out.status_mut() = status;
            if let Ok(hv) = HeaderValue::from_str(&ct) {
                out.headers_mut().insert(header::CONTENT_TYPE, hv);
            }
            out
        }
        Err(e) => err(StatusCode::BAD_GATEWAY, &e.to_string(), "upstream_error"),
    }
}

async fn audio_voices(Query(q): Query<HashMap<String, String>>) -> Response {
    let allowed = ["elevenlabs", "deepgram", "inworld", "edge-tts", "local-device"];
    let Some(provider) = q.get("provider").map(String::as_str) else {
        return err(
            StatusCode::BAD_REQUEST,
            &format!("provider must be one of: {}", allowed.join(", ")),
            "invalid_request_error",
        );
    };
    if !allowed.contains(&provider) {
        return err(
            StatusCode::BAD_REQUEST,
            &format!("provider must be one of: {}", allowed.join(", ")),
            "invalid_request_error",
        );
    }
    let alias = providers::lookup(provider)
        .filter(|p| !p.alias.is_empty())
        .map(|p| p.alias.clone())
        .unwrap_or_else(|| provider.to_string());
    let voices: Vec<Value> = match provider {
        "deepgram" => vec!["aura-asteria-en", "aura-luna-en", "aura-orion-en"],
        "elevenlabs" => vec!["21m00Tcm4TlvDq8ikWAM", "AZnzlk1XvdvUeBnXmlld"],
        "inworld" => vec!["Ashley", "Dennis", "Alex"],
        _ => vec!["en-US-AriaNeural", "en-US-GuyNeural"],
    }
    .into_iter()
    .map(|v| {
        json!({
            "id": format!("{alias}/{v}"),
            "object": "voice",
            "name": v,
            "provider": provider,
        })
    })
    .collect();
    Json(json!({ "object": "list", "data": voices })).into_response()
}

async fn web_search(State(s): State<AppState>, Json(body): Json<Value>) -> Response {
    let provider_input = body["provider"]
        .as_str()
        .or_else(|| body["model"].as_str())
        .unwrap_or("");
    let query = body["query"].as_str().unwrap_or("");
    if provider_input.is_empty() {
        return err(StatusCode::BAD_REQUEST, "Missing provider", "invalid_request_error");
    }
    if query.is_empty() {
        return err(StatusCode::BAD_REQUEST, "Missing query", "invalid_request_error");
    }
    let provider = providers::lookup(provider_input)
        .map(|p| p.id.as_str())
        .unwrap_or(provider_input);
    let Some((token, base_override)) = active_credential(&s, provider) else {
        return err(
            StatusCode::SERVICE_UNAVAILABLE,
            &format!("No active credentials for search provider: {provider}"),
            "unavailable_error",
        );
    };
    let url = match provider {
        "tavily" => "https://api.tavily.com/search".to_string(),
        "exa" => "https://api.exa.ai/search".to_string(),
        "serper" => "https://google.serper.dev/search".to_string(),
        _ => format!("{}/search", provider_base_url(provider, base_override)),
    };
    forward_json(&s, reqwest::Method::POST, &url, &token, Some(body)).await
}

async fn web_fetch(State(s): State<AppState>, Json(body): Json<Value>) -> Response {
    let Some(target) = body["url"].as_str().filter(|u| !u.is_empty()) else {
        return err(StatusCode::BAD_REQUEST, "Missing required field: url", "invalid_request_error");
    };
    if !crate::ssrf::url_is_public(target).await {
        return err(StatusCode::BAD_REQUEST, "URL is invalid or blocked", "invalid_request_error");
    }
    if let Some(provider_input) = body["provider"]
        .as_str()
        .or_else(|| body["model"].as_str())
        .filter(|p| !p.is_empty())
    {
        let provider = providers::lookup(provider_input)
            .map(|p| p.id.as_str())
            .unwrap_or(provider_input);
        if let Some((token, base_override)) = active_credential(&s, provider) {
            let url = format!("{}/extract", provider_base_url(provider, base_override));
            return forward_json(&s, reqwest::Method::POST, &url, &token, Some(body)).await;
        }
    }
    match s.executor.0.get(target).send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let ct = resp
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("text/plain")
                .to_string();
            let text = resp.text().await.unwrap_or_default();
            Json(json!({
                "url": target,
                "status": status,
                "contentType": ct,
                "content": text,
            }))
            .into_response()
        }
        Err(e) => err(StatusCode::BAD_GATEWAY, &e.to_string(), "upstream_error"),
    }
}

async fn ollama_chat(
    State(s): State<AppState>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    Json(mut body): Json<Value>,
) -> Response {
    let model_name = body["model"]
        .as_str()
        .filter(|m| !m.is_empty())
        .unwrap_or("llama3.2")
        .to_string();
    body["stream"] = json!(false);
    let resp = crate::server::completion(State(s), OriginalUri(uri), headers, Json(body)).await;
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap_or_default();
    if !status.is_success() {
        return (status, bytes).into_response();
    }
    let Ok(val) = serde_json::from_slice::<Value>(&bytes) else {
        return (status, bytes).into_response();
    };
    let msg = &val["choices"][0]["message"];
    if msg.is_object() {
        return Json(json!({
            "model": model_name,
            "message": {
                "role": msg["role"].as_str().unwrap_or("assistant"),
                "content": msg["content"].as_str().unwrap_or(""),
            },
            "done": true,
            "done_reason": "stop",
            "prompt_eval_count": val["usage"]["prompt_tokens"].as_u64().unwrap_or(0),
            "eval_count": val["usage"]["completion_tokens"].as_u64().unwrap_or(0),
        }))
        .into_response();
    }
    Json(val).into_response()
}

async fn responses_compact(
    State(s): State<AppState>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    Json(mut body): Json<Value>,
) -> Response {
    body["_compact"] = json!(true);
    crate::server::completion(State(s), OriginalUri(uri), headers, Json(body)).await
}

#[cfg(test)]
mod tests {
    use crate::api::routes::test_support::{app, call};
    use axum::{
        body::Body,
        http::{Method, Request, StatusCode},
    };
    use serde_json::json;

    #[tokio::test]
    async fn v1_media_routes_validate_and_dispatch() {
        let (app, db) = app();

        // /v1/embeddings missing input -> 400
        let (st, _) = call(
            app.clone(),
            Request::builder()
                .method(Method::POST)
                .uri("/v1/embeddings")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"openai/text-embedding-3-small"}"#))
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);

        // /v1/embeddings without provider credentials -> 400 (upstream memberFailover)
        let (st, _) = call(
            app.clone(),
            Request::builder()
                .method(Method::POST)
                .uri("/v1/embeddings")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"model":"openai/text-embedding-3-small","input":"hello"}"#,
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);

        // /v1/audio/voices valid provider -> 200 list
        let (st, voices) = call(
            app.clone(),
            Request::get("/v1/audio/voices?provider=deepgram")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert!(voices["data"].as_array().is_some_and(|a| !a.is_empty()));

        // /v1/web/fetch blocks localhost SSRF -> 400
        let (st, _) = call(
            app.clone(),
            Request::builder()
                .method(Method::POST)
                .uri("/v1/web/fetch")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"url":"http://127.0.0.1:8080/secret"}"#))
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);

        // /v1/search missing query -> 400
        let (st, _) = call(
            app.clone(),
            Request::builder()
                .method(Method::POST)
                .uri("/v1/search")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"provider":"tavily"}"#))
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);

        // /v1/systemone missing messages/candidates -> 400
        let (st, _) = call(
            app.clone(),
            Request::builder()
                .method(Method::POST)
                .uri("/v1/systemone")
                .header("content-type", "application/json")
                .body(Body::from(r#"{}"#))
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);

        // Mock upstream test for /v1/embeddings
        let mock = axum::Router::new().route(
            "/embeddings",
            axum::routing::post(|| async {
                axum::Json(json!({ "object": "list", "data": [{ "embedding": [0.1, 0.2] }] }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, mock).await.unwrap();
        });
        {
            let conn = db.lock().unwrap();
            let data = json!({
                "apiKey": "sk-upstream",
                "providerSpecificData": { "baseUrl": format!("http://{addr}") }
            });
            conn.execute(
                "INSERT INTO providerConnections(id, provider, authType, name, isActive, priority, createdAt, updatedAt, data) VALUES('c1','openai','apikey','mock',1,1,'2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z',?1)",
                [data.to_string()],
            )
            .unwrap();
        }
        let (st, emb) = call(
            app,
            Request::builder()
                .method(Method::POST)
                .uri("/v1/embeddings")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"model":"openai/text-embedding-3-small","input":"hi"}"#,
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(emb["data"][0]["embedding"][0], 0.1);
    }
}
