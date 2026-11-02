//! The token form and its grants: creating, editing and refusing them.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions unwrap to fail the test"
)]

use super::bearer_status;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use coffer_domain::accounts::AccountKind;
use coffer_domain::identity::{Actor, Role, Scope, TokenName};
use coffer_domain::ids::{AccountId, AgentTokenId};
use coffer_domain::money::Currency;
use coffer_domain::policy::{AccountLevel, GrantTarget, TokenGrant};
use coffer_server::ledger::accounts::{AccountPatch, NewAccountInput};
use coffer_server::testkit::TestServer;
use coffer_server::testkit::browser::{Browser, Reply, with_admin};
use secrecy::ExposeSecret as _;

/// A signed-in admin with accounts "Checking" and "Savings".
pub(super) async fn with_accounts() -> (TestServer, Browser, Actor, AccountId, AccountId) {
    let (server, admin, _) = with_admin().await;
    let alice = server
        .sessions
        .resolve(admin.cookie.as_deref().unwrap())
        .await
        .unwrap();
    let checking = new_account(&server, &alice, "Checking").await;
    let savings = new_account(&server, &alice, "Savings").await;
    (server, admin, alice, checking, savings)
}

pub(super) async fn new_account(server: &TestServer, actor: &Actor, name: &str) -> AccountId {
    server
        .ledger
        .create_account(
            actor,
            NewAccountInput {
                name: name.into(),
                kind: AccountKind::Checking,
                currency: Currency::parse("USD").unwrap(),
                liquidity: None,
                include_in_net_worth: true,
                include_in_forecast: true,
                opening_balance: None,
            },
            None,
        )
        .await
        .unwrap()
        .id
}

/// A token of `actor` with `scope` and `grants`.
pub(super) async fn grants_token(
    server: &TestServer,
    actor: &Actor,
    scope: Scope,
    grants: Vec<TokenGrant>,
) -> AgentTokenId {
    server
        .agent_tokens
        .create(
            actor,
            TokenName::parse("restricted").unwrap(),
            scope,
            None,
            grants,
        )
        .await
        .unwrap()
        .0
        .id
}

/// Posts the token form with `name`, `scope` and the grant `fields`.
async fn post_token_form(
    admin: &mut Browser,
    name: &str,
    scope: &str,
    fields: &[(&str, &str)],
) -> Reply {
    let csrf = admin.get("/account").await.csrf();
    let mut form = vec![("csrf", csrf.as_str()), ("name", name), ("scope", scope)];
    form.extend_from_slice(fields);
    admin.post_form("/account/tokens", &form).await
}

/// The grants of the only token named `name`, by listing the store.
async fn grants_of_named(server: &TestServer, name: &str) -> Vec<TokenGrant> {
    let tokens = server.store.list_agent_tokens(None).await.unwrap();
    let token = tokens
        .iter()
        .find(|token| token.name.as_str() == name)
        .unwrap();
    server.store.token_grants(token.id).await.unwrap()
}

#[tokio::test]
async fn token_form_defaults_reproduce_uniform_access() {
    let (server, mut admin, _, checking, savings) = with_accounts().await;
    let page = admin.get("/account").await;

    // The form as served: `all` selects write, each account selects none.
    let all = page.between("name=\"all\"", "</select>");
    assert!(
        all.contains("value=\"write\" selected"),
        "all accounts defaults to write"
    );
    for level in ["none", "balances", "transactions", "write"] {
        assert!(
            all.contains(&format!("value=\"{level}\"")),
            "offers {level}"
        );
    }
    for account in [checking, savings] {
        let select = page.between(&format!("name=\"account_{account}\""), "</select>");
        assert!(
            select.contains("value=\"none\" selected"),
            "an account defaults to none"
        );
    }

    // Submitted unchanged it makes a token without grants, whatever the
    // scope: for a read token the default is capped at what read allows.
    let checking_field = format!("account_{checking}");
    let savings_field = format!("account_{savings}");
    let fields = [
        ("all", "write"),
        (checking_field.as_str(), "none"),
        (savings_field.as_str(), "none"),
    ];
    for scope in ["read", "write", "admin"] {
        let name = format!("default-{scope}");
        let created = post_token_form(&mut admin, &name, scope, &fields).await;
        assert_eq!(created.status, StatusCode::OK, "defaults are accepted");
        assert_eq!(
            grants_of_named(&server, &name).await.len(),
            0,
            "defaults store no grants"
        );
    }

    // So does a form without the selects.
    let created = post_token_form(&mut admin, "bare", "read", &[]).await;
    assert_eq!(created.status, StatusCode::OK);
    assert_eq!(grants_of_named(&server, "bare").await.len(), 0);
}

#[tokio::test]
async fn token_form_sets_grants_and_lists_them() {
    let (server, mut admin, _, checking, savings) = with_accounts().await;
    let field = format!("account_{checking}");
    let savings_field = format!("account_{savings}");
    let created = post_token_form(
        &mut admin,
        "narrow",
        "write",
        &[
            ("all", "none"),
            (field.as_str(), "balances"),
            (savings_field.as_str(), "none"),
        ],
    )
    .await;
    assert_eq!(created.status, StatusCode::OK);
    assert_eq!(
        grants_of_named(&server, "narrow").await,
        [TokenGrant {
            target: GrantTarget::Account(checking),
            level: AccountLevel::Balances,
        }]
    );
    let secret = created.between("id=\"new-token\">", "<");
    assert_eq!(bearer_status(&mut admin, &secret).await, StatusCode::OK);

    // The list names the account and the level.
    let page = admin.get("/account").await;
    assert!(
        page.body.contains("Checking: balances"),
        "the token shows its grants"
    );

    // An `all` level below the ceiling is a grant too.
    let created = post_token_form(&mut admin, "wide", "write", &[("all", "transactions")]).await;
    assert_eq!(created.status, StatusCode::OK);
    assert_eq!(
        grants_of_named(&server, "wide").await,
        [TokenGrant {
            target: GrantTarget::All,
            level: AccountLevel::Transactions,
        }]
    );
    let page = admin.get("/account").await;
    assert!(
        page.body.contains("All accounts: transactions"),
        "the token shows its grants"
    );
}

/// Choosing nothing must not make a token that sees everything.
#[tokio::test]
async fn token_form_with_every_level_none_is_refused() {
    let (server, mut admin, _, checking, _) = with_accounts().await;
    let field = format!("account_{checking}");
    let refused = post_token_form(
        &mut admin,
        "nothing",
        "write",
        &[("all", "none"), (field.as_str(), "none")],
    )
    .await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        server.store.list_agent_tokens(None).await.unwrap().len(),
        0,
        "no token was created"
    );
}

/// `all=write` with one account lowered looks restricted but is not: the
/// best grant wins. The form refuses it instead of misleading its user.
#[tokio::test]
async fn account_level_below_all_is_refused() {
    let (server, mut admin, _, checking, _) = with_accounts().await;
    let field = format!("account_{checking}");
    for all in ["write", "transactions"] {
        let refused = post_token_form(
            &mut admin,
            "misleading",
            "write",
            &[("all", all), (field.as_str(), "balances")],
        )
        .await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY, "{all}");
    }
    assert_eq!(server.store.list_agent_tokens(None).await.unwrap().len(), 0);
}

/// Choosing one account and leaving All accounts alone is the usual way to
/// fill the form. At the same level, All accounts already gives the account
/// that level, so it would look restricted and reach every account.
#[tokio::test]
async fn account_level_equal_to_all_is_refused_when_creating() {
    let (server, mut admin, _, checking, _) = with_accounts().await;
    let field = format!("account_{checking}");
    for (scope, all, level) in [
        ("write", "write", "write"),
        ("write", "balances", "balances"),
        // All accounts is capped at what read allows: transactions.
        ("read", "write", "transactions"),
    ] {
        let refused = post_token_form(
            &mut admin,
            "looks-limited",
            scope,
            &[("all", all), (field.as_str(), level)],
        )
        .await;
        assert_eq!(
            refused.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{scope} {all} {level}"
        );
        assert!(
            refused.body.contains("set All accounts to none"),
            "the error says what to do"
        );
    }
    assert_eq!(server.store.list_agent_tokens(None).await.unwrap().len(), 0);

    // The form says so before anyone tries.
    let page = admin.get("/account").await;
    let form = page.between("action=\"/account/tokens\"", "</form>");
    assert!(
        form.contains("set All accounts to none"),
        "the help text says how to limit a token"
    );
}

/// A uniform token's edit form opens at All accounts at the ceiling, so the
/// same trap is refused there, and a restricted token keeps its grants.
#[tokio::test]
async fn account_level_equal_to_all_is_refused_when_editing() {
    let (server, mut admin, alice, checking, savings) = with_accounts().await;
    let field = format!("account_{checking}");
    let restricted = vec![TokenGrant {
        target: GrantTarget::Account(savings),
        level: AccountLevel::Balances,
    }];
    let tokens = [
        (Scope::Write, Vec::new(), "write", "write"),
        (Scope::Read, Vec::new(), "transactions", "transactions"),
        (Scope::Write, restricted, "balances", "balances"),
    ];
    for (scope, grants, all, level) in tokens {
        let id = grants_token(&server, &alice, scope, grants.clone()).await;
        let path = format!("/account/tokens/{id}/grants");
        let page = admin.get("/account").await;
        let form = page.between(&format!("action=\"{path}\""), "</form>");
        assert!(
            form.contains("set All accounts to none"),
            "the help text says how to limit a token"
        );
        let refused = admin
            .post_form(
                &path,
                &[
                    ("csrf", &page.csrf()),
                    ("all", all),
                    (field.as_str(), level),
                ],
            )
            .await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY, "{all}");
        assert!(refused.body.contains("set All accounts to none"));
        assert_eq!(
            server.store.token_grants(id).await.unwrap(),
            grants,
            "the grants stay"
        );
    }
}

#[tokio::test]
async fn bad_grant_fields_are_refused_and_never_echoed() {
    let (server, mut admin, alice, checking, _) = with_accounts().await;
    let marker = "zqmarker";
    let good = format!("account_{checking}");
    let unknown = format!("account_{}", AccountId::new());
    let bad_id = format!("account_{marker}");
    let bad_field = format!("{marker}_field");
    let cases: [Vec<(&str, &str)>; 5] = [
        vec![("all", marker)],
        vec![("all", "none"), (good.as_str(), marker)],
        vec![("all", "none"), (bad_id.as_str(), "write")],
        vec![("all", "none"), (bad_field.as_str(), "write")],
        vec![("all", "none"), (unknown.as_str(), "write")],
    ];
    let id = grants_token(
        &server,
        &alice,
        Scope::Write,
        vec![TokenGrant {
            target: GrantTarget::Account(checking),
            level: AccountLevel::Balances,
        }],
    )
    .await;
    for fields in &cases {
        let refused = post_token_form(&mut admin, "bad", "write", fields).await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY, "create");
        assert!(!refused.body.contains(marker), "no echo from create");
        assert!(
            refused
                .header(header::CONTENT_TYPE)
                .starts_with("text/html"),
            "create answers with a page"
        );

        let csrf = admin.get("/account").await.csrf();
        let mut form = vec![("csrf", csrf.as_str())];
        form.extend_from_slice(fields);
        let refused = admin
            .post_form(&format!("/account/tokens/{id}/grants"), &form)
            .await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY, "edit");
        assert!(!refused.body.contains(marker), "no echo from edit");
    }
    assert_eq!(server.store.list_agent_tokens(None).await.unwrap().len(), 1);
    assert_eq!(
        server.store.token_grants(id).await.unwrap(),
        [TokenGrant {
            target: GrantTarget::Account(checking),
            level: AccountLevel::Balances,
        }],
        "the edit changed nothing"
    );

    // A level above the scope, on a form for a read token.
    let refused = post_token_form(
        &mut admin,
        "bad",
        "read",
        &[("all", "none"), (good.as_str(), "write")],
    )
    .await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    let refused = post_token_form(
        &mut admin,
        "bad",
        "read",
        &[("all", "write"), (good.as_str(), "write")],
    )
    .await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn grants_edit_requires_csrf() {
    let (server, mut admin, alice, checking, _) = with_accounts().await;
    let original = vec![TokenGrant {
        target: GrantTarget::Account(checking),
        level: AccountLevel::Balances,
    }];
    let id = grants_token(&server, &alice, Scope::Write, original.clone()).await;
    let path = format!("/account/tokens/{id}/grants");
    for fields in [
        vec![("all", "write")],
        vec![("csrf", "forged"), ("all", "write")],
    ] {
        let refused = admin.post_form(&path, &fields).await;
        assert_eq!(refused.status, StatusCode::FORBIDDEN);
    }
    assert_eq!(server.store.token_grants(id).await.unwrap(), original);
}

#[tokio::test]
async fn grants_edit_form_changes_access() {
    let (server, mut admin, alice, checking, savings) = with_accounts().await;
    let (record, secret) = server
        .agent_tokens
        .create(
            &alice,
            TokenName::parse("editable").unwrap(),
            Scope::Read,
            None,
            vec![TokenGrant {
                target: GrantTarget::Account(checking),
                level: AccountLevel::Transactions,
            }],
        )
        .await
        .unwrap();
    let id = record.id;
    let path = format!("/account/tokens/{id}/grants");
    let status_of = |account: AccountId| {
        Request::builder()
            .uri(format!("/api/v1/accounts/{account}"))
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", secret.expose_secret()),
            )
            .body(Body::empty())
            .unwrap()
    };
    assert_eq!(
        admin.send(status_of(savings)).await.status,
        StatusCode::NOT_FOUND
    );

    // The edit form posts here, holds the current levels, and offers nothing
    // above what a read token may have.
    let page = admin.get("/account").await;
    let form = page.between(&format!("action=\"{path}\""), "</form>");
    assert!(form.contains("name=\"csrf\""), "the form carries the token");
    let current = form[form.find(&format!("name=\"account_{checking}\"")).unwrap()..].to_owned();
    assert!(
        current.contains("value=\"transactions\" selected"),
        "the current level is selected"
    );
    assert!(
        !form.contains("value=\"write\""),
        "a read token is offered no write"
    );

    let csrf = page.csrf();
    let savings_field = format!("account_{savings}");
    let moved = admin
        .post_form(
            &path,
            &[
                ("csrf", &csrf),
                ("all", "none"),
                (savings_field.as_str(), "balances"),
            ],
        )
        .await;
    assert_eq!(moved.status, StatusCode::SEE_OTHER);
    assert_eq!(moved.header(header::LOCATION), "/account");
    assert_eq!(
        admin.send(status_of(savings)).await.status,
        StatusCode::OK,
        "newly granted"
    );
    assert_eq!(
        admin.send(status_of(checking)).await.status,
        StatusCode::NOT_FOUND,
        "no longer granted"
    );

    // An edit to nothing is refused, not read as no restriction at all.
    let refused = admin
        .post_form(&path, &[("csrf", &csrf), ("all", "none")])
        .await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        admin.send(status_of(checking)).await.status,
        StatusCode::NOT_FOUND,
        "still restricted"
    );

    // Widening is an explicit `all`, kept as a grant.
    let widened = admin
        .post_form(&path, &[("csrf", &csrf), ("all", "transactions")])
        .await;
    assert_eq!(widened.status, StatusCode::SEE_OTHER);
    assert_eq!(
        server.store.token_grants(id).await.unwrap(),
        [TokenGrant {
            target: GrantTarget::All,
            level: AccountLevel::Transactions,
        }]
    );
    assert_eq!(admin.send(status_of(checking)).await.status, StatusCode::OK);
}

#[tokio::test]
async fn grants_edit_of_another_members_token_is_refused() {
    let (server, mut admin, alice, checking, _) = with_accounts().await;
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
    let original = vec![TokenGrant {
        target: GrantTarget::Account(checking),
        level: AccountLevel::Balances,
    }];
    let id = grants_token(&server, &alice, Scope::Read, original.clone()).await;
    let path = format!("/account/tokens/{id}/grants");

    let csrf = bob.get("/account").await.csrf();
    let refused = bob
        .post_form(&path, &[("csrf", &csrf), ("all", "transactions")])
        .await;
    assert_eq!(refused.status, StatusCode::NOT_FOUND);
    let missing = bob
        .post_form(
            &format!("/account/tokens/{}/grants", AgentTokenId::new()),
            &[("csrf", &csrf), ("all", "transactions")],
        )
        .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    assert_eq!(
        refused.body, missing.body,
        "the same answer as a missing token"
    );
    assert_eq!(server.store.token_grants(id).await.unwrap(), original);

    // Unparsable ids answer the same.
    let garbled = bob
        .post_form(
            "/account/tokens/not-an-id/grants",
            &[("csrf", &csrf), ("all", "transactions")],
        )
        .await;
    assert_eq!(garbled.status, StatusCode::NOT_FOUND);

    // A member's page never shows another's token; an admin may change it.
    assert!(!bob.get("/account").await.body.contains(&path));
    let bob_actor = server
        .sessions
        .resolve(bob.cookie.as_deref().unwrap())
        .await
        .unwrap();
    let bob_token = grants_token(&server, &bob_actor, Scope::Read, original).await;
    let csrf = admin.get("/account").await.csrf();
    let changed = admin
        .post_form(
            &format!("/account/tokens/{bob_token}/grants"),
            &[("csrf", &csrf), ("all", "balances")],
        )
        .await;
    assert_eq!(changed.status, StatusCode::SEE_OTHER);
    assert_eq!(
        server.store.token_grants(bob_token).await.unwrap(),
        [TokenGrant {
            target: GrantTarget::All,
            level: AccountLevel::Balances,
        }]
    );
}

/// A grant outlives the account's closing, so the edit form must keep it:
/// without a select, saving would silently take it away. A closed account
/// no token asked for is not offered.
#[tokio::test]
async fn closed_accounts_keep_their_select_only_when_granted() {
    let (server, mut admin, alice, checking, savings) = with_accounts().await;
    let id = grants_token(
        &server,
        &alice,
        Scope::Write,
        vec![TokenGrant {
            target: GrantTarget::Account(checking),
            level: AccountLevel::Balances,
        }],
    )
    .await;
    for account in [checking, savings] {
        server
            .ledger
            .update_account(
                &alice,
                account,
                AccountPatch {
                    closed: Some(true),
                    ..AccountPatch::default()
                },
            )
            .await
            .unwrap();
    }
    let page = admin.get("/account").await;
    let edit = page.between(
        &format!("action=\"/account/tokens/{id}/grants\""),
        "</form>",
    );
    assert!(
        edit.contains(&format!("name=\"account_{checking}\"")),
        "the granted closed account stays editable"
    );
    assert!(
        !edit.contains(&format!("name=\"account_{savings}\"")),
        "an ungranted closed account is not offered"
    );
    let create = page.between("action=\"/account/tokens\"", "</form>");
    assert!(
        !create.contains("name=\"account_"),
        "the token form lists open accounts only"
    );
}

#[tokio::test]
async fn revoked_tokens_offer_no_edit_and_refuse_one() {
    let (server, mut admin, alice, checking, _) = with_accounts().await;
    let original = vec![TokenGrant {
        target: GrantTarget::Account(checking),
        level: AccountLevel::Balances,
    }];
    let id = grants_token(&server, &alice, Scope::Read, original.clone()).await;
    let path = format!("/account/tokens/{id}/grants");
    assert!(admin.get("/account").await.body.contains(&path));
    server.agent_tokens.revoke(&alice, id).await.unwrap();

    let page = admin.get("/account").await;
    assert!(!page.body.contains(&path), "no form for a revoked token");
    let refused = admin
        .post_form(&path, &[("csrf", &page.csrf()), ("all", "balances")])
        .await;
    assert_eq!(refused.status, StatusCode::CONFLICT);
    assert_eq!(server.store.token_grants(id).await.unwrap(), original);
}

#[tokio::test]
async fn account_names_in_grant_forms_are_escaped() {
    let (server, mut admin, alice, _, _) = with_accounts().await;
    new_account(&server, &alice, "<script>alert(1)</script>").await;
    let page = admin.get("/account").await;
    assert!(
        page.body.contains("&lt;script&gt;alert(1)"),
        "the name is shown"
    );
    assert!(!page.body.contains("<script>alert"), "and never as markup");
}
