//! The HTTP layer: the router, request authentication, error bodies and the
//! server-rendered pages.
//!
//! Origin rule: every routed, state-changing (non-GET/HEAD/OPTIONS) request
//! outside `/api/` must carry an `Origin` equal to the public origin. It is
//! enforced here, once, over the whole router, so a new cookie-authenticated
//! route cannot forget it. `/api/` routes take bearer tokens only, which a
//! browser never attaches on its own, so they are exempt.

mod api;
pub mod extract;
mod health;
mod pages;
pub mod problem;
mod static_files;

use std::sync::Arc;

use axum::Router;

use coffer_domain::time::Clock;
use coffer_store::Store;

use crate::appearance::Appearance;
use crate::auth::{AgentTokens, Identity, Sessions};
use crate::config::Config;

/// Shared state of every handler.
#[derive(Clone)]
pub struct AppState {
    pub sessions: Arc<Sessions>,
    pub agent_tokens: Arc<AgentTokens>,
    pub identity: Arc<Identity>,
    pub appearance: Arc<Appearance>,
    pub store: Store,
    pub config: Config,
    pub clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState").finish_non_exhaustive()
    }
}

/// The application router.
pub fn router(state: AppState) -> Router {
    Router::new()
        .merge(api::router())
        .merge(health::router())
        .merge(pages::router())
        .merge(static_files::router())
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            pages::require_same_origin,
        ))
        .layer(axum::middleware::map_response(pages::security_headers))
        .layer(axum::middleware::from_fn(api::problem_bodies))
        .with_state(state)
}
