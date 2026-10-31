//! Hashed static assets, served through the real router.

use axum::http::{StatusCode, header};
use coffer_server::assets::{
    ALL, ICONS_APPLE_TOUCH_ICON_PNG, ICONS_FAVICON_ICO, ICONS_FAVICON_SVG, MANIFEST_WEBMANIFEST,
};
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

#[tokio::test]
async fn favicon_is_served_at_the_root() {
    let server = TestServer::new().await;
    let mut browser = Browser::new(&server);
    let reply = browser.get("/favicon.ico").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.header(header::CONTENT_TYPE), "image/x-icon");
    assert_eq!(reply.header(header::CACHE_CONTROL), "public, max-age=86400");
    assert_eq!(reply.bytes, ICONS_FAVICON_ICO.body);
    assert!(reply.bytes.starts_with(&[0, 0, 1, 0]));
}

#[tokio::test]
async fn manifest_lists_served_icons() {
    let server = TestServer::new().await;
    let mut browser = Browser::new(&server);
    let manifest: serde_json::Value =
        serde_json::from_slice(MANIFEST_WEBMANIFEST.body).expect("manifest is JSON");
    assert_eq!(manifest["start_url"], "/");
    let icons = manifest["icons"].as_array().expect("icons array");
    assert_eq!(icons.len(), 3);
    for icon in icons {
        let src = icon["src"].as_str().expect("icon src");
        let reply = browser.get(src).await;
        assert_eq!(reply.status, StatusCode::OK, "{src}");
        assert_eq!(reply.header(header::CONTENT_TYPE), "image/png", "{src}");
    }
}

#[tokio::test]
async fn pages_declare_icons_and_manifest() {
    let server = TestServer::new().await;
    let mut browser = Browser::new(&server);
    let page = browser.get("/login").await;
    for link in [
        format!(
            "<link rel=\"icon\" type=\"image/svg+xml\" href=\"{}\">",
            ICONS_FAVICON_SVG.url
        ),
        "<link rel=\"icon\" href=\"/favicon.ico\" sizes=\"48x48\">".to_owned(),
        format!(
            "<link rel=\"apple-touch-icon\" href=\"{}\">",
            ICONS_APPLE_TOUCH_ICON_PNG.url
        ),
        format!(
            "<link rel=\"manifest\" href=\"{}\">",
            MANIFEST_WEBMANIFEST.url
        ),
    ] {
        assert!(page.body.contains(&link), "missing {link}");
    }
}
