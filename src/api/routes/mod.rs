//! Ported 9router-pro API routes. Add one file per route group and merge it below.
//! `management()` is wrapped in dashboard auth, `v1()` in API-key auth (see server.rs).
use axum::Router;

use crate::server::AppState;

mod oauth_core;
mod oauth_imports;
mod obs_events;
mod obs_usage;

pub fn management() -> Router<AppState> {
    Router::new()
        .merge(obs_events::router())
        .merge(obs_usage::router())
        .merge(oauth_core::router())
        .merge(oauth_imports::router())
}

pub fn public() -> Router<AppState> {
    Router::new().merge(oauth_core::public_router())
}

pub fn v1() -> Router<AppState> {
    Router::new()
}
