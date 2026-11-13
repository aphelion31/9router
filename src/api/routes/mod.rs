//! Ported 9router-pro API routes. Add one file per route group and merge it below.
//! `management()` is wrapped in dashboard auth, `v1()` in API-key auth (see server.rs).
use axum::Router;

use crate::server::AppState;

mod tools_headroom;
mod tools_media_providers;
mod tools_prompt_injection;
mod tools_token_saver;

pub fn management() -> Router<AppState> {
    Router::new()
        .merge(tools_token_saver::router())
        .merge(tools_prompt_injection::router())
        .merge(tools_media_providers::router())
        .merge(tools_headroom::router())
}

pub fn v1() -> Router<AppState> {
    Router::new()
}
