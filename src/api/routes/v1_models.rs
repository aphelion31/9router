//! `GET /v1/models/{kind}` and `GET /v1/models/{provider}/{model}`. Port of the kind-slug and
//! single-model branches in `v1/models/[...model]/route.js`. Registry-backed: the JS dynamic layer
//! (connections, combos, custom models, key restrictions) is not ported, so kind filters
//! return registry entries only and the restricted-key filter is not applied (see API key access in server.rs).
use axum::{
    Json, Router,
    extract::Path,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use serde_json::{Value, json};

use crate::{providers, server::AppState};

// URL slug -> registry kinds. `web` covers webSearch and webFetch, as in JS.
fn kinds_for_slug(slug: &str) -> Option<&'static [&'static str]> {
    Some(match slug {
        "image" => &["image"],
        "tts" => &["tts"],
        "stt" => &["stt"],
        "embedding" => &["embedding"],
        "image-to-text" => &["imageToText"],
        "web" => &["webSearch", "webFetch"],
        _ => return None,
    })
}

fn model_kind(m: &providers::Model) -> &str {
    m.metadata.get("kind").and_then(Value::as_str).unwrap_or("llm")
}

pub fn router() -> Router<AppState> {
    Router::new().route("/v1/models/{*model}", get(by_path))
}

async fn by_path(Path(model): Path<String>) -> Response {
    let model = model.trim_matches('/').to_string();
    if let Some(kinds) = kinds_for_slug(&model) {
        let data: Vec<Value> = providers::all()
            .iter()
            .flat_map(|p| {
                p.models.iter().filter_map(move |m| {
                    let kind = model_kind(m);
                    kinds.contains(&kind).then(|| {
                        json!({
                            "id": format!("{}/{}", p.alias, m.id),
                            "object": "model",
                            "owned_by": p.id,
                            "kind": kind,
                        })
                    })
                })
            })
            .collect();
        return Json(json!({"object":"list","data":data})).into_response();
    }
    let found = model.split_once('/').and_then(|(alias, id)| {
        let p = providers::lookup(alias)?;
        let m = p.models.iter().find(|m| m.id == id && model_kind(m) == "llm")?;
        Some(json!({
            "id": format!("{}/{}", p.alias, m.id),
            "object": "model",
            "owned_by": p.id,
        }))
    });
    match found {
        Some(v) => Json(v).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"error":{
                "message": format!("The model '{model}' does not exist or you do not have access to it."),
                "type": "invalid_request_error",
                "code": "model_not_found",
            }})),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use crate::api::routes::test_support::{app, call};
    use axum::{body::Body, http::{Request, StatusCode}};

    #[tokio::test]
    async fn unknown_model_is_404_openai_error() {
        let (router, _) = app();
        let (status, v) = call(router, Request::get("/v1/models/nope/none").header("authorization", "Bearer sk-test").body(Body::empty()).unwrap()).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(v["error"]["code"], "model_not_found");
    }

    #[tokio::test]
    async fn kind_slug_lists_list_object() {
        let (router, _) = app();
        let (status, v) = call(router, Request::get("/v1/models/tts").header("authorization", "Bearer sk-test").body(Body::empty()).unwrap()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["object"], "list");
        assert!(v["data"].as_array().is_some_and(|a| a.iter().all(|m| m["kind"] == "tts")));
    }
}
