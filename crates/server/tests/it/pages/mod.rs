//! The web pages and ceremony endpoints, driven through the real router with
//! software passkeys and a hand-managed cookie.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions unwrap to fail the test"
)]

mod members;
mod rendering;
mod security;
mod sign_in;
mod token_grants;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use coffer_server::testkit::browser::{Browser, Reply};

/// Status of `GET /api/v1/me` with `secret` as the bearer token.
async fn bearer_status(browser: &mut Browser, secret: &str) -> StatusCode {
    let request = Request::builder()
        .uri("/api/v1/me")
        .header(header::AUTHORIZATION, format!("Bearer {secret}"))
        .body(Body::empty())
        .unwrap();
    browser.send(request).await.status
}

/// `GET` and `HEAD` of `path` with the browser's cookie, if any.
async fn get_and_head(browser: &mut Browser, path: &str) -> [Reply; 2] {
    let get = browser.get(path).await;
    let request = browser
        .builder(Method::HEAD, path)
        .body(Body::empty())
        .unwrap();
    let head = browser.send(request).await;
    [get, head]
}
