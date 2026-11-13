//! Ported 9router-pro API routes. Add one file per route group and merge it below.
//! `management()` is wrapped in dashboard auth, `v1()` in API-key auth (see server.rs).
use axum::Router;

use crate::server::AppState;

mod tools_cli_tools;
mod tools_headroom;
mod tools_media_providers;
mod tools_prompt_injection;
mod tools_proxy_pools;
mod tools_pxpipe;
mod tools_token_saver;
mod tools_translator;
mod tools_tunnel;

pub fn management() -> Router<AppState> {
    Router::new()
        .merge(tools_token_saver::router())
        .merge(tools_prompt_injection::router())
        .merge(tools_media_providers::router())
        .merge(tools_headroom::router())
        .merge(tools_pxpipe::router())
        .merge(tools_translator::router())
        .merge(tools_proxy_pools::router())
        .merge(tools_tunnel::router())
        .merge(tools_cli_tools::router())
}

pub fn v1() -> Router<AppState> {
    Router::new()
}
