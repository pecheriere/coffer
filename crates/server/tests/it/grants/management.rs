//! Setting, validating, auditing and reading back a token's grants.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use super::{World, grant, on, s, world};
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use coffer_domain::audit::{AuditValue, FieldChange};
use coffer_domain::identity::{Actor, Role, Scope, TokenName, Via};
use coffer_domain::ids::{AccountId, AgentTokenId};
use coffer_domain::policy::{AccountLevel, DenyReason, GrantError, GrantTarget, TokenGrant};
use coffer_domain::time::Clock as _;
use coffer_server::auth::AuthError;
use coffer_server::http::router;
use coffer_server::testkit::TEST_PUBLIC_URL;
use coffer_store::{AuditEvent, AuditFilter, PageRequest};
use secrecy::ExposeSecret as _;
use serde_json::json;
use std::collections::BTreeMap;
use tower::ServiceExt as _;

#[tokio::test]
async fn token_grants_are_validated() {
    let world = world().await;
    let a = world.account("Checking").await;
    let create = async |scope: Scope, grants: Vec<TokenGrant>| {
        world
            .server
            .agent_tokens
            .create(
                &world.alice,
                TokenName::parse("claude").unwrap(),
                scope,
                None,
                grants,
            )
            .await
    };
    assert!(matches!(
        create(Scope::Read, vec![on(a, AccountLevel::Write)]).await,
        Err(AuthError::InvalidGrants(GrantError::AboveScope))
    ));
    assert!(matches!(
        create(
            Scope::Write,
            vec![on(a, AccountLevel::Write), on(a, AccountLevel::Balances)]
        )
        .await,
        Err(AuthError::InvalidGrants(GrantError::DuplicateTarget))
    ));
    let unknown = create(
        Scope::Write,
        vec![on(AccountId::new(), AccountLevel::Write)],
    )
    .await;
    assert!(matches!(unknown, Err(AuthError::UnknownGrantedAccount)));
    assert_eq!(
        unknown.err().unwrap().to_string(),
        "a granted account does not exist"
    );
    let (id, _) = world
        .token(
            Scope::Write,
            vec![
                on(a, AccountLevel::Write),
                grant(GrantTarget::All, AccountLevel::Balances),
            ],
        )
        .await;
    assert_eq!(
        world.server.store.token_grants(id).await.unwrap(),
        [
            grant(GrantTarget::All, AccountLevel::Balances),
            on(a, AccountLevel::Write),
        ]
    );
    let events = world
        .server
        .store
        .list_audit(
            &AuditFilter {
                entity_id: Some(s(id)),
                ..AuditFilter::default()
            },
            &PageRequest::default(),
        )
        .await
        .unwrap()
        .items;
    let (_, recorded) = events[0]
        .changes
        .iter()
        .find(|(field, _)| *field == "grants")
        .unwrap();
    assert_eq!(
        recorded.after,
        AuditValue::from(format!("all=balances,{a}=write").as_str()),
        "the audit entry holds the canonical grants"
    );
}

#[tokio::test]
async fn a_grant_that_no_longer_parses_fails_closed() {
    let world = world().await;
    let (a, b) = (
        world.account("Checking").await,
        world.account("Hidden").await,
    );
    for sql in [
        // A level the schema refuses.
        "INSERT INTO agent_token_grants (token_id, target, level) VALUES (?, 'all', 'admin')",
        // A target of the right length that is no account id.
        "INSERT INTO agent_token_grants (token_id, target, level) \
         VALUES (?, 'xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx', 'write')",
    ] {
        let (id, token) = world
            .token(Scope::Read, vec![on(a, AccountLevel::Balances)])
            .await;
        world
            .server
            .store
            .execute_unchecked_for_tests(sql, &s(id))
            .await
            .unwrap();
        let (status, text) = world
            .send(Method::GET, "/api/v1/accounts", &token, None, None)
            .await;
        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "a corrupt grant refuses the request"
        );
        assert!(
            !text.contains(&s(a)) && !text.contains(&s(b)),
            "the refusal names no account"
        );
    }
}

#[tokio::test]
async fn grants_go_with_their_token() {
    let world = world().await;
    let a = world.account("Checking").await;
    let (id, _) = world
        .token(Scope::Read, vec![on(a, AccountLevel::Balances)])
        .await;
    assert_eq!(world.server.store.token_grants(id).await.unwrap().len(), 1);
    world
        .server
        .store
        .execute_unchecked_for_tests("DELETE FROM agent_tokens WHERE id = ?", &s(id))
        .await
        .unwrap();
    let left = world.server.store.token_grants(id).await.unwrap();
    assert_eq!(left, Vec::<TokenGrant>::new(), "the grants are gone");
}

impl World {
    /// Member "bob", signed in with a passkey.
    async fn bob(&self) -> Actor {
        let invite = self
            .server
            .identity
            .create_invite(&self.alice, Role::Member, None)
            .await
            .unwrap();
        let member = self
            .server
            .authenticator()
            .enroll(&invite, "bob", "Bob")
            .await;
        Actor {
            member,
            role: Role::Member,
            via: Via::Session,
        }
    }

    /// The audit events of `token` with `action`, newest first.
    async fn token_events(&self, token: AgentTokenId, action: &str) -> Vec<AuditEvent> {
        self.server
            .store
            .list_audit(
                &AuditFilter {
                    entity_id: Some(s(token)),
                    ..AuditFilter::default()
                },
                &PageRequest::default(),
            )
            .await
            .unwrap()
            .items
            .into_iter()
            .filter(|event| event.action == action)
            .collect()
    }
}

/// The `grants` change an audit event recorded.
fn grants_change(event: &AuditEvent) -> &FieldChange {
    event
        .changes
        .iter()
        .find_map(|(field, change)| (field == "grants").then_some(change))
        .unwrap()
}

#[tokio::test]
async fn grants_change_audited_and_session_only() {
    let world = world().await;
    let a = world.account("Checking").await;
    let b = world.account("Savings").await;
    let (id, secret) = world
        .token(
            Scope::Write,
            vec![grant(GrantTarget::All, AccountLevel::Balances)],
        )
        .await;

    // An agent cannot change grants, not even its own token's; through the
    // router its bearer token gets it no page route either.
    let agent = world
        .server
        .agent_tokens
        .resolve(secret.expose_secret())
        .await
        .unwrap();
    let refused = world
        .server
        .agent_tokens
        .replace_grants(
            &agent,
            id,
            vec![grant(GrantTarget::All, AccountLevel::Write)],
        )
        .await;
    assert!(
        matches!(
            refused,
            Err(AuthError::Denied(denied)) if denied.reason == DenyReason::SessionRequired
        ),
        "an agent is refused"
    );
    let response = router(world.server.app_state())
        .oneshot(
            Request::post(format!("/account/tokens/{id}/grants"))
                .header(header::ORIGIN, TEST_PUBLIC_URL)
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", secret.expose_secret()),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::SEE_OTHER,
        "from the right origin, a bearer token is still no session"
    );
    assert_eq!(response.headers()[header::LOCATION], "/login");
    assert_eq!(
        world.server.store.token_grants(id).await.unwrap(),
        [grant(GrantTarget::All, AccountLevel::Balances)],
        "refused attempts change nothing"
    );
    assert_eq!(world.token_events(id, "token.grants").await.len(), 0);

    // A session can, and the next request of the token obeys.
    world
        .server
        .agent_tokens
        .replace_grants(
            &world.alice,
            id,
            vec![on(b, AccountLevel::Write), on(a, AccountLevel::Balances)],
        )
        .await
        .unwrap();
    let (status, _) = world
        .get(&format!("/api/v1/accounts/{a}/balances"), &secret)
        .await;
    assert_eq!(status, StatusCode::OK);
    let new = world.account("New").await;
    let (status, _) = world.get(&format!("/api/v1/accounts/{new}"), &secret).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "the all grant is gone");

    let events = world.token_events(id, "token.grants").await;
    assert_eq!(events.len(), 1, "one audit entry");
    let (first, second) = if a < b { (a, b) } else { (b, a) };
    let level = |account: AccountId| if account == a { "balances" } else { "write" };
    assert_eq!(
        *grants_change(&events[0]),
        FieldChange {
            before: AuditValue::from("all=balances"),
            after: AuditValue::from(
                format!("{first}={},{second}={}", level(first), level(second)).as_str()
            ),
        },
        "the entry holds both canonical forms"
    );
}

#[tokio::test]
async fn granting_a_uniform_token_records_no_before() {
    let world = world().await;
    let (id, _) = world.token(Scope::Read, Vec::new()).await;
    world
        .server
        .agent_tokens
        .replace_grants(
            &world.alice,
            id,
            vec![grant(GrantTarget::All, AccountLevel::Balances)],
        )
        .await
        .unwrap();
    let events = world.token_events(id, "token.grants").await;
    assert_eq!(
        *grants_change(&events[0]),
        FieldChange {
            before: AuditValue::Null,
            after: AuditValue::from("all=balances"),
        },
        "a token without grants has nothing before"
    );
}

/// No grant rows means uniform access, so emptying the list would silently
/// widen a restricted token to everything its scope allows.
#[tokio::test]
async fn emptying_grants_is_refused_and_never_widens() {
    let world = world().await;
    let a = world.account("Checking").await;
    let b = world.account("Savings").await;
    let (id, secret) = world
        .token(Scope::Write, vec![on(a, AccountLevel::Balances)])
        .await;

    let refused = world
        .server
        .agent_tokens
        .replace_grants(&world.alice, id, Vec::new())
        .await;
    assert!(
        matches!(refused, Err(AuthError::InvalidGrants(GrantError::Empty))),
        "an empty list is refused"
    );
    assert_eq!(
        world.server.store.token_grants(id).await.unwrap(),
        [on(a, AccountLevel::Balances)],
        "the grants stay"
    );
    let (status, _) = world.get(&format!("/api/v1/accounts/{b}"), &secret).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "still hidden");
    assert_eq!(world.token_events(id, "token.grants").await.len(), 0);

    // Widening stays possible, and it is written down: an explicit `all`.
    world
        .server
        .agent_tokens
        .replace_grants(
            &world.alice,
            id,
            vec![grant(GrantTarget::All, AccountLevel::Write)],
        )
        .await
        .unwrap();
    let (status, _) = world.get(&format!("/api/v1/accounts/{b}"), &secret).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn replaced_grants_are_validated_like_new_ones() {
    let world = world().await;
    let a = world.account("Checking").await;
    let (id, _) = world
        .token(Scope::Read, vec![on(a, AccountLevel::Balances)])
        .await;
    let replace = async |grants: Vec<TokenGrant>| {
        world
            .server
            .agent_tokens
            .replace_grants(&world.alice, id, grants)
            .await
    };

    assert!(matches!(
        replace(vec![on(a, AccountLevel::Write)]).await,
        Err(AuthError::InvalidGrants(GrantError::AboveScope))
    ));
    assert!(matches!(
        replace(vec![
            on(a, AccountLevel::Balances),
            on(a, AccountLevel::Transactions)
        ])
        .await,
        Err(AuthError::InvalidGrants(GrantError::DuplicateTarget))
    ));
    let many = (0..=coffer_domain::policy::MAX_GRANTS)
        .map(|_| on(AccountId::new(), AccountLevel::Balances))
        .collect();
    assert!(matches!(
        replace(many).await,
        Err(AuthError::InvalidGrants(GrantError::TooMany))
    ));
    assert!(matches!(
        replace(vec![on(AccountId::new(), AccountLevel::Balances)]).await,
        Err(AuthError::UnknownGrantedAccount)
    ));

    assert_eq!(
        world.server.store.token_grants(id).await.unwrap(),
        [on(a, AccountLevel::Balances)],
        "nothing changed"
    );
    assert_eq!(world.token_events(id, "token.grants").await.len(), 0);
}

#[tokio::test]
async fn another_members_token_is_hidden_unless_managing_members() {
    let world = world().await;
    let a = world.account("Checking").await;
    let bob = world.bob().await;
    let (alice_token, _) = world
        .token(Scope::Read, vec![on(a, AccountLevel::Balances)])
        .await;
    let (bob_token, _) = world
        .server
        .agent_tokens
        .create(
            &bob,
            TokenName::parse("bobs").unwrap(),
            Scope::Read,
            None,
            vec![on(a, AccountLevel::Balances)],
        )
        .await
        .map(|(record, secret)| (record.id, secret))
        .unwrap();
    let wide = vec![grant(GrantTarget::All, AccountLevel::Transactions)];

    // Bob gets what a missing token gets, whether or not his list is valid.
    for grants in [wide.clone(), Vec::new()] {
        for token in [alice_token, AgentTokenId::new()] {
            assert!(
                matches!(
                    world
                        .server
                        .agent_tokens
                        .replace_grants(&bob, token, grants.clone())
                        .await,
                    Err(AuthError::NotFound)
                ),
                "hidden like a missing token"
            );
        }
    }
    assert_eq!(
        world.server.store.token_grants(alice_token).await.unwrap(),
        [on(a, AccountLevel::Balances)]
    );

    // His own token and, as an admin, anyone's.
    world
        .server
        .agent_tokens
        .replace_grants(&bob, bob_token, vec![on(a, AccountLevel::Transactions)])
        .await
        .unwrap();
    world
        .server
        .agent_tokens
        .replace_grants(&world.alice, bob_token, wide.clone())
        .await
        .unwrap();
    assert_eq!(
        world.server.store.token_grants(bob_token).await.unwrap(),
        wide
    );
}

#[tokio::test]
async fn revoked_and_expired_tokens_keep_their_grants() {
    let world = world().await;
    let a = world.account("Checking").await;
    let wide = vec![grant(GrantTarget::All, AccountLevel::Balances)];

    let (revoked, _) = world
        .token(Scope::Read, vec![on(a, AccountLevel::Balances)])
        .await;
    world
        .server
        .agent_tokens
        .revoke(&world.alice, revoked)
        .await
        .unwrap();

    // `create` always makes a new token: one born expired stores its grants
    // and can never be used, so grants cannot revive anything.
    let past = world
        .server
        .clock
        .now()
        .checked_sub(jiff::SignedDuration::from_hours(1))
        .unwrap();
    let (expired, secret) = world
        .server
        .agent_tokens
        .create(
            &world.alice,
            TokenName::parse("late").unwrap(),
            Scope::Read,
            Some(past),
            vec![on(a, AccountLevel::Balances)],
        )
        .await
        .unwrap();
    assert!(matches!(
        world
            .server
            .agent_tokens
            .resolve(secret.expose_secret())
            .await,
        Err(AuthError::Unauthenticated)
    ));

    for token in [revoked, expired.id] {
        assert!(
            matches!(
                world
                    .server
                    .agent_tokens
                    .replace_grants(&world.alice, token, wide.clone())
                    .await,
                Err(AuthError::TokenInactive)
            ),
            "a token that no longer works takes no new grants"
        );
        assert_eq!(
            world.server.store.token_grants(token).await.unwrap(),
            [on(a, AccountLevel::Balances)]
        );
        assert_eq!(world.token_events(token, "token.grants").await.len(), 0);
    }
}

#[tokio::test]
async fn replacing_grants_with_the_same_ones_records_nothing() {
    let world = world().await;
    let a = world.account("Checking").await;
    let (id, _) = world
        .token(
            Scope::Write,
            vec![
                on(a, AccountLevel::Write),
                grant(GrantTarget::All, AccountLevel::Balances),
            ],
        )
        .await;
    world
        .server
        .agent_tokens
        .replace_grants(
            &world.alice,
            id,
            vec![
                grant(GrantTarget::All, AccountLevel::Balances),
                on(a, AccountLevel::Write),
            ],
        )
        .await
        .unwrap();
    assert_eq!(
        world.token_events(id, "token.grants").await.len(),
        0,
        "the same grants in another order change nothing"
    );
}

#[tokio::test]
async fn me_reports_access() {
    let world = world().await;
    let a = world.account("Checking").await;
    let b = world.account("Savings").await;

    // Uniform: everything at the scope's level, no account listed.
    let (_, uniform) = world.token(Scope::Read, Vec::new()).await;
    let (status, body) = world.get("/api/v1/me", &uniform).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["access"],
        json!({"ceiling": "transactions", "all": "transactions", "accounts": []})
    );

    // Restricted: only what the grants name.
    let (_, restricted) = world
        .token(Scope::Write, vec![on(a, AccountLevel::Balances)])
        .await;
    let (_, body) = world.get("/api/v1/me", &restricted).await;
    assert_eq!(
        body["access"],
        json!({
            "ceiling": "write",
            "all": null,
            "accounts": [{"account": s(a), "level": "balances"}],
        })
    );

    // With an `all` grant, a listed account shows the level it really has.
    let (_, both) = world
        .token(
            Scope::Write,
            vec![
                grant(GrantTarget::All, AccountLevel::Transactions),
                on(b, AccountLevel::Write),
                on(a, AccountLevel::Balances),
            ],
        )
        .await;
    let (_, body) = world.get("/api/v1/me", &both).await;
    assert_eq!(body["access"]["all"], "transactions");
    let levels: BTreeMap<String, String> = body["access"]["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|grant| {
            (
                grant["account"].as_str().unwrap().to_owned(),
                grant["level"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        levels,
        BTreeMap::from([
            (s(a), "transactions".to_owned()),
            (s(b), "write".to_owned()),
        ])
    );
}
