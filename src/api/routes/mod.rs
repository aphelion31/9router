//! Ported 9router-pro API routes. Add one file per route group and merge it below.
//! `management()` is wrapped in dashboard auth, `v1()` in API-key auth (see server.rs).
use axum::{
    Json, Router,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;

use crate::server::AppState;
mod mgmt_misc;
mod mgmt_models;
mod mgmt_providers;
mod mgmt_system;

mod v1_catalog;
mod v1_count_tokens;
mod v1_models;
mod v1_stubs;
mod v1_usage;

mod oauth_core;
mod oauth_imports;
mod obs_events;
mod obs_usage;

mod tools_cli_tools;
mod tools_headroom;
mod tools_mcp;
mod tools_media_providers;
mod tools_prompt_injection;
mod tools_proxy_pools;
mod tools_pxpipe;
mod tools_token_saver;
mod tools_translator;
mod tools_tunnel;

pub fn management() -> Router<AppState> {
    Router::new()
        .merge(mgmt_system::routes())
        .merge(mgmt_providers::routes())
        .merge(mgmt_models::routes())
        .merge(mgmt_misc::routes())
        .merge(obs_events::router())
        .merge(obs_usage::router())
        .merge(oauth_core::router())
        .merge(oauth_imports::router())
        .merge(tools_token_saver::router())
        .merge(tools_prompt_injection::router())
        .merge(tools_media_providers::router())
        .merge(tools_headroom::router())
        .merge(tools_pxpipe::router())
        .merge(tools_translator::router())
        .merge(tools_proxy_pools::router())
        .merge(tools_tunnel::router())
        .merge(tools_cli_tools::router())
        .merge(tools_mcp::router())
}

pub fn public() -> Router<AppState> {
    Router::new().merge(oauth_core::public_router())
}

pub fn v1() -> Router<AppState> {
    Router::new()
        .merge(v1_count_tokens::router())
        .merge(v1_usage::router())
        .merge(v1_stubs::router())
        .merge(v1_catalog::router())
        .merge(v1_models::router())
}

/// 501 for routes whose upstream service is not ported yet.
#[allow(dead_code)]
pub(crate) fn not_available() -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({"error":"not available"})),
    )
        .into_response()
}

#[cfg(test)]
pub(crate) mod test_support {
    use crate::server::AppState;
    use axum::{
        Router,
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use rusqlite::Connection;
    use serde_json::Value;
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;

    /// v1 router over an in-memory DB with one active key `sk-test` ("Test key", id `k1`).
    pub fn app() -> (Router, Arc<Mutex<Connection>>) {
        let db = Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        db.execute(
            "INSERT INTO apiKeys(id,key,name,createdAt,isActive,accessRestricted,accessAllow) \
             VALUES('k1','sk-test','Test key','2026-01-01T00:00:00.000Z',1,0,'[]')",
            [],
        )
        .unwrap();
        let db = Arc::new(Mutex::new(db));
        let state = AppState {
            db: Arc::clone(&db),
            executor: Arc::new(crate::api::executor::HttpExecutor(reqwest::Client::new())),
            jwt_secret: None,
        };
        (super::v1().with_state(state), db)
    }

    pub async fn call(app: Router, req: Request<Body>) -> (StatusCode, Value) {
        let res = app.oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }
}
