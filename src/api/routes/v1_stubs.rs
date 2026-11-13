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
        .route("/v1/audio/speech", post(stub))
        .route("/v1/audio/transcriptions", post(stub))
        .route("/v1/audio/voices", get(stub))
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
            ("POST", "/v1/audio/speech"),
            ("POST", "/v1/audio/transcriptions"),
            ("GET", "/v1/audio/voices"),
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
}
