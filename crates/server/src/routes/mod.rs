// SPDX-License-Identifier: AGPL-3.0-only
//! HTTP and WebSocket routes.

pub mod api;
mod guard;
pub mod static_files;
pub mod ws;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::routing::{delete, get, post};
use axum::{Router, middleware};

use crate::state::AppState;

pub fn router(state: Arc<AppState>, bind: SocketAddr, allow_remote: bool) -> Router {
    Router::new()
        .route("/", get(static_files::index))
        .route("/app.js", get(static_files::app_js))
        .route("/style.css", get(static_files::style_css))
        .route("/api/hosts", get(api::list_hosts))
        .route("/api/register", post(api::register))
        .route("/api/psn/login", get(api::psn_login))
        .route("/api/psn/account-id", post(api::psn_account_id))
        .route("/api/hosts/{id}", delete(api::delete_host))
        .route("/api/discover", post(api::discover))
        .route("/api/wakeup/{id}", post(api::wakeup))
        .route("/api/session/start", post(api::start_session))
        .route("/api/session/answer", post(api::answer))
        .route("/api/session/stop", post(api::stop_session))
        .route("/api/status", get(api::status))
        .route("/api/session/ws", get(ws::ws_handler))
        .route("/api/demo/start", post(crate::demo::start))
        .layer(middleware::from_fn_with_state(
            guard::LocalOnly::new(bind, allow_remote),
            guard::local_only,
        ))
        .with_state(state)
}
