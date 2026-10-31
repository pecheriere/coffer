//! Liveness and readiness in one: the process is up and the database answers.

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;

use super::AppState;

pub(super) fn router() -> Router<AppState> {
    Router::new().route("/healthz", get(healthz))
}

async fn healthz(State(state): State<AppState>) -> (StatusCode, &'static str) {
    match state.store.ping().await {
        Ok(()) => (StatusCode::OK, "ok"),
        Err(err) => {
            tracing::error!(error = ?err, "health check failed");
            (StatusCode::SERVICE_UNAVAILABLE, "unavailable")
        }
    }
}
