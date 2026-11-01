//! CSRF, origin checks, security headers and what the pages refuse to read.

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use coffer_domain::identity::{Scope, TokenName};
use coffer_server::assets::CSS_BASE_CSS;
use coffer_server::testkit::browser::{Browser, with_admin};
use coffer_server::testkit::{TEST_PUBLIC_URL, TestServer};
use serde_json::json;

#[tokio::test]
async fn post_without_csrf_rejected() {
    let (_server, mut admin, _) = with_admin().await;

    let missing = admin
        .post_form("/account/tokens", &[("name", "sneaky"), ("scope", "read")])
        .await;
    assert_eq!(missing.status, StatusCode::FORBIDDEN);
    let wrong = admin
        .post_form(
            "/account/tokens",
            &[("csrf", "AAAA"), ("name", "sneaky"), ("scope", "read")],
        )
        .await;
    assert_eq!(wrong.status, StatusCode::FORBIDDEN);
    let logout = admin.post_form("/logout", &[]).await;
    assert_eq!(logout.status, StatusCode::FORBIDDEN);
    let invite = admin
        .post_form("/admin/invites", &[("role", "member")])
        .await;
    assert_eq!(invite.status, StatusCode::FORBIDDEN);

    // The add-passkey ceremony needs the header.
    let begin = admin
        .post_json_with(
            "/auth/passkeys/begin",
            Some(TEST_PUBLIC_URL),
            None,
            &json!({}),
        )
        .await;
    assert_eq!(begin.status, StatusCode::FORBIDDEN);

    // Nothing happened, and the session is still good.
    let after = admin.get("/account").await;
    assert_eq!(after.status, StatusCode::OK);
    assert!(!after.body.contains("sneaky"));
}

#[tokio::test]
async fn security_headers_present() {
    let server = TestServer::new().await;
    let mut browser = Browser::new(&server);

    let page = browser.get("/login").await;

    assert_eq!(page.status, StatusCode::OK);
    assert_eq!(
        page.header(header::CONTENT_SECURITY_POLICY),
        "default-src 'self'; script-src 'self'; style-src 'self'; frame-ancestors 'none'; \
         form-action 'self'; base-uri 'none'"
    );
    assert_eq!(page.header(header::REFERRER_POLICY), "same-origin");
    assert_eq!(page.header(header::X_CONTENT_TYPE_OPTIONS), "nosniff");
    assert!(
        !page.body.contains("<script>") && !page.body.contains("style=\""),
        "pages carry no inline script or style"
    );
}

/// `same-origin`, not `no-referrer`: under `no-referrer` a browser sends
/// `Origin: null` on a form POST, which the origin rule refuses, so every
/// plain form would fail. `same-origin` still sends nothing to other sites,
/// so setup and invite tokens in the path never leave coffer.
#[tokio::test]
async fn referrer_policy_lets_forms_send_their_origin() {
    let server = TestServer::new().await;
    let mut browser = Browser::new(&server);
    for path in ["/login", CSS_BASE_CSS.url] {
        let reply = browser.get(path).await;
        assert_eq!(reply.status, StatusCode::OK, "{path}");
        assert_eq!(
            reply.header(header::REFERRER_POLICY),
            "same-origin",
            "{path}"
        );
    }
}

#[tokio::test]
async fn invite_page_no_store() {
    let server = TestServer::new().await;
    let setup = server.identity.bootstrap_link().await.unwrap().unwrap();
    let mut browser = Browser::new(&server);

    let page = browser.get(setup.path()).await;

    assert_eq!(page.status, StatusCode::OK);
    assert_eq!(page.header(header::CACHE_CONTROL), "no-store");
    assert_eq!(page.header(header::REFERRER_POLICY), "same-origin");
}

#[tokio::test]
async fn unusable_link_page_is_generic() {
    let server = TestServer::new().await;
    let mut browser = Browser::new(&server);

    let page = browser.get("/invite/not-a-real-token").await;

    assert_eq!(page.status, StatusCode::GONE);
    assert_eq!(page.header(header::CACHE_CONTROL), "no-store");
    assert!(!page.body.contains("not-a-real-token"));
}

#[tokio::test]
async fn cross_origin_post_rejected() {
    let server = TestServer::new().await;
    let mut browser = Browser::new(&server);
    let body = json!({ "username": "alice" });

    let missing = browser
        .post_json_with("/auth/login/begin", None, None, &body)
        .await;
    assert_eq!(missing.status, StatusCode::FORBIDDEN);
    let foreign = browser
        .post_json_with("/auth/login/begin", Some("https://evil.test"), None, &body)
        .await;
    assert_eq!(foreign.status, StatusCode::FORBIDDEN);
    let enroll = browser
        .post_json_with(
            "/auth/enroll/begin",
            Some("https://evil.test"),
            None,
            &json!({ "token": "x" }),
        )
        .await;
    assert_eq!(enroll.status, StatusCode::FORBIDDEN);

    // The same request from the public origin reaches the handler.
    let ok = browser
        .post_json_with("/auth/login/begin", Some(TEST_PUBLIC_URL), None, &body)
        .await;
    assert_ne!(ok.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn bearer_token_does_not_authenticate_pages() {
    let (server, admin, _) = with_admin().await;
    let alice = server
        .sessions
        .resolve(admin.cookie.as_deref().unwrap())
        .await
        .unwrap();
    let (_, token) = server
        .agent_tokens
        .create(
            &alice,
            TokenName::parse("agent").unwrap(),
            Scope::Admin,
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    let bearer = format!("Bearer {}", secrecy::ExposeSecret::expose_secret(&token));
    let mut anonymous = Browser::new(&server);

    for path in [
        "/account",
        "/connections",
        "/admin/members",
        "/admin/invites",
        "/admin/household",
    ] {
        let request = Request::builder()
            .uri(path)
            .header(header::AUTHORIZATION, &bearer)
            .body(Body::empty())
            .unwrap();
        let reply = anonymous.send(request).await;
        assert_eq!(reply.status, StatusCode::SEE_OTHER, "{path}");
        assert_eq!(reply.header(header::LOCATION), "/login", "{path}");
    }
    // The cookie-less browser cannot use the ceremony endpoints either.
    let reply = anonymous
        .post_json_with(
            "/auth/passkeys/begin",
            Some(TEST_PUBLIC_URL),
            None,
            &json!({}),
        )
        .await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn only_the_exact_public_origin_may_post() {
    let (_server, mut admin, _) = with_admin().await;
    let csrf = admin.get("/account").await.csrf();
    let fields = [
        ("csrf", csrf.as_str()),
        ("name", "sneaky"),
        ("scope", "read"),
    ];

    // A logged-in form post with a valid CSRF token is still refused from
    // another origin, as an HTML error page.
    for origin in [
        "https://evil.test",
        "null",
        "http://coffer.test",
        "https://coffer.test:8443",
        "https://coffer.test.evil.test",
    ] {
        let reply = admin
            .post_form_from("/account/tokens", origin, &fields)
            .await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{origin}");
        assert!(
            reply.header(header::CONTENT_TYPE).starts_with("text/html"),
            "{origin}"
        );
        let json = admin
            .post_json_with(
                "/auth/login/begin",
                Some(origin),
                None,
                &json!({"username": "alice"}),
            )
            .await;
        assert_eq!(json.status, StatusCode::FORBIDDEN, "{origin}");
    }
    assert!(!admin.get("/account").await.body.contains("sneaky"));
}

#[tokio::test]
async fn unreadable_bodies_get_generic_errors() {
    let (_server, mut admin, _) = with_admin().await;
    let csrf = admin.get("/account").await.csrf();

    // Page route: a form missing its fields is a generic HTML error.
    let form = admin.post_form("/account/tokens", &[("csrf", &csrf)]).await;
    assert_eq!(form.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(form.header(header::CONTENT_TYPE).starts_with("text/html"));
    assert!(!form.body.contains("missing field"));

    // Ceremony route: missing and malformed JSON are 422 `invalid`.
    for body in [json!({}), json!("not an object")] {
        let reply = admin.post_json("/auth/login/begin", &body).await;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(reply.json()["code"], "invalid");
        assert!(!reply.body.contains("missing field"));
        assert!(!reply.body.contains("invalid type"));
    }
    let request = Request::builder()
        .method(Method::POST)
        .uri("/auth/login/begin")
        .header(header::ORIGIN, TEST_PUBLIC_URL)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{"))
        .unwrap();
    let malformed = admin.send(request).await;
    assert_eq!(malformed.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(malformed.json()["code"], "invalid");
}
