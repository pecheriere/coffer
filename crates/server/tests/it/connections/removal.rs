//! Removing a connection, including while a sync runs or the provider refuses.

use super::{PLAINTEXT, denied, reported, setup_token, txn, world};
use axum::http::StatusCode;
use coffer_domain::identity::{Actor, Role, Scope, Via};
use coffer_domain::policy::{Action, DenyReason};
use coffer_domain::sync::{SyncRunStatus, SyncTrigger};
use coffer_server::sync::SyncError;
use coffer_server::sync::provider::{FetchResult, ProviderError, ProviderKind};
use coffer_server::testkit::{TestServer, captured_log_text};
use coffer_store::ConnectionStatus;
use secrecy::ExposeSecret as _;
use std::sync::Arc;

#[tokio::test]
async fn remove_fails_cleanly_when_disconnect_fails() {
    let world = world().await;
    let before = world
        .server
        .store
        .get_connection(world.connection)
        .await
        .unwrap();
    world
        .fake
        .fail_next_disconnect(ProviderError::Unavailable("bridge down".into()));

    let err = world
        .server
        .sync
        .remove_connection(&world.alice, world.connection)
        .await
        .err()
        .unwrap();
    assert!(matches!(err, SyncError::Provider(_)));
    let problem = coffer_server::http::problem::Problem::from(err);
    assert_eq!(problem.status, StatusCode::BAD_GATEWAY);
    let unchanged = world
        .server
        .store
        .get_connection(world.connection)
        .await
        .unwrap()
        == before;
    assert!(unchanged);
    assert_eq!(world.audit_actions("connection.remove").await.len(), 0);

    // Once the provider answers, the removal goes through.
    world
        .server
        .sync
        .remove_connection(&world.alice, world.connection)
        .await
        .unwrap();
    let removed = world
        .server
        .store
        .get_connection(world.connection)
        .await
        .unwrap();
    assert_eq!(removed.status, ConnectionStatus::Removed);
    assert!(removed.credentials.is_none());
    let disconnects = world.fake.disconnects();
    assert_eq!(disconnects.len(), 2);
    let gave_up_stored = disconnects[1].expose_secret() == PLAINTEXT;
    assert!(gave_up_stored);
    assert_eq!(world.audit_actions("connection.remove").await.len(), 1);

    let err = world
        .server
        .sync
        .request_sync(Some(&world.alice), world.connection, SyncTrigger::Manual)
        .await
        .err()
        .unwrap();
    assert!(matches!(err, SyncError::Conflict(_)));
    let err = world
        .server
        .sync
        .remove_connection(&world.alice, world.connection)
        .await
        .err()
        .unwrap();
    assert!(matches!(err, SyncError::Conflict(_)));
}

#[tokio::test]
async fn member_cannot_remove_or_reconnect_others_connection() {
    let world = world().await;
    let invite = world
        .server
        .identity
        .create_invite(&world.alice, Role::Member, None)
        .await
        .unwrap();
    let bob_member = world
        .server
        .authenticator()
        .enroll(&invite, "bob", "Bob")
        .await;
    let bob = Actor {
        member: bob_member,
        role: Role::Member,
        via: Via::Session,
    };
    let before = world
        .server
        .store
        .get_connection(world.connection)
        .await
        .unwrap();
    let needs_admin = DenyReason::InsufficientScope {
        needed: Scope::Admin,
    };
    let action = Action::RemoveConnection {
        owner: world.alice.member,
    };

    let err = world
        .server
        .sync
        .reconnect(
            &bob,
            world.connection,
            setup_token("https://other@bridge.example"),
        )
        .await
        .err()
        .unwrap();
    assert!(denied(&err, action, needs_admin));
    let err = world
        .server
        .sync
        .remove_connection(&bob, world.connection)
        .await
        .err()
        .unwrap();
    assert!(denied(&err, action, needs_admin));
    let unchanged = world
        .server
        .store
        .get_connection(world.connection)
        .await
        .unwrap()
        == before;
    assert!(unchanged);
    assert_eq!(world.fake.disconnects().len(), 0);

    // Bob may remove a connection he added.
    let own = world
        .server
        .seed_connection(ProviderKind::SimpleFin, "Bob's bank", bob_member, PLAINTEXT)
        .await;
    world
        .server
        .sync
        .remove_connection(&bob, own)
        .await
        .unwrap();
    assert_eq!(
        world.server.store.get_connection(own).await.unwrap().status,
        ConnectionStatus::Removed
    );
}

/// A connection removed while a run is fetching gets nothing written: the
/// apply step re-checks the connection in the transaction that writes.
#[tokio::test]
async fn removal_during_a_sync_writes_nothing() {
    let world = world().await;
    let gate = world.fake.hold_next_fetch();
    world.fake.push(Ok(FetchResult {
        accounts: vec![reported("acct-1", "USD", None, vec![txn("t-1", "-4.50")])],
        connection_errors: Vec::new(),
        skipped: 0,
        cursor: None,
    }));
    let run = world
        .server
        .sync
        .request_sync(Some(&world.alice), world.connection, SyncTrigger::Manual)
        .await
        .unwrap();
    let sync = Arc::clone(&world.server.sync);
    let running = tokio::spawn(async move { sync.run(run.id).await });

    gate.entered().await;
    world
        .server
        .sync
        .remove_connection(&world.alice, world.connection)
        .await
        .unwrap();
    gate.release();
    let finished = running.await.unwrap().unwrap();

    assert_eq!(finished.status, SyncRunStatus::Failed);
    assert_eq!(
        finished.detail.as_deref(),
        Some("the connection is removed")
    );
    assert_eq!(world.provider_accounts().await.len(), 0);
    let connection = world
        .server
        .store
        .get_connection(world.connection)
        .await
        .unwrap();
    assert_eq!(connection.status, ConnectionStatus::Removed);
    assert_eq!(connection.last_success_at, None);
}

#[tokio::test]
async fn removal_proceeds_when_the_provider_already_refuses_the_credentials() {
    let world = world().await;
    world
        .fake
        .fail_next_disconnect(ProviderError::ReauthRequired);
    world
        .server
        .sync
        .remove_connection(&world.alice, world.connection)
        .await
        .unwrap();
    let stored = world
        .server
        .store
        .get_connection(world.connection)
        .await
        .unwrap();
    assert_eq!(stored.status, ConnectionStatus::Removed);
    assert!(stored.credentials.is_none());
    assert_eq!(world.fake.disconnects().len(), 1);
}

#[tokio::test]
async fn removal_proceeds_when_the_credentials_cannot_be_opened() {
    let world = world().await;
    let mut tx = world.server.store.begin().await.unwrap();
    tx.set_connection_credentials(world.connection, Some(vec![1, 2, 3]))
        .await
        .unwrap();
    tx.commit().await.unwrap();

    world
        .server
        .sync
        .remove_connection(&world.alice, world.connection)
        .await
        .unwrap();
    let stored = world
        .server
        .store
        .get_connection(world.connection)
        .await
        .unwrap();
    assert_eq!(stored.status, ConnectionStatus::Removed);
    assert!(stored.credentials.is_none());
    assert_eq!(world.fake.disconnects().len(), 0);
    assert!(captured_log_text().contains("its credentials cannot be opened"));
}

#[tokio::test]
async fn removal_proceeds_when_the_provider_is_not_offered() {
    let mut server = TestServer::new().await;
    server.offer_no_provider();
    let link = server.identity.bootstrap_link().await.unwrap().unwrap();
    let member = server.authenticator().enroll(&link, "alice", "Alice").await;
    let alice = Actor {
        member,
        role: Role::Admin,
        via: Via::Session,
    };
    let connection = server
        .seed_connection(ProviderKind::SimpleFin, "Family bank", member, PLAINTEXT)
        .await;

    server
        .sync
        .remove_connection(&alice, connection)
        .await
        .unwrap();
    let stored = server.store.get_connection(connection).await.unwrap();
    assert_eq!(stored.status, ConnectionStatus::Removed);
    assert!(stored.credentials.is_none());
    assert!(captured_log_text().contains("its provider is not offered"));
}
