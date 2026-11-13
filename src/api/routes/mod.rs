//! Ported 9router-pro API routes. Add one file per route group and merge it below.
//! `management()` is wrapped in dashboard auth, `v1()` in API-key auth (see server.rs).
use axum::Router;

use crate::server::AppState;
mod mgmt_misc;
mod mgmt_models;
mod mgmt_providers;
mod mgmt_system;

pub fn management() -> Router<AppState> {
    Router::new()
        .merge(mgmt_system::routes())
        .merge(mgmt_providers::routes())
        .merge(mgmt_models::routes())
        .merge(mgmt_misc::routes())
}

pub fn v1() -> Router<AppState> {
    Router::new()
}
