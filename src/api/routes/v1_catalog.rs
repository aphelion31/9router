//! Catalog routes backed by the static provider registry snapshot (`providers::all()`).
//! Port of `v1beta/models/route.js` and `v1/models/info/route.js` (static part).
//! The JS `/v1/models` family also merges connections, combos and custom models from the DB;
//! that dynamic layer is not ported, so kind listings and provider-scoped lookups use the registry only.
use axum::{
    Json, Router,
    extract::Query,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{providers, server::AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1beta/models", get(gemini_models))
        .route("/v1/models/info", get(info))
}

/// Gemini list: `models/{provider}/{id}` for every model, plus bare `models/{id}` for gemini.
async fn gemini_models() -> Json<Value> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    let mut push = |name: String, display: &str, desc: String, methods: &[&str]| {
        if seen.insert(name.clone()) {
            out.push(json!({
                "name": name,
                "displayName": display,
                "description": desc,
                "supportedGenerationMethods": methods,
                "inputTokenLimit": 128000,
                "outputTokenLimit": 8192,
            }));
        }
    };
    for p in providers::all() {
        for m in &p.models {
            let label = m.name.as_deref().unwrap_or(&m.id);
            push(
                format!("models/{}/{}", p.id, m.id),
                label,
                format!("{} model: {label}", p.id),
                &["generateContent"],
            );
            if p.id == "gemini" {
                push(
                    format!("models/{}", m.id),
                    label,
                    format!("Gemini model: {label}"),
                    &["generateContent", "streamGenerateContent"],
                );
            }
        }
    }
    Json(json!({"models": out}))
}

#[derive(Deserialize)]
struct InfoQuery {
    id: Option<String>,
    kind: Option<String>,
}

/// `{alias}/{modelId}` lookup. Kind is taken from the registry entry (default `llm`).
async fn info(Query(q): Query<InfoQuery>) -> Response {
    let Some(id) = q.id.filter(|s| !s.is_empty()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":{"message":"Missing required query param: id (e.g. ?id=openai/dall-e-3)","type":"invalid_request_error"}})),
        )
            .into_response();
    };
    let found = id.split_once('/').and_then(|(alias, model_id)| {
        let p = providers::lookup(alias)?;
        let m = p.models.iter().find(|m| m.id == model_id)?;
        let kind = m
            .metadata
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("llm");
        if q.kind.as_deref().is_some_and(|k| k != kind) {
            return None;
        }
        let mut out = json!({
            "id": format!("{alias}/{}", m.id),
            "name": m.name.clone().unwrap_or_else(|| m.id.clone()),
            "kind": kind,
            "owned_by": alias,
        });
        for key in ["params", "capabilities", "options", "dimensions", "contextLength"] {
            if let Some(v) = m.metadata.get(key) {
                out[key] = v.clone();
            }
        }
        Some(out)
    });
    match found {
        Some(info) => Json(info).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"error":{"message":format!("Model not found: {id}"),"type":"not_found"}})),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use crate::api::routes::test_support::{app, call};
    use axum::{body::Body, http::{Request, StatusCode}};

    #[tokio::test]
    async fn info_requires_id() {
        let (router, _) = app();
        let (status, v) = call(router, Request::get("/v1/models/info").header("authorization", "Bearer sk-test").body(Body::empty()).unwrap()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(v["error"]["type"], "invalid_request_error");
    }

    #[tokio::test]
    async fn info_unknown_is_404() {
        let (router, _) = app();
        let (status, v) = call(router, Request::get("/v1/models/info?id=nope/none").header("authorization", "Bearer sk-test").body(Body::empty()).unwrap()).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(v["error"]["type"], "not_found");
    }

    #[tokio::test]
    async fn gemini_list_has_models_key() {
        let (router, _) = app();
        let (status, v) = call(router, Request::get("/v1beta/models").header("authorization", "Bearer sk-test").body(Body::empty()).unwrap()).await;
        assert_eq!(status, StatusCode::OK);
        assert!(v["models"].as_array().is_some_and(|a| !a.is_empty()));
    }
}
