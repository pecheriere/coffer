//! [`ApiRouter`]: the only way to add an API route, so every route is in
//! the OpenAPI document.

use axum::Router;
use axum::extract::DefaultBodyLimit;
use utoipa_axum::router::{OpenApiRouter, UtoipaMethodRouter};

use super::openapi;
use crate::http::AppState;

/// The largest request body any `/api/v1` operation accepts.
const BODY_LIMIT: usize = 256 * 1024;

/// API routes and their OpenAPI operations, built together.
pub struct ApiRouter(OpenApiRouter<AppState>);

impl ApiRouter {
    /// No routes yet; the document's info, servers, tags and security
    /// scheme are set.
    #[must_use]
    pub fn new() -> Self {
        Self(OpenApiRouter::with_openapi(openapi::base()))
    }

    /// Adds the routes of one `utoipa_axum::routes!` invocation, documented
    /// by their `#[utoipa::path]`.
    #[must_use]
    pub fn op(self, routes: UtoipaMethodRouter<AppState>) -> Self {
        Self(self.0.routes(routes))
    }

    /// The router, limited to [`BODY_LIMIT`] bytes of body, and the
    /// document describing it.
    pub fn split(self) -> (Router<AppState>, utoipa::openapi::OpenApi) {
        let (router, document) = self.0.split_for_parts();
        (router.layer(DefaultBodyLimit::max(BODY_LIMIT)), document)
    }
}

impl Default for ApiRouter {
    fn default() -> Self {
        Self::new()
    }
}
