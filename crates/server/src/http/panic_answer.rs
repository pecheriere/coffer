//! The answer to a request whose handler panicked: a generic 500, a problem
//! under `/api` and a plain page anywhere else, with nothing from the panic.
//! The panic hook has already logged where it happened
//! (`cli::panic_report`), so nothing here logs.
//!
//! The answer is fixed text and touches neither the store nor the page layout,
//! so it cannot fail the way the handler did; that is why a page gets this
//! rather than the themed error page.

use std::any::Any;

use axum::Extension;
use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse as _, Response};
use tower_http::catch_panic::CatchPanicLayer;

use super::pages::in_api;
use super::problem::{Problem, ProblemCode};

const PAGE: &str = "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\">\
                    <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
                    <title>Something went wrong</title></head><body>\
                    <h1>Something went wrong</h1>\
                    <p>coffer hit an internal error. Please try again.</p>\
                    </body></html>";

/// Marks the response to a panic for [`answer`].
#[derive(Debug, Clone, Copy)]
struct Panicked;

/// Catches a panic in everything inside it. Not the layer's default response,
/// which logs the panic's message.
pub(super) fn catch_panics() -> CatchPanicLayer<fn(Box<dyn Any + Send + 'static>) -> Response> {
    CatchPanicLayer::custom(mark_panicked)
}

fn mark_panicked(_panic: Box<dyn Any + Send + 'static>) -> Response {
    Extension(Panicked).into_response()
}

/// Turns what [`catch_panics`] caught into the answer for this request's path,
/// which the catching layer cannot see.
pub(super) async fn answer(request: Request, next: Next) -> Response {
    let in_api = in_api(request.uri());
    let response = next.run(request).await;
    if response.extensions().get::<Panicked>().is_none() {
        return response;
    }
    let status = StatusCode::INTERNAL_SERVER_ERROR;
    if in_api {
        return Problem::new(status, ProblemCode::Internal, "Internal error").into_response();
    }
    let mut page = Response::new(Body::from(PAGE));
    *page.status_mut() = status;
    page.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    page
}
