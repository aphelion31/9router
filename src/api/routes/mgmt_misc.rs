use crate::server::AppState;
use axum::{
    Json, Router,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde_json::json;
// ponytail: default provider pricing catalog is not ported; implement it before permitting pricing reads or writes.
async fn unavailable() -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({"error":"Pricing catalog not implemented"})),
    )
        .into_response()
}
pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/tags",
            get(tags).options(|| async {
                (
                    [
                        ("access-control-allow-origin", "*"),
                        ("access-control-allow-methods", "GET, OPTIONS"),
                        ("access-control-allow-headers", "*"),
                    ],
                    "",
                )
            }),
        )
        .route(
            "/api/pricing",
            get(unavailable).patch(unavailable).delete(unavailable),
        )
}
async fn tags() -> Response {
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN,"*"),(header::ACCESS_CONTROL_ALLOW_METHODS,"GET, OPTIONS"),(header::ACCESS_CONTROL_ALLOW_HEADERS,"*")],Json(json!({"models":[{"name":"llama3.2","modified_at":"2025-12-26T00:00:00Z","size":2000000000i64,"digest":"abc123def456","details":{"format":"gguf","family":"llama","parameter_size":"3B","quantization_level":"Q4_K_M"}},{"name":"qwen2.5","modified_at":"2025-12-26T00:00:00Z","size":4000000000i64,"digest":"def456abc123","details":{"format":"gguf","family":"qwen","parameter_size":"7B","quantization_level":"Q4_K_M"}}]}))).into_response()
}
