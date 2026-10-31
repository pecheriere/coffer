//! The JSON API, version 1. Bearer tokens only (see `extract::ApiActor`).

mod audit;
mod me;
mod members;

use axum::Router;
use axum::extract::Request;
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse as _, Response};
use axum::routing::get;

use super::AppState;
use super::problem::Problem;

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/me", get(me::me))
        .route("/api/v1/members", get(members::list))
        .route("/api/v1/audit", get(audit::list))
}

/// Gives every routing failure under `/api` (no such route, wrong method) a
/// problem body, answered the same with or without a token.
pub(super) async fn problem_bodies(request: Request, next: Next) -> Response {
    let path = request.uri().path();
    let in_api = path == "/api" || path.starts_with("/api/");
    let response = next.run(request).await;
    if !in_api || response.headers().contains_key(header::CONTENT_TYPE) {
        return response;
    }
    let problem = match response.status() {
        StatusCode::NOT_FOUND => Problem::new(StatusCode::NOT_FOUND, "not_found", "Not found"),
        StatusCode::METHOD_NOT_ALLOWED => Problem::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            "Method not allowed",
        ),
        _ => return response,
    };
    let mut replaced = problem.into_response();
    if let Some(allow) = response.headers().get(header::ALLOW) {
        replaced.headers_mut().insert(header::ALLOW, allow.clone());
    }
    replaced
}
