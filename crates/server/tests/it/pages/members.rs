//! Passkeys, members, invites and the tokens an admin sees on the pages.

use super::bearer_status;
use axum::http::{StatusCode, header};
use coffer_domain::identity::{Role, Scope, TokenName};
use coffer_server::auth::AuthError;
use coffer_server::testkit::TEST_PUBLIC_URL;
use coffer_server::testkit::browser::{Browser, Reply, with_admin};
use secrecy::ExposeSecret as _;
use serde_json::json;
use url::Url;
use webauthn_rs::prelude::CreationChallengeResponse;

#[tokio::test]
async fn passkeys_can_be_added_and_removed() {
    let (server, mut admin, _) = with_admin().await;
    let account = admin.get("/account").await;
    let csrf = account.csrf();

    // Add a second passkey through the session-bound ceremony.
    let begun = admin
        .post_json_with(
            "/auth/passkeys/begin",
            Some(TEST_PUBLIC_URL),
            Some(&csrf),
            &json!({}),
        )
        .await;
    assert_eq!(begun.status, StatusCode::OK, "`begun` status");
    let begun = begun.json();
    let options: CreationChallengeResponse =
        serde_json::from_value(begun["options"].clone()).unwrap();
    let mut second = server.authenticator();
    let credential = second.register(options).unwrap();
    let done = admin
        .post_json_with(
            "/auth/passkeys/finish",
            Some(TEST_PUBLIC_URL),
            Some(&csrf),
            &json!({
                "ceremony": begun["ceremony"],
                "label": "second key",
                "credential": credential,
            }),
        )
        .await;
    assert_eq!(done.status, StatusCode::OK, "`done` status");

    let listed = admin.get("/account").await;
    assert!(listed.body.contains("second key"));

    // Remove it again; the form carries the passkey id.
    let after_label = Reply {
        body: listed.body[listed.body.find("second key").unwrap()..].to_owned(),
        ..listed
    };
    let id = after_label.between("action=\"/account/passkeys/", "/remove\"");
    let removed = admin
        .post_form(
            &format!("/account/passkeys/{id}/remove"),
            &[("csrf", &csrf)],
        )
        .await;
    assert_eq!(removed.status, StatusCode::SEE_OTHER);
    assert!(!admin.get("/account").await.body.contains("second key"));
}

#[tokio::test]
async fn last_passkey_cannot_be_removed() {
    let (_server, mut admin, _) = with_admin().await;
    let account = admin.get("/account").await;
    let id = account.between("action=\"/account/passkeys/", "/remove\"");

    let reply = admin
        .post_form(
            &format!("/account/passkeys/{id}/remove"),
            &[("csrf", &account.csrf())],
        )
        .await;

    assert_eq!(reply.status, StatusCode::CONFLICT);
    assert!(reply.body.contains("last passkey"));
    assert!(admin.get("/account").await.body.contains("soft passkey"));
}

#[tokio::test]
async fn members_page_manages_members_and_reenroll_links() {
    let (server, mut admin, _) = with_admin().await;
    let invite = server
        .identity
        .create_invite(
            &server
                .sessions
                .resolve(admin.cookie.as_deref().unwrap())
                .await
                .unwrap(),
            Role::Member,
            None,
        )
        .await
        .unwrap();
    let mut bob = Browser::new(&server);
    let mut bob_key = server.authenticator();
    assert_eq!(
        bob.enroll(&mut bob_key, &invite, "bob", "Bob").await.status,
        StatusCode::OK
    );

    let members = admin.get("/admin/members").await;
    let csrf = members.csrf();
    let bob_row = Reply {
        body: members.body[members.body.find("class=\"item-title\">bob<").unwrap()..].to_owned(),
        ..members
    };
    let bob_id = bob_row.between("action=\"/admin/members/", "/role\"");

    // A re-enroll link is issued and shown once; its page hides the name
    // fields because the member already exists.
    let issued = admin
        .post_form(
            &format!("/admin/members/{bob_id}/reenroll"),
            &[("csrf", &csrf)],
        )
        .await;
    assert_eq!(issued.status, StatusCode::OK, "`issued` status");
    assert_eq!(issued.header(header::CACHE_CONTROL), "no-store");
    let link = Url::parse(&issued.between("id=\"new-link\">", "<")).unwrap();
    let page = Browser::new(&server).get(link.path()).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(!page.body.contains("name=\"username\""));

    // Promote and disable through the forms; disabling ends bob's session.
    let promoted = admin
        .post_form(
            &format!("/admin/members/{bob_id}/role"),
            &[("csrf", &csrf), ("role", "admin")],
        )
        .await;
    assert_eq!(promoted.status, StatusCode::SEE_OTHER);
    assert_eq!(bob.get("/admin/members").await.status, StatusCode::OK);
    let disabled = admin
        .post_form(
            &format!("/admin/members/{bob_id}/disable"),
            &[("csrf", &csrf)],
        )
        .await;
    assert_eq!(disabled.status, StatusCode::SEE_OTHER);
    assert_eq!(bob.get("/account").await.status, StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn invites_page_lists_and_revokes_pending_invites() {
    let (_server, mut admin, _) = with_admin().await;
    let page = admin.get("/admin/invites").await;
    let csrf = page.csrf();
    admin
        .post_form("/admin/invites", &[("csrf", &csrf), ("role", "member")])
        .await;

    let listed = admin.get("/admin/invites").await;
    let id = listed.between("action=\"/admin/invites/", "/revoke\"");
    let revoked = admin
        .post_form(&format!("/admin/invites/{id}/revoke"), &[("csrf", &csrf)])
        .await;

    assert_eq!(revoked.status, StatusCode::SEE_OTHER);
    assert!(!admin.get("/admin/invites").await.body.contains("/revoke\""));
}

#[tokio::test]
async fn signing_in_again_ends_the_previous_session() {
    let (server, mut admin, mut auth) = with_admin().await;
    let old = admin.cookie.clone().unwrap();

    let done = admin.login(&mut auth, "alice").await;

    assert_eq!(done.status, StatusCode::OK, "`done` status");
    let new = admin.cookie.clone().unwrap();
    assert_ne!(old, new);
    assert!(matches!(
        server.sessions.resolve(&old).await,
        Err(AuthError::Unauthenticated)
    ));
    server.sessions.resolve(&new).await.unwrap();
}

#[tokio::test]
async fn admin_sees_and_revokes_member_tokens() {
    let (server, mut admin, _) = with_admin().await;
    let alice = server
        .sessions
        .resolve(admin.cookie.as_deref().unwrap())
        .await
        .unwrap();
    let invite = server
        .identity
        .create_invite(&alice, Role::Member, None)
        .await
        .unwrap();
    let mut bob = Browser::new(&server);
    let mut bob_key = server.authenticator();
    assert_eq!(
        bob.enroll(&mut bob_key, &invite, "bob", "Bob").await.status,
        StatusCode::OK
    );
    let bob_actor = server
        .sessions
        .resolve(bob.cookie.as_deref().unwrap())
        .await
        .unwrap();
    let (bob_token, bob_secret) = server
        .agent_tokens
        .create(
            &bob_actor,
            TokenName::parse("bob-planner").unwrap(),
            Scope::Read,
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    let (alice_token, alice_secret) = server
        .agent_tokens
        .create(
            &alice,
            TokenName::parse("alice-agent").unwrap(),
            Scope::Read,
            None,
            Vec::new(),
        )
        .await
        .unwrap();

    // The members page lists every member's tokens, without their secrets.
    let members = admin.get("/admin/members").await;
    assert_eq!(members.status, StatusCode::OK);
    assert!(members.body.contains("bob-planner"));
    assert!(members.body.contains(&bob_token.prefix));
    assert!(!members.body.contains(bob_secret.expose_secret()));
    let revoke_path = format!("/admin/tokens/{}/revoke", bob_token.id);
    assert!(
        members.body.contains(&revoke_path),
        "revoke link missing from the members page"
    );

    // A member may not use it, not even on another member's token.
    let bob_page = bob.get("/account").await;
    let denied = bob
        .post_form(
            &format!("/admin/tokens/{}/revoke", alice_token.id),
            &[("csrf", &bob_page.csrf())],
        )
        .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    assert_eq!(
        bearer_status(&mut bob, alice_secret.expose_secret()).await,
        StatusCode::OK
    );

    // Without the anti-forgery token, nothing happens.
    let forged = admin.post_form(&revoke_path, &[]).await;
    assert_eq!(forged.status, StatusCode::FORBIDDEN);
    assert_eq!(
        bearer_status(&mut admin, bob_secret.expose_secret()).await,
        StatusCode::OK
    );

    let revoked = admin
        .post_form(&revoke_path, &[("csrf", &members.csrf())])
        .await;
    assert_eq!(revoked.status, StatusCode::SEE_OTHER);
    assert_eq!(revoked.header(header::LOCATION), "/admin/members");
    assert_eq!(
        bearer_status(&mut admin, bob_secret.expose_secret()).await,
        StatusCode::UNAUTHORIZED
    );
    let listed = admin.get("/admin/members").await;
    assert!(!listed.body.contains(&revoke_path));
    assert!(listed.body.contains("revoked"));
}
