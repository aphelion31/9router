//! v1 routes whose upstream service is not ported yet. Each answers 501 `{error:"not available"}`
//! with the same path and method as the JS route so clients get a stable contract.
//! Replace each handler with a real port when its service lands in Rust.
use axum::{
    Router,
    routing::{get, post},
};

use crate::{api::routes::not_available, server::AppState};

// ponytail: stub. Upstream is 9router-pro sse/handlers/embeddings.js (needs embedding providers in Rust). Add when embeddings service is ported.
async fn stub() -> axum::response::Response {
    not_available()
}

pub fn router() -> Router<AppState> {
    Router::new()
        // embeddings: sse/handlers/embeddings.js
        .route("/v1/embeddings", post(stub))
        // image / video / audio: sse/handlers/{imageGeneration,videoGeneration,tts,stt}.js
        .route("/v1/images/generations", post(stub))
        .route("/v1/videos/generations", post(stub))
        .route("/v1/videos/edits", post(stub))
        .route("/v1/videos/extensions", post(stub))
        .route("/v1/videos/{id}", get(stub))
        // audio: ttsCore + sttCore
        .route("/v1/audio/speech", post(crate::handlers::media::tts::audio_speech))
        .route("/v1/audio/transcriptions", post(crate::handlers::media::stt::audio_transcriptions))
        .route("/v1/audio/voices", get(crate::handlers::media::tts::audio_voices))
        // web: sse/handlers/{search,fetch}.js
        .route("/v1/search", post(stub))
        .route("/v1/web/fetch", post(stub))
        // systemone: sse/handlers/systemone.js
        .route("/v1/systemone", post(stub))
        // api/chat: Ollama-shaped wrapper over handleChat + open-sse/utils/ollamaTransform.js
        .route("/v1/api/chat", post(stub))
        // responses/compact: needs codex executor `_compact` flag, not ported
        .route("/v1/responses/compact", post(stub))
}

#[cfg(test)]
mod tests {
    use crate::api::routes::test_support::{app, call};
    use axum::{body::Body, http::{Request, StatusCode}};

    #[tokio::test]
    async fn stubs_return_501_json() {
        let cases = [
            ("POST", "/v1/embeddings"),
            ("POST", "/v1/images/generations"),
            ("POST", "/v1/videos/generations"),
            ("POST", "/v1/videos/edits"),
            ("POST", "/v1/videos/extensions"),
            ("GET", "/v1/videos/abc"),
            ("POST", "/v1/search"),
            ("POST", "/v1/web/fetch"),
            ("POST", "/v1/systemone"),
            ("POST", "/v1/api/chat"),
            ("POST", "/v1/responses/compact"),
        ];
        for (method, path) in cases {
            let (router, _) = app();
            let req = Request::builder()
                .method(method)
                .uri(path)
                .header("authorization", "Bearer sk-test")
                .body(Body::empty())
                .unwrap();
            let (status, v) = call(router, req).await;
            assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{method} {path}");
            assert_eq!(v, serde_json::json!({"error":"not available"}), "{method} {path}");
        }
    }

    #[tokio::test]
    async fn audio_voices_returns_provider_voices() {
        let (router, _) = app();
        let req = Request::builder()
            .method("GET")
            .uri("/v1/audio/voices?provider=openai&model=tts-1")
            .header("authorization", "Bearer sk-test")
            .body(Body::empty())
            .unwrap();
        let (status, v) = call(router, req).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["provider"], "openai");
        assert_eq!(v["voices"].as_array().unwrap().len(), 9);
    }

    #[tokio::test]
    async fn audio_speech_and_transcriptions_mock_http() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            for _ in 0..2 {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = vec![0u8; 4096];
                let n = sock.read(&mut buf).await.unwrap();
                let req_str = String::from_utf8_lossy(&buf[..n]);
                if req_str.contains("/v1/audio/speech") {
                    let body = b"ID3MOCKAUDIO";
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nContent-Length: {}\r\n\r\n",
                        body.len()
                    );
                    sock.write_all(resp.as_bytes()).await.unwrap();
                    sock.write_all(body).await.unwrap();
                } else {
                    let body = r#"{"text":"hello world"}"#;
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    sock.write_all(resp.as_bytes()).await.unwrap();
                }
            }
        });

        let (router, state) = app();
        {
            let db = state.lock().unwrap();
            let data = serde_json::json!({
                "apiKey": "sk-mock",
                "providerSpecificData": { "baseUrl": format!("http://{addr}") }
            })
            .to_string();
            db.execute(
                "INSERT INTO providerConnections (id, provider, authType, name, data, isActive, priority, createdAt, updatedAt) VALUES ('c1', 'openai', 'apikey', 'mock', ?1, 1, 1, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
                [data],
            )
            .unwrap();
        }

        // TTS
        let req = Request::builder()
            .method("POST")
            .uri("/v1/audio/speech")
            .header("authorization", "Bearer sk-test")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"model":"openai/tts-1","input":"hi","response_format":"json"}"#))
            .unwrap();
        let (status, v) = call(router.clone(), req).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["format"], "mp3");

        // STT
        let (ct, payload) = crate::handlers::media::multipart::MultipartBuilder::new("btest")
            .field("model", "openai/whisper-1")
            .file("file", "clip.wav", "audio/wav", b"RIFFDATA")
            .finish();
        let req = Request::builder()
            .method("POST")
            .uri("/v1/audio/transcriptions")
            .header("authorization", "Bearer sk-test")
            .header("content-type", ct)
            .body(Body::from(payload))
            .unwrap();
        let (status, v) = call(router, req).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["text"], "hello world");
    }
}
