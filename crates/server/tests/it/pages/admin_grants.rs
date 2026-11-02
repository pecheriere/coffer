//! Token grants on the admin members page: shown, edited, and refused.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions unwrap to fail the test"
)]

use super::token_grants::{grants_token, new_account, with_accounts};
use axum::http::{StatusCode, header};
use coffer_domain::audit::AuditActor;
use coffer_domain::identity::{Actor, Role, Scope, TokenName};
use coffer_domain::ids::{AccountId, AgentTokenId};
use coffer_domain::policy::{AccountLevel, GrantTarget, TokenGrant};
use coffer_domain::time::Clock as _;
use coffer_server::testkit::TestServer;
use coffer_server::testkit::browser::Browser;
use coffer_store::{AuditEvent, AuditFilter, PageRequest};
use jiff::SignedDuration;

/// A signed-in member "bob", enrolled by invite from `admin`.
async fn enrolled_member(server: &TestServer, admin: &Actor) -> (Browser, Actor) {
    let invite = server
        .identity
        .create_invite(admin, Role::Member, None)
        .await
        .unwrap();
    let mut bob = Browser::new(server);
    let mut key = server.authenticator();
    assert_eq!(
        bob.enroll(&mut key, &invite, "bob", "Bob").await.status,
        StatusCode::OK,
        "bob enrolls"
    );
    let actor = server
        .sessions
        .resolve(bob.cookie.as_deref().unwrap())
        .await
        .unwrap();
    (bob, actor)
}

fn only(account: AccountId, level: AccountLevel) -> Vec<TokenGrant> {
    vec![TokenGrant {
        target: GrantTarget::Account(account),
        level,
    }]
}

fn edit_path(id: AgentTokenId) -> String {
    format!("/account/tokens/{id}/grants")
}

async fn grant_audit(server: &TestServer, id: AgentTokenId) -> Vec<AuditEvent> {
    server
        .store
        .list_audit(
            &AuditFilter {
                entity_id: Some(id.to_string()),
                ..AuditFilter::default()
            },
            &PageRequest {
                limit: 50,
                ..PageRequest::default()
            },
        )
        .await
        .unwrap()
        .items
        .into_iter()
        .filter(|event| event.action == "token.grants")
        .collect()
}

#[tokio::test]
async fn members_page_shows_grants_and_a_form_for_each_active_token() {
    let (server, mut admin, alice, checking, _) = with_accounts().await;
    let (_, bob) = enrolled_member(&server, &alice).await;
    let id = grants_token(
        &server,
        &bob,
        Scope::Write,
        only(checking, AccountLevel::Balances),
    )
    .await;

    let page = admin.get("/admin/members").await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(
        page.body.contains("Checking: balances"),
        "the grants are summarised"
    );
    let form = page.between(&format!("action=\"{}\"", edit_path(id)), "</form>");
    assert!(
        form.contains(&format!("name=\"account_{checking}\"")),
        "the form offers the account"
    );
    assert!(
        form.contains("name=\"back\" value=\"members\""),
        "the form returns to the members page"
    );
    assert!(
        form.contains("can widen"),
        "the help says the member may widen it again"
    );
    assert!(
        !form.contains("<script") && !form.contains("style="),
        "no inline script or style"
    );
}

#[tokio::test]
async fn admin_edit_changes_the_members_token_and_is_audited() {
    let (server, mut admin, alice, checking, _) = with_accounts().await;
    let (_, bob) = enrolled_member(&server, &alice).await;
    let id = grants_token(
        &server,
        &bob,
        Scope::Write,
        only(checking, AccountLevel::Write),
    )
    .await;

    let csrf = admin.get("/admin/members").await.csrf();
    let field = format!("account_{checking}");
    let saved = admin
        .post_form(
            &edit_path(id),
            &[
                ("csrf", &csrf),
                ("back", "members"),
                ("all", "none"),
                (field.as_str(), "balances"),
            ],
        )
        .await;
    assert_eq!(saved.status, StatusCode::SEE_OTHER);
    assert_eq!(
        saved.header(header::LOCATION),
        "/admin/members",
        "back to the members page"
    );
    assert_eq!(
        server.store.token_grants(id).await.unwrap(),
        only(checking, AccountLevel::Balances),
        "bob's token is narrowed"
    );
    let audit = grant_audit(&server, id).await;
    assert_eq!(audit.len(), 1, "one token.grants entry");
    assert_eq!(
        audit[0].actor,
        AuditActor::Member {
            member: alice.member,
            agent: None,
        },
        "attributed to the admin"
    );
}

#[tokio::test]
async fn return_target_is_an_allow_list() {
    let (server, mut admin, alice, checking, _) = with_accounts().await;
    let (mut bob, bob_actor) = enrolled_member(&server, &alice).await;
    let id = grants_token(
        &server,
        &bob_actor,
        Scope::Write,
        only(checking, AccountLevel::Write),
    )
    .await;
    let csrf = admin.get("/admin/members").await.csrf();
    for back in [
        "https://evil.example/",
        "//evil.example",
        "/admin/invites",
        "",
    ] {
        let saved = admin
            .post_form(
                &edit_path(id),
                &[("csrf", &csrf), ("back", back), ("all", "balances")],
            )
            .await;
        assert_eq!(saved.status, StatusCode::SEE_OTHER);
        assert_eq!(
            saved.header(header::LOCATION),
            "/account",
            "an unknown target falls back to the account page"
        );
    }

    // A member cannot use the members page, so is not sent there.
    let csrf = bob.get("/account").await.csrf();
    let saved = bob
        .post_form(
            &edit_path(id),
            &[
                ("csrf", &csrf),
                ("back", "members"),
                ("all", "transactions"),
            ],
        )
        .await;
    assert_eq!(saved.status, StatusCode::SEE_OTHER);
    assert_eq!(saved.header(header::LOCATION), "/account");
}

#[tokio::test]
async fn non_admin_cannot_see_or_use_the_members_page() {
    let (server, _admin, alice, checking, _) = with_accounts().await;
    let (mut bob, bob_actor) = enrolled_member(&server, &alice).await;
    let alices = grants_token(
        &server,
        &alice,
        Scope::Write,
        only(checking, AccountLevel::Write),
    )
    .await;
    grants_token(&server, &bob_actor, Scope::Read, Vec::new()).await;

    let page = bob.get("/admin/members").await;
    assert_eq!(page.status, StatusCode::FORBIDDEN, "no members page");
    assert!(
        !page.body.contains(&edit_path(alices)),
        "no edit form in the refusal"
    );

    let csrf = bob.get("/account").await.csrf();
    let refused = bob
        .post_form(
            &edit_path(alices),
            &[("csrf", &csrf), ("back", "members"), ("all", "write")],
        )
        .await;
    assert_eq!(refused.status, StatusCode::NOT_FOUND, "not bob's token");
    assert_eq!(
        server.store.token_grants(alices).await.unwrap(),
        only(checking, AccountLevel::Write),
        "unchanged"
    );
}

#[tokio::test]
async fn admin_edit_refuses_what_the_account_page_refuses() {
    let (server, mut admin, alice, checking, _) = with_accounts().await;
    let (_, bob) = enrolled_member(&server, &alice).await;
    let original = only(checking, AccountLevel::Balances);
    let id = grants_token(&server, &bob, Scope::Write, original.clone()).await;
    let csrf = admin.get("/admin/members").await.csrf();
    let field = format!("account_{checking}");

    // Every level none.
    let empty = admin
        .post_form(
            &edit_path(id),
            &[
                ("csrf", &csrf),
                ("back", "members"),
                ("all", "none"),
                (field.as_str(), "none"),
            ],
        )
        .await;
    assert_eq!(empty.status, StatusCode::UNPROCESSABLE_ENTITY, "empty");

    // An account at or below All accounts.
    for (all, level) in [("transactions", "balances"), ("balances", "balances")] {
        let refused = admin
            .post_form(
                &edit_path(id),
                &[
                    ("csrf", &csrf),
                    ("back", "members"),
                    ("all", all),
                    (field.as_str(), level),
                ],
            )
            .await;
        assert_eq!(
            refused.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "account at or below all"
        );
    }
    assert_eq!(server.store.token_grants(id).await.unwrap(), original);
    assert!(grant_audit(&server, id).await.is_empty(), "nothing audited");
}

#[tokio::test]
async fn inactive_tokens_offer_no_form_on_the_members_page() {
    let (server, mut admin, alice, checking, _) = with_accounts().await;
    let (_, bob) = enrolled_member(&server, &alice).await;
    let revoked = grants_token(
        &server,
        &bob,
        Scope::Read,
        only(checking, AccountLevel::Balances),
    )
    .await;
    let (expiring, _) = server
        .agent_tokens
        .create(
            &bob,
            TokenName::parse("short").unwrap(),
            Scope::Read,
            Some(server.clock.now() + SignedDuration::from_hours(1)),
            only(checking, AccountLevel::Balances),
        )
        .await
        .unwrap();
    let working = grants_token(
        &server,
        &bob,
        Scope::Read,
        only(checking, AccountLevel::Balances),
    )
    .await;
    assert!(
        admin
            .get("/admin/members")
            .await
            .body
            .contains(&edit_path(expiring.id)),
        "an unexpired token has a form"
    );

    server.agent_tokens.revoke(&bob, revoked).await.unwrap();
    server.clock.advance(SignedDuration::from_hours(2));
    let page = admin.get("/admin/members").await;
    assert!(
        !page.body.contains(&edit_path(revoked)),
        "no form for a revoked token"
    );
    assert!(
        !page.body.contains(&edit_path(expiring.id)),
        "no form for an expired token"
    );
    assert!(page.body.contains(&edit_path(working)), "a working one has");
}

#[tokio::test]
async fn members_page_grants_edit_requires_csrf() {
    let (server, mut admin, alice, checking, _) = with_accounts().await;
    let (_, bob) = enrolled_member(&server, &alice).await;
    let original = only(checking, AccountLevel::Balances);
    let id = grants_token(&server, &bob, Scope::Write, original.clone()).await;
    let path = edit_path(id);
    let forms: [&[(&str, &str)]; 2] = [
        &[("back", "members"), ("all", "write")],
        &[("csrf", "forged"), ("back", "members"), ("all", "write")],
    ];
    for fields in forms {
        let refused = admin.post_form(&path, fields).await;
        assert_eq!(refused.status, StatusCode::FORBIDDEN, "csrf is required");
    }
    assert_eq!(server.store.token_grants(id).await.unwrap(), original);
}

#[tokio::test]
async fn account_and_token_names_on_the_members_page_are_escaped() {
    let (server, mut admin, alice, checking, _) = with_accounts().await;
    let (_, bob) = enrolled_member(&server, &alice).await;
    new_account(&server, &alice, "<b>acct</b>").await;
    server
        .agent_tokens
        .create(
            &bob,
            TokenName::parse("<i>tok</i>").unwrap(),
            Scope::Write,
            None,
            only(checking, AccountLevel::Balances),
        )
        .await
        .unwrap();

    let page = admin.get("/admin/members").await;
    assert!(
        page.body.contains("&lt;b&gt;acct&lt;/b&gt;"),
        "account name"
    );
    assert!(page.body.contains("&lt;i&gt;tok&lt;/i&gt;"), "token name");
    assert!(
        !page.body.contains("<b>acct") && !page.body.contains("<i>tok"),
        "never as markup"
    );
}
