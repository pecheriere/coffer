//! Content-hashed static files: `/static/<path>` serves the asset whose URL
//! is exactly that. A URL with a stale hash is not found, never redirected
//! to the current file.

use axum::Router;
use axum::extract::Path;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse as _, Response};
use axum::routing::get;

use super::AppState;
use crate::assets::ALL;

pub(super) fn router() -> Router<AppState> {
    Router::new().route("/static/{*path}", get(serve))
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
