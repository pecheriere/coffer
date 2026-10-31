//! Content-hashed static files: `/static/<path>` serves the asset whose URL
//! is exactly that. A URL with a stale hash is not found, never redirected
//! to the current file.

use axum::Router;
use axum::extract::Path;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse as _, Response};
use axum::routing::get;

use super::AppState;
use crate::assets::{ALL, ICONS_FAVICON_ICO};

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route("/static/{*path}", get(serve))
        .route("/favicon.ico", get(favicon))
}

/// The unhashed `/favicon.ico` that browsers request by convention; its URL
/// never changes, so it is cached for a day only.
async fn favicon() -> Response {
    (
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static(ICONS_FAVICON_ICO.content_type),
            ),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=86400"),
            ),
        ],
        ICONS_FAVICON_ICO.body,
    )
        .into_response()
}

async fn serve(Path(path): Path<String>) -> Response {
    let url = format!("/static/{path}");
    let Some(asset) = ALL.iter().find(|asset| asset.url == url) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    (
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static(asset.content_type),
            ),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=31536000, immutable"),
            ),
        ],
        asset.body,
    )
        .into_response()
}
