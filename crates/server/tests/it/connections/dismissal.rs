//! Dismissing an unbound provider account and taking that back: audited, refused to who may not
//! write, kept through syncs, and ended by binding.

use axum::http::{Method, StatusCode};
use coffer_domain::accounts::AccountKind;
use coffer_domain::audit::AuditActor;
use coffer_domain::identity::{Scope, TokenName};
use coffer_domain::ids::ProviderAccountId;
use coffer_domain::money::DecimalAmount;
use coffer_domain::policy::{AccountLevel, GrantTarget, TokenGrant};
use coffer_server::sync::SyncError;
use coffer_server::sync::connections::NewLinkedAccountInput;
use coffer_server::sync::provider::ProviderAccountData;
use jiff::civil::date;
use serde_json::json;

use super::{World, reported, txn, world};

fn dismiss_uri(id: ProviderAccountId) -> String {
    format!("/api/v1/provider-accounts/{id}/dismiss")
}

fn restore_uri(id: ProviderAccountId) -> String {
    format!("/api/v1/provider-accounts/{id}/restore")
}

async fn with_one_held() -> (World, ProviderAccountId) {
    let world = world().await;
    world
        .sync_once(vec![reported("acct-1", "USD", None, Vec::new())])
        .await;
    let held = world.provider_account("acct-1").await.id;
    (world, held)
}

#[tokio::test]
async fn dismissing_and_restoring_are_audited_and_reversible() {
    let (world, held) = with_one_held().await;

    let dismissed = world
        .server
        .sync
        .dismiss_provider_account(&world.alice, held)
        .await
        .unwrap();
    assert!(dismissed.dismissed_at.is_some());
    assert!(dismissed.account.is_none(), "dismissing binds nothing");
    assert_eq!(
        world
            .server
            .sync
            .count_provider_accounts_to_set_up(&world.alice)
            .await
            .unwrap(),
        0,
        "a dismissed account is not one to set up"
    );

    let restored = world
        .server
        .sync
        .restore_provider_account(&world.alice, held)
        .await
        .unwrap();
    assert!(restored.dismissed_at.is_none());
    assert_eq!(
        world
            .server
            .sync
            .count_provider_accounts_to_set_up(&world.alice)
            .await
            .unwrap(),
        1
    );

    let member = AuditActor::Member {
        member: world.alice.member,
        agent: None,
    };
    for action in ["provider_account.dismiss", "provider_account.restore"] {
        let events = world.audit_actions(action).await;
        assert_eq!(events.len(), 1, "{action} is audited once");
        assert_eq!(events[0].entity_id, held.to_string());
        assert_eq!(events[0].actor, member);
    }
}

#[tokio::test]
async fn a_write_token_does_it_through_the_api_and_the_audit_names_the_agent() {
    let (world, held) = with_one_held().await;
    let writer = world.token(Scope::Write).await;

    let (status, body) = world
        .call(Method::POST, &dismiss_uri(held), &writer, None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["dismissed_at"].is_string());
    let (_, listed) = world
        .call(
            Method::GET,
            "/api/v1/provider-accounts?unbound=true",
            &writer,
            None,
        )
        .await;
    assert_eq!(listed["items"][0]["id"], json!(held.to_string()));
    assert!(
        listed["items"][0]["dismissed_at"].is_string(),
        "it stays listed, marked"
    );

    let (status, body) = world
        .call(Method::POST, &restore_uri(held), &writer, None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["dismissed_at"].is_null());

    for action in ["provider_account.dismiss", "provider_account.restore"] {
        let events = world.audit_actions(action).await;
        assert!(
            matches!(events[0].actor, AuditActor::Member { agent: Some(_), .. }),
            "{action} names the agent"
        );
    }
}

#[tokio::test]
async fn a_read_token_is_refused_and_changes_nothing() {
    let (world, held) = with_one_held().await;
    let reader = world.token(Scope::Read).await;

    for uri in [dismiss_uri(held), restore_uri(held)] {
        let (status, _) = world.call(Method::POST, &uri, &reader, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    assert!(
        world
            .provider_account("acct-1")
            .await
            .dismissed_at
            .is_none()
    );
    assert_eq!(
        world.audit_actions("provider_account.dismiss").await.len(),
        0
    );
}

#[tokio::test]
async fn a_token_that_cannot_see_it_gets_the_answer_for_a_missing_one() {
    let (world, held) = with_one_held().await;
    let checking = world.account("Checking", "USD").await;
    let (_, narrow) = world
        .server
        .agent_tokens
        .create(
            &world.alice,
            TokenName::parse("narrow").unwrap(),
            Scope::Write,
            None,
            vec![TokenGrant {
                target: GrantTarget::Account(checking),
                level: AccountLevel::Write,
            }],
        )
        .await
        .unwrap();

    for uri in [dismiss_uri, restore_uri] {
        let real = world.call(Method::POST, &uri(held), &narrow, None).await;
        let missing = world
            .call(Method::POST, &uri(ProviderAccountId::new()), &narrow, None)
            .await;
        assert_eq!(real, missing);
    }
    assert!(
        world
            .provider_account("acct-1")
            .await
            .dismissed_at
            .is_none()
    );
}

#[tokio::test]
async fn an_unknown_one_is_missing() {
    let world = world().await;
    let err = world
        .server
        .sync
        .dismiss_provider_account(&world.alice, ProviderAccountId::new())
        .await
        .unwrap_err();
    assert!(matches!(err, SyncError::NotFound));
}

#[tokio::test]
async fn a_stale_request_is_a_conflict() {
    let (world, held) = with_one_held().await;
    let sync = &world.server.sync;

    assert!(matches!(
        sync.restore_provider_account(&world.alice, held).await,
        Err(SyncError::Conflict(_))
    ));
    sync.dismiss_provider_account(&world.alice, held)
        .await
        .unwrap();
    assert!(matches!(
        sync.dismiss_provider_account(&world.alice, held).await,
        Err(SyncError::Conflict(_))
    ));

    let checking = world.account("Checking", "USD").await;
    sync.bind(&world.alice, held, checking).await.unwrap();
    assert!(matches!(
        sync.dismiss_provider_account(&world.alice, held).await,
        Err(SyncError::Conflict(_))
    ));
    assert!(matches!(
        sync.restore_provider_account(&world.alice, held).await,
        Err(SyncError::Conflict(_))
    ));
}

#[tokio::test]
async fn a_removed_connections_account_cannot_be_dismissed() {
    let (world, held) = with_one_held().await;
    world
        .server
        .sync
        .remove_connection(&world.alice, world.connection)
        .await
        .unwrap();
    let err = world
        .server
        .sync
        .dismiss_provider_account(&world.alice, held)
        .await;
    assert!(matches!(err, Err(SyncError::Conflict(_))));
}

#[tokio::test]
async fn binding_a_dismissed_account_ends_the_dismissal() {
    let (world, held) = with_one_held().await;
    world
        .server
        .sync
        .dismiss_provider_account(&world.alice, held)
        .await
        .unwrap();
    let checking = world.account("Checking", "USD").await;

    let bound = world
        .server
        .sync
        .bind(&world.alice, held, checking)
        .await
        .unwrap();

    assert!(bound.dismissed_at.is_none());
    assert!(
        world
            .provider_account("acct-1")
            .await
            .dismissed_at
            .is_none()
    );
}

#[tokio::test]
async fn creating_an_account_for_a_dismissed_one_ends_the_dismissal() {
    let world = world().await;
    world
        .sync_once(vec![reported(
            "acct-1",
            "USD",
            Some(AccountKind::Savings),
            Vec::new(),
        )])
        .await;
    let held = world.provider_account("acct-1").await.id;
    world
        .server
        .sync
        .dismiss_provider_account(&world.alice, held)
        .await
        .unwrap();

    let linked = world
        .server
        .sync
        .create_account_and_bind(
            &world.alice,
            held,
            NewLinkedAccountInput {
                name: None,
                kind: None,
                liquidity: None,
                include_in_net_worth: None,
                include_in_forecast: None,
                belongs_to: None,
                beneficiary: None,
            },
        )
        .await
        .unwrap();

    assert!(linked.provider_account.dismissed_at.is_none());
}

#[tokio::test]
async fn syncing_keeps_a_dismissed_account_dismissed_and_tracks_its_report() {
    let (world, held) = with_one_held().await;
    world
        .server
        .sync
        .dismiss_provider_account(&world.alice, held)
        .await
        .unwrap();
    let before = world.provider_account("acct-1").await;

    world
        .sync_once(vec![ProviderAccountData {
            balance: Some((date(2027, 1, 10), DecimalAmount::parse("12.34").unwrap())),
            name: "Renamed".into(),
            transactions: vec![txn("t-1", "-4.50")],
            ..reported("acct-1", "USD", None, Vec::new())
        }])
        .await;

    let after = world.provider_account("acct-1").await;
    assert_eq!(after.dismissed_at, before.dismissed_at, "still dismissed");
    assert_eq!(after.name, "Renamed", "it tracks the report");
    assert_eq!(
        after.last_balance.as_ref().map(|b| b.amount_raw.as_str()),
        Some("12.34")
    );
    assert!(after.account.is_none(), "and is still bound to nothing");
    let accounts = world
        .server
        .ledger
        .list_accounts(&world.alice, true)
        .await
        .unwrap();
    assert!(accounts.is_empty(), "no account appears for it");
}
