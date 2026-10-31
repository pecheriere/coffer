//! Hashed static assets, served through the real router.

use axum::http::{StatusCode, header};
use coffer_server::assets::ALL;
use coffer_server::testkit::{TestServer, browser::Browser};

#[tokio::test]
async fn hashed_assets_are_served_immutable() {
    let server = TestServer::new().await;
    let mut browser = Browser::new(&server);
    for asset in ALL {
        let reply = browser.get(asset.url).await;
        assert_eq!(reply.status, StatusCode::OK, "{}", asset.url);
        assert_eq!(
            reply.header(header::CONTENT_TYPE),
            asset.content_type,
            "{}",
            asset.url
        );
        assert_eq!(
            reply.header(header::CACHE_CONTROL),
            "public, max-age=31536000, immutable",
            "{}",
            asset.url
        );
        assert_eq!(
            reply.header(header::X_CONTENT_TYPE_OPTIONS),
            "nosniff",
            "{}",
            asset.url
        );
        assert_eq!(reply.bytes, asset.body, "{}", asset.url);
    }
}

#[tokio::test]
async fn stale_or_unknown_static_paths_are_404() {
    let server = TestServer::new().await;
    let mut browser = Browser::new(&server);
    for path in [
        "/static/js/coffer.0000000000000000.js",
        "/static/coffer.js",
        "/static/coffer.css",
        "/static/../Cargo.toml",
        "/static/",
    ] {
        assert_eq!(
            browser.get(path).await.status,
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }
}

#[tokio::test]
async fn pages_reference_only_served_assets() {
    let server = TestServer::new().await;
    let mut browser = Browser::new(&server);
    let page = browser.get("/login").await;
    assert_eq!(page.status, StatusCode::OK);
    let mut seen = 0;
    for attribute in ["href=\"", "src=\""] {
        for (at, _) in page.body.match_indices(attribute) {
            let rest = &page.body[at + attribute.len()..];
            let url = &rest[..rest.find('"').expect("closing quote")];
            if !url.starts_with("/static/") {
                continue;
            }
            seen += 1;
            assert!(
                ALL.iter().any(|asset| asset.url == url),
                "{url} is not in ALL"
            );
            assert_eq!(browser.get(url).await.status, StatusCode::OK, "{url}");
        }
    }
    assert!(seen >= 2, "the page links a stylesheet and a script");
}
