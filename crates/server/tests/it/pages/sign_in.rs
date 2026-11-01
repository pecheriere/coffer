//! The full journey through the pages, sign-in and where the root sends a
//! visitor.

use super::get_and_head;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use coffer_server::testkit::browser::{Browser, with_admin};
use coffer_server::testkit::{TestServer, link_token};
use serde_json::json;
use url::Url;
use webauthn_rs::prelude::CreationChallengeResponse;

#[tokio::test]
async fn full_journey() {
    let server = TestServer::new().await;
    let setup = server.identity.bootstrap_link().await.unwrap().unwrap();
    assert!(setup.path().starts_with("/setup/"));

    // The bootstrap link renders a form and enrolls the first admin.
    let mut admin = Browser::new(&server);
    let page = admin.get(setup.path()).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("name=\"username\""));
    let mut alice_key = server.authenticator();
    let done = admin.enroll(&mut alice_key, &setup, "alice", "Alice").await;
    assert_eq!(done.status, StatusCode::OK, "`done` status");
    assert_eq!(done.json()["redirect"], "/account");
    assert!(done.header(header::SET_COOKIE).contains("HttpOnly"));

    let account = admin.get("/account").await;
    assert_eq!(account.status, StatusCode::OK);
    assert!(account.body.contains("Alice"));

    // The admin invites bob through the form and the link is shown once.
    let csrf = account.csrf();
    let invites = admin.get("/admin/invites").await;
    assert_eq!(invites.status, StatusCode::OK);
    let created = admin
        .post_form(
            "/admin/invites",
            &[("csrf", &csrf), ("role", "member"), ("display_name", "Bob")],
        )
        .await;
    assert_eq!(created.status, StatusCode::OK, "`created` status");
    assert_eq!(created.header(header::CACHE_CONTROL), "no-store");
    let link = Url::parse(&created.between("id=\"new-link\">", "<")).expect("invite link");
    assert!(link.path().starts_with("/invite/"));

    // Bob enrolls with his own authenticator and cannot see admin pages.
    let mut bob = Browser::new(&server);
    let mut bob_key = server.authenticator();
    let done = bob.enroll(&mut bob_key, &link, "bob", "Bob").await;
    assert_eq!(done.status, StatusCode::OK, "`done` status");
    let denied = bob.get("/admin/members").await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    assert!(denied.header(header::CONTENT_TYPE).starts_with("text/html"));
    assert_eq!(
        bob.get("/admin/invites").await.status,
        StatusCode::FORBIDDEN
    );

    // Bob's token form offers no scope above his role's ceiling, and the
    // server refuses one anyway.
    let bob_account = bob.get("/account").await;
    assert!(bob_account.body.contains("value=\"write\""));
    assert!(!bob_account.body.contains("value=\"admin\""));
    let refused = bob
        .post_form(
            "/account/tokens",
            &[
                ("csrf", &bob_account.csrf()),
                ("name", "x"),
                ("scope", "admin"),
            ],
        )
        .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);

    // The admin sees bob in the members list and creates a write token.
    let members = admin.get("/admin/members").await;
    assert_eq!(members.status, StatusCode::OK);
    assert!(members.body.contains("bob"));
    let created = admin
        .post_form(
            "/account/tokens",
            &[
                ("csrf", &csrf),
                ("name", "planner"),
                ("scope", "write"),
                ("expires_days", "30"),
            ],
        )
        .await;
    assert_eq!(created.status, StatusCode::OK, "`created` status");
    assert_eq!(created.header(header::CACHE_CONTROL), "no-store");
    let token = created.between("id=\"new-token\">", "<");
    assert!(token.starts_with("coffer_"));

    // The token is shown once: a reload lists the token without its secret.
    let reload = admin.get("/account").await;
    assert!(reload.body.contains("planner"));
    assert!(!reload.body.contains(&token));

    // And it works as a bearer token on the API.
    let request = Request::builder()
        .uri("/api/v1/me")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let me = admin.send(request).await;
    assert_eq!(me.status, StatusCode::OK);
    assert_eq!(me.json()["agent"]["scope"], "write");

    // Signing in again over the login endpoints works, and so does logout.
    let mut returning = Browser::new(&server);
    let done = returning.login(&mut alice_key, "alice").await;
    assert_eq!(done.status, StatusCode::OK, "`done` status");
    assert_eq!(done.json()["redirect"], "/account");
    let page = returning.get("/account").await;
    assert_eq!(page.status, StatusCode::OK);
    let out = returning
        .post_form("/logout", &[("csrf", &page.csrf())])
        .await;
    assert_eq!(out.status, StatusCode::SEE_OTHER);
    assert_eq!(out.header(header::LOCATION), "/login");
    let after = returning.get("/account").await;
    assert_eq!(after.status, StatusCode::SEE_OTHER);
    assert_eq!(after.header(header::LOCATION), "/login");
}

#[tokio::test]
async fn sign_in_trims_the_username() {
    let (server, _, mut auth) = with_admin().await;
    let mut browser = Browser::new(&server);

    let done = browser.login(&mut auth, "  Alice ").await;

    assert_eq!(done.status, StatusCode::OK, "`done` status");
}

#[tokio::test]
async fn unverifiable_registration_is_a_client_error() {
    let server = TestServer::new().await;
    let link = server.identity.bootstrap_link().await.unwrap().unwrap();
    let mut browser = Browser::new(&server);
    let begin = json!({
        "token": link_token(&link),
        "username": "alice",
        "display_name": "Alice",
    });
    let first = browser.post_json("/auth/enroll/begin", &begin).await.json();
    let second = browser.post_json("/auth/enroll/begin", &begin).await.json();
    let options: CreationChallengeResponse =
        serde_json::from_value(first["options"].clone()).unwrap();
    let credential = server.authenticator().register(options).unwrap();

    // Answers the first ceremony's challenge but finishes the second one.
    let rejected = browser
        .post_json(
            "/auth/enroll/finish",
            &json!({
                "ceremony": second["ceremony"],
                "label": "soft passkey",
                "credential": credential,
            }),
        )
        .await;

    assert_eq!(rejected.status, StatusCode::BAD_REQUEST);
    assert_eq!(rejected.json()["code"], "passkey_rejected");
    assert_eq!(server.store.list_members().await.unwrap().len(), 0);
}

#[tokio::test]
async fn root_sends_a_visitor_without_a_session_to_sign_in() {
    let server = TestServer::new().await;
    let mut browser = Browser::new(&server);
    let mut stale = Browser::new(&server);
    stale.cookie = Some("not-a-session".to_owned());

    for reply in get_and_head(&mut browser, "/")
        .await
        .into_iter()
        .chain(get_and_head(&mut stale, "/").await)
    {
        assert_eq!(reply.status, StatusCode::SEE_OTHER, "`reply` status");
        assert_eq!(reply.header(header::LOCATION), "/login");
    }
}

#[tokio::test]
async fn root_sends_a_signed_in_member_home() {
    let (_server, mut admin, _) = with_admin().await;

    for reply in get_and_head(&mut admin, "/").await {
        assert_eq!(reply.status, StatusCode::SEE_OTHER, "`reply` status");
        assert_eq!(reply.header(header::LOCATION), "/account");
    }
}
