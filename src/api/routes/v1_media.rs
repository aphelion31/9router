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

use crate::handlers::media::common::{AttemptOutcome, for_each_account};

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

async fn forward_json_attempt(
    state: &AppState,
    method: reqwest::Method,
    url: &str,
    token: &str,
    body: Option<&Value>,
) -> AttemptOutcome {
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
            let mut out = Response::new(Body::from(bytes.clone()));
            *out.status_mut() = status;
            if let Ok(hv) = HeaderValue::from_str(&ct) {
                out.headers_mut().insert(header::CONTENT_TYPE, hv);
            }
            if status.is_success() {
                AttemptOutcome::Success(out)
            } else {
                AttemptOutcome::Failure {
                    status,
                    message: String::from_utf8_lossy(&bytes).into_owned(),
                    response: out,
                }
            }
        }
        Err(e) => {
            let msg = e.to_string();
            AttemptOutcome::Failure {
                status: StatusCode::BAD_GATEWAY,
                message: msg.clone(),
                response: err(StatusCode::BAD_GATEWAY, &msg, "upstream_error"),
            }
        }
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

async fn web_search(State(s): State<AppState>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
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
        .map(|p| p.id.clone())
        .unwrap_or_else(|| provider_input.to_owned());

    for_each_account(
        &s,
        &provider,
        "",
        false,
        || {
            err(
                StatusCode::SERVICE_UNAVAILABLE,
                &format!("No active credentials for search provider: {provider}"),
                "unavailable_error",
            )
        },
        |creds| {
            let s = &s;
            let headers = &headers;
            let body = &body;
            let provider = &provider;
            async move {
                let Some(creds) = creds else {
                    let msg = format!("No active credentials for search provider: {provider}");
                    return AttemptOutcome::Failure {
                        status: StatusCode::SERVICE_UNAVAILABLE,
                        message: msg.clone(),
                        response: err(StatusCode::SERVICE_UNAVAILABLE, &msg, "unavailable_error"),
                    };
                };
                let token = creds
                    .api_key
                    .as_deref()
                    .or(creds.access_token.as_deref())
                    .unwrap_or("");
                if token.is_empty() {
                    let msg = format!("No active credentials for search provider: {provider}");
                    return AttemptOutcome::Failure {
                        status: StatusCode::UNAUTHORIZED,
                        message: msg.clone(),
                        response: err(StatusCode::UNAUTHORIZED, &msg, "unavailable_error"),
                    };
                }
                let base_override = creds
                    .provider_specific_data
                    .get("baseUrl")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let url = match provider.as_str() {
                    "tavily" => "https://api.tavily.com/search".to_string(),
                    "exa" => "https://api.exa.ai/search".to_string(),
                    "serper" => "https://google.serper.dev/search".to_string(),
                    _ => format!("{}/search", provider_base_url(provider, base_override)),
                };
                let outcome = forward_json_attempt(s, reqwest::Method::POST, &url, token, Some(body)).await;
                if matches!(outcome, AttemptOutcome::Success(_)) {
                    let cost = match provider.as_str() {
                        "exa" => Some(0.007),
                        "serper" => Some(0.001),
                        "tavily" => Some(0.008),
                        "brave-search" | "google-pse" | "linkup" | "youcom" => Some(0.005),
                        "searxng" | "tinyfish" => Some(0.0),
                        _ => None,
                    };
                    crate::services::modality_usage::record_search_usage(
                        s,
                        provider,
                        Some(&creds.connection_id),
                        crate::server::api_key_id(s, headers).as_deref(),
                        cost,
                    );
                }
                outcome
            }
        },
    )
    .await
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
            .map(|p| p.id.clone())
            .unwrap_or_else(|| provider_input.to_owned());
        return for_each_account(
            &s,
            &provider,
            "",
            true,
            || err(StatusCode::SERVICE_UNAVAILABLE, "No credentials", "unavailable_error"),
            |creds| {
                let s = &s;
                let body = &body;
                let provider = &provider;
                async move {
                    if let Some(creds) = creds {
                        let token = creds
                            .api_key
                            .as_deref()
                            .or(creds.access_token.as_deref())
                            .unwrap_or("");
                        if !token.is_empty() {
                            let base_override = creds
                                .provider_specific_data
                                .get("baseUrl")
                                .and_then(Value::as_str)
                                .map(str::to_string);
                            let url = format!("{}/extract", provider_base_url(provider, base_override));
                            return forward_json_attempt(s, reqwest::Method::POST, &url, token, Some(body))
                                .await;
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
                            AttemptOutcome::Success(
                                Json(json!({
                                    "url": target,
                                    "status": status,
                                    "contentType": ct,
                                    "content": text,
                                }))
                                .into_response(),
                            )
                        }
                        Err(e) => {
                            let msg = e.to_string();
                            AttemptOutcome::Failure {
                                status: StatusCode::BAD_GATEWAY,
                                message: msg.clone(),
                                response: err(StatusCode::BAD_GATEWAY, &msg, "upstream_error"),
                            }
                        }
                    }
                }
            },
        )
        .await;
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
        response::IntoResponse,
    };
    use serde_json::json;

    #[tokio::test]
    async fn embeddings_records_reported_usage() {
        let (app, db) = app();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0; 8192];
            let _ = stream.read(&mut buf).await;
            let body = r#"{"object":"list","data":[],"usage":{"prompt_tokens":9,"total_tokens":9}}"#;
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).as_bytes()).await.unwrap();
        });
        {
            let conn = db.lock().unwrap();
            let data = json!({"apiKey":"test","providerSpecificData":{"baseUrl":format!("http://{addr}")}}).to_string();
            conn.execute("INSERT INTO providerConnections(id,provider,authType,name,isActive,priority,data,createdAt,updatedAt) VALUES('embed-test','openai','apikey','test',1,1,?1,'now','now')", [data]).unwrap();
        }
        let (status, _) = call(app, Request::post("/v1/embeddings").header("content-type", "application/json").body(Body::from(r#"{"model":"openai/test","input":"hello"}"#)).unwrap()).await;
        assert_eq!(status, StatusCode::OK);
        let conn = db.lock().unwrap();
        let (tokens, meta): (i64, String) = conn.query_row("SELECT promptTokens,meta FROM usageHistory WHERE connectionId='embed-test' AND endpoint='/v1/embeddings'", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(tokens, 9);
        assert_eq!(serde_json::from_str::<serde_json::Value>(&meta).unwrap()["accrual"], "reported");
    }

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

    async fn spawn_mock_media_server(
        path: &'static str,
        fail_status: StatusCode,
        ok_body: serde_json::Value,
    ) -> std::net::SocketAddr {
        use axum::http::HeaderMap as ReqHeaders;
        let router = axum::Router::new().route(
            path,
            axum::routing::post(move |headers: ReqHeaders| {
                let ok_body = ok_body.clone();
                async move {
                    let auth = headers
                        .get(axum::http::header::AUTHORIZATION)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("");
                    if auth == "Bearer key-1" {
                        (
                            fail_status,
                            axum::Json(json!({ "error": { "message": "account 1 failed" } })),
                        )
                            .into_response()
                    } else {
                        (StatusCode::OK, axum::Json(ok_body)).into_response()
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        addr
    }

    fn insert_conn(
        db: &std::sync::Arc<std::sync::Mutex<rusqlite::Connection>>,
        id: &str,
        provider: &str,
        priority: i64,
        api_key: &str,
        base_url: &str,
    ) {
        let conn = db.lock().unwrap();
        let data = json!({
            "apiKey": api_key,
            "providerSpecificData": { "baseUrl": base_url }
        })
        .to_string();
        conn.execute(
            "INSERT INTO providerConnections(id,provider,authType,name,isActive,priority,data,createdAt,updatedAt) VALUES(?1,?2,'apikey',?1,1,?3,?4,'2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z')",
            rusqlite::params![id, provider, priority, data],
        )
        .unwrap();
    }

    #[tokio::test]
    async fn modality_account_fallback_and_single_failure() {
        // 1. Embeddings: 2 accounts (429 -> 200) and single failing account (429 -> 429)
        let addr = spawn_mock_media_server(
            "/embeddings",
            StatusCode::TOO_MANY_REQUESTS,
            json!({ "object": "list", "data": [{ "embedding": [0.42] }] }),
        )
        .await;
        let base = format!("http://{addr}");

        let (app1, db1) = app();
        insert_conn(&db1, "emb-1", "openai", 1, "key-1", &base);
        let (st, _) = call(
            app1,
            Request::post("/v1/embeddings")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"openai/text-embedding-3-small","input":"hi"}"#))
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);

        let (app2, db2) = app();
        insert_conn(&db2, "emb-1", "openai", 1, "key-1", &base);
        insert_conn(&db2, "emb-2", "openai", 2, "key-2", &base);
        let (st, body) = call(
            app2,
            Request::post("/v1/embeddings")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"openai/text-embedding-3-small","input":"hi"}"#))
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(body["data"][0]["embedding"][0], 0.42);

        // 2. Images: 2 accounts (401 -> 200)
        let addr = spawn_mock_media_server(
            "/v1/images/generations",
            StatusCode::UNAUTHORIZED,
            json!({ "created": 1, "data": [{ "url": "https://img.example/ok.png" }] }),
        )
        .await;
        let (app_img, db_img) = app();
        insert_conn(&db_img, "img-1", "openai", 1, "key-1", &format!("http://{addr}"));
        insert_conn(&db_img, "img-2", "openai", 2, "key-2", &format!("http://{addr}"));
        let (st, body) = call(
            app_img,
            Request::post("/v1/images/generations")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"openai/dall-e-3","prompt":"a cat"}"#))
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(body["data"][0]["url"], "https://img.example/ok.png");

        // 3. STT: 2 accounts (429 -> 200)
        let addr = spawn_mock_media_server(
            "/",
            StatusCode::TOO_MANY_REQUESTS,
            json!({ "text": "hello from account 2" }),
        )
        .await;
        let (app_stt, db_stt) = app();
        insert_conn(&db_stt, "stt-1", "openai", 1, "key-1", &format!("http://{addr}"));
        insert_conn(&db_stt, "stt-2", "openai", 2, "key-2", &format!("http://{addr}"));
        let boundary = "----testboundary";
        let multipart_body = format!(
            "--{b}\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\nopenai/whisper-1\r\n--{b}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.wav\"\r\nContent-Type: audio/wav\r\n\r\nRIFF\r\n--{b}--\r\n",
            b = boundary
        );
        let (st, body) = call(
            app_stt,
            Request::post("/v1/audio/transcriptions")
                .header("content-type", format!("multipart/form-data; boundary={boundary}"))
                .body(Body::from(multipart_body))
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(body["text"], "hello from account 2");

        // 4. TTS: 2 accounts (429 -> 200)
        let addr = spawn_mock_media_server(
            "/v1/audio/speech",
            StatusCode::TOO_MANY_REQUESTS,
            json!({ "ok": true }),
        )
        .await;
        let (app_tts, db_tts) = app();
        insert_conn(&db_tts, "tts-1", "openai", 1, "key-1", &format!("http://{addr}"));
        insert_conn(&db_tts, "tts-2", "openai", 2, "key-2", &format!("http://{addr}"));
        let (st, _) = call(
            app_tts,
            Request::post("/v1/audio/speech")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"openai/tts-1","input":"hi","response_format":"json"}"#))
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);

        // 5. Videos: 2 accounts (429 -> 200)
        let addr = spawn_mock_media_server(
            "/generations",
            StatusCode::TOO_MANY_REQUESTS,
            json!({ "id": "vid-2", "status": "queued" }),
        )
        .await;
        let (app_vid, db_vid) = app();
        insert_conn(&db_vid, "vid-1", "xai", 1, "key-1", &format!("http://{addr}"));
        insert_conn(&db_vid, "vid-2", "xai", 2, "key-2", &format!("http://{addr}"));
        let (st, body) = call(
            app_vid,
            Request::post("/v1/videos/generations")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"xai/grok-imagine-video","prompt":"sunset"}"#))
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(body["id"], "vid-2");

        // 6. SystemOne: 2 accounts (429 -> 200)
        let addr = spawn_mock_media_server(
            "/v1/systemone",
            StatusCode::TOO_MANY_REQUESTS,
            json!({ "answers": ["ok2"] }),
        )
        .await;
        let (app_sys, db_sys) = app();
        insert_conn(&db_sys, "sys-1", "openai", 1, "key-1", &format!("http://{addr}"));
        insert_conn(&db_sys, "sys-2", "openai", 2, "key-2", &format!("http://{addr}"));
        let (st, body) = call(
            app_sys,
            Request::post("/v1/systemone")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"openai/systemone","state":{},"questions":{"q1":{}}}"#))
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(body["answers"][0], "ok2");

        // 7. Search: 2 accounts (429 -> 200)
        let addr = spawn_mock_media_server(
            "/search",
            StatusCode::TOO_MANY_REQUESTS,
            json!({ "results": [{ "title": "search-ok-2" }] }),
        )
        .await;
        let (app_srch, db_srch) = app();
        insert_conn(&db_srch, "srch-1", "brave-search", 1, "key-1", &format!("http://{addr}"));
        insert_conn(&db_srch, "srch-2", "brave-search", 2, "key-2", &format!("http://{addr}"));
        let (st, body) = call(
            app_srch,
            Request::post("/v1/search")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"provider":"brave-search","query":"rust"}"#))
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(body["results"][0]["title"], "search-ok-2");

        // 8. Fetch: 2 accounts (429 -> 200)
        let addr = spawn_mock_media_server(
            "/extract",
            StatusCode::TOO_MANY_REQUESTS,
            json!({ "content": "fetch-ok-2" }),
        )
        .await;
        let (app_fch, db_fch) = app();
        insert_conn(&db_fch, "fch-1", "tavily", 1, "key-1", &format!("http://{addr}"));
        insert_conn(&db_fch, "fch-2", "tavily", 2, "key-2", &format!("http://{addr}"));
        let (st, body) = call(
            app_fch,
            Request::post("/v1/web/fetch")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"provider":"tavily","url":"https://1.1.1.1/"}"#))
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(body["content"], "fetch-ok-2");
    }
}
