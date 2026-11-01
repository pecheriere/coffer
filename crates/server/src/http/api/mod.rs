//! The JSON API, version 1. Bearer tokens only (see `extract::ApiActor`).
//! Every route is added through `ApiRouter` (`router.rs`), so the OpenAPI
//! document describes exactly what is routed.

mod accounts;
mod audit;
mod categories;
pub(crate) mod connections;
pub mod extract;
mod me;
mod members;
pub mod openapi;
mod provider_accounts;
mod router;
mod transactions;

use axum::Router;
use axum::extract::Request;
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse as _, Response};
use utoipa_axum::routes;

use self::router::ApiRouter;
use super::AppState;
use super::pages::in_api;
use super::problem::{Problem, ProblemCode};

/// Every API route. The application router merges nothing else under
/// `/api`.
pub(super) fn router() -> Router<AppState> {
    routes().split().0
}

/// Every API operation, routed and documented.
fn routes() -> ApiRouter {
    ApiRouter::new()
        .op(routes!(me::me))
        .op(routes!(members::list))
        .op(routes!(audit::list))
        .op(routes!(accounts::list, accounts::create))
        .op(routes!(accounts::get, accounts::update))
        .op(routes!(accounts::list_balances))
        .op(routes!(accounts::set_balance))
        .op(routes!(categories::list, categories::create))
        .op(routes!(categories::update))
        .op(routes!(categories::create_group))
        .op(routes!(categories::update_group))
        .op(routes!(transactions::list, transactions::create))
        .op(routes!(transactions::batch))
        .op(routes!(transactions::merge))
        .op(routes!(transactions::unmerge))
        .op(routes!(transactions::dismiss_duplicate))
        .op(routes!(
            transactions::get,
            transactions::update,
            transactions::delete
        ))
        .op(routes!(connections::create_simplefin))
        .op(routes!(connections::reconnect_simplefin))
        .op(routes!(provider_accounts::list))
        .op(routes!(provider_accounts::bind))
        .op(routes!(provider_accounts::create_account))
        .op(routes!(openapi::serve))
}

/// Gives every routing failure under `/api` (no such route, wrong method) a
/// problem body, answered the same with or without a token.
pub(super) async fn problem_bodies(request: Request, next: Next) -> Response {
    let in_api = in_api(request.uri());
    let response = next.run(request).await;
    if !in_api || response.headers().contains_key(header::CONTENT_TYPE) {
        return response;
    }
    let problem = match response.status() {
        StatusCode::NOT_FOUND => Problem::not_found(),
        StatusCode::METHOD_NOT_ALLOWED => Problem::new(
            StatusCode::METHOD_NOT_ALLOWED,
            ProblemCode::MethodNotAllowed,
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
