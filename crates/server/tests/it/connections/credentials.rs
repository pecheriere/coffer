//! Credentials sealed at connect and reconnect, and given up when a connection
//! is replaced or fails.

use super::{PLAINTEXT, RENEWED, SECRET_PART, contains, reported, setup_token, txn, world};
use coffer_domain::accounts::ConnectionLabel;
use coffer_domain::sync::{SyncRunStatus, SyncTrigger};
use coffer_server::sync::SyncError;
use coffer_server::sync::provider::{FetchResult, ProviderError, ProviderKind};
use coffer_server::testkit::{INJECTED_FAILURE_LABEL, captured_log_text};
use coffer_store::{ConnectionStatus, ConnectionStatusChange, PageRequest};
use jiff::SignedDuration;
use secrecy::ExposeSecret as _;
use std::sync::Arc;

#[tokio::test]
async fn credentials_never_stored_in_clear() {
    let world = world().await;
    let view = world
        .server
        .sync
        .create_connection(
            &world.alice,
            ProviderKind::SimpleFin,
            ConnectionLabel::parse("Second bank").unwrap(),
            setup_token(PLAINTEXT),
            None,
        )
        .await
        .unwrap();
    assert_eq!(view.provider, "simplefin");
    assert_eq!(view.label, "Second bank");
    assert_eq!(view.owner, world.alice.member);
    assert_eq!(view.accounts.len(), 0);

    let record = world.server.store.get_connection(view.id).await.unwrap();
    let sealed = record.credentials.unwrap();
    assert!(!contains(&sealed, SECRET_PART));
    // Positive control: the sealed bytes do hold the credentials.
    assert!(world.opens_to(view.id, sealed, PLAINTEXT));
    assert!(!serde_json::to_string(&view).unwrap().contains(SECRET_PART));

    for event in world.audit().await {
        assert!(
            !serde_json::to_string(&event.changes)
                .unwrap()
                .contains(SECRET_PART)
        );
    }
    let creates = world.audit_actions("connection.create").await;
    assert_eq!(creates.len(), 1);
    assert_eq!(creates[0].entity_id, view.id.to_string());
    let fields: Vec<&str> = creates[0].changes.iter().map(|(f, _)| f).collect();
    assert_eq!(fields, ["label", "provider"]);

    let log = captured_log_text();
    assert!(log.contains(&view.id.to_string()));
    assert!(!log.contains(SECRET_PART));

    let runs = world
        .server
        .store
        .list_sync_runs(view.id, &PageRequest::default())
        .await
        .unwrap()
        .items;
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].trigger, SyncTrigger::Initial);
    assert_eq!(runs[0].status, SyncRunStatus::Queued);
}

#[tokio::test]
async fn list_connections_shows_schedule_and_visible_accounts() {
    let world = world().await;
    world
        .sync_once(vec![reported("acct-1", "USD", None, Vec::new())])
        .await;
    let attempted = world.now();
    let views = world
        .server
        .sync
        .list_connections(&world.alice)
        .await
        .unwrap();
    assert_eq!(views.len(), 1);
    let view = &views[0];
    assert_eq!(view.id, world.connection);
    assert_eq!(view.last_attempt_at, Some(attempted));
    let interval = world
        .server
        .config
        .sync_interval
        .max(SignedDuration::from_hours(2));
    assert_eq!(
        view.next_sync_at,
        Some(attempted.checked_add(interval).unwrap())
    );
    assert_eq!(view.accounts.len(), 1);
    assert_eq!(view.accounts[0].name, "Everyday savings");
    assert_eq!(
        view.accounts[0].institution.as_deref(),
        Some("Example Bank")
    );

    world
        .server
        .sync
        .remove_connection(&world.alice, world.connection)
        .await
        .unwrap();
    let views = world
        .server
        .sync
        .list_connections(&world.alice)
        .await
        .unwrap();
    assert_eq!(views[0].next_sync_at, None);
    assert_eq!(views[0].accounts.len(), 1, "provider accounts stay");
}

#[tokio::test]
async fn reconnect_reseals_and_keeps_bindings() {
    let world = world().await;
    world
        .sync_once(vec![reported(
            "acct-1",
            "USD",
            None,
            vec![txn("t-1", "-4.50")],
        )])
        .await;
    let held = world.provider_account("acct-1").await;
    let checking = world.account("Checking", "USD").await;
    world
        .server
        .sync
        .bind(&world.alice, held.id, checking)
        .await
        .unwrap();
    let before = world.sealed().await.unwrap();
    let mut tx = world.server.store.begin().await.unwrap();
    tx.update_connection_status(
        world.connection,
        ConnectionStatusChange {
            status: ConnectionStatus::ReauthRequired,
            last_error: Some("the provider refused the credentials".into()),
            attempted_at: None,
            succeeded_at: None,
            at: world.now(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    world.server.clock.advance(SignedDuration::from_hours(1));

    let renewed = "https://user:RENEWED-CREDENTIAL@bridge.example/simplefin";
    let view = world
        .server
        .sync
        .reconnect(&world.alice, world.connection, setup_token(renewed))
        .await
        .unwrap();
    assert_eq!(view.id, world.connection);
    assert!(matches!(
        view.status,
        coffer_server::sync::connections::ConnectionState::Active
    ));
    assert_eq!(view.last_error, None);
    assert_eq!(view.accounts.len(), 1);
    assert_eq!(view.accounts[0].account, Some(checking));

    let after = world.sealed().await.unwrap();
    let resealed = after != before;
    assert!(resealed);
    assert!(!contains(&after, "RENEWED-CREDENTIAL"));
    assert!(world.opens_to(world.connection, after, renewed));
    assert_eq!(
        world.provider_account("acct-1").await.account,
        Some(checking)
    );
    assert_eq!(world.transactions(checking).await.len(), 1);

    let reconnects = world.audit_actions("connection.reconnect").await;
    assert_eq!(reconnects.len(), 1);
    assert!(
        !serde_json::to_string(&reconnects[0].changes)
            .unwrap()
            .contains("RENEWED-CREDENTIAL")
    );
    let runs = world
        .server
        .store
        .list_sync_runs(world.connection, &PageRequest::default())
        .await
        .unwrap()
        .items;
    assert_eq!(runs[0].trigger, SyncTrigger::Manual);
    assert_eq!(runs[0].status, SyncRunStatus::Queued);

    // The next sync uses the new credentials.
    world.fake.push(Ok(FetchResult {
        accounts: Vec::new(),
        connection_errors: Vec::new(),
        skipped: 0,
        cursor: None,
    }));
    world.server.sync.run(runs[0].id).await.unwrap();
    let fetches = world.fake.requests();
    let used_renewed = fetches.last().unwrap().credentials.expose_secret() == renewed;
    assert!(used_renewed);
}

/// A reconnect that commits while a removal is giving up the old
/// credentials makes the removal refuse: erasing the new credentials would
/// leave them live at the provider.
#[tokio::test]
async fn remove_refuses_when_reconnected_during_disconnect() {
    let world = world().await;
    let gate = world.fake.hold_next_disconnect();
    let sync = Arc::clone(&world.server.sync);
    let alice = world.alice;
    let connection = world.connection;
    let removing = tokio::spawn(async move { sync.remove_connection(&alice, connection).await });

    gate.entered().await;
    world
        .server
        .sync
        .reconnect(&world.alice, world.connection, setup_token(RENEWED))
        .await
        .unwrap();
    gate.release();
    let err = removing.await.unwrap().err().unwrap();

    assert!(matches!(err, SyncError::Conflict(_)));
    let stored = world
        .server
        .store
        .get_connection(world.connection)
        .await
        .unwrap();
    assert_eq!(stored.status, ConnectionStatus::Active);
    assert!(world.opens_to(world.connection, stored.credentials.unwrap(), RENEWED));
    assert_eq!(world.audit_actions("connection.remove").await.len(), 0);
}

/// A reconnect whose connection is removed while the provider connects
/// gives the new credentials up again before refusing.
#[tokio::test]
async fn reconnect_gives_up_new_credentials_when_removed_meanwhile() {
    let world = world().await;
    let gate = world.fake.hold_next_connect();
    let sync = Arc::clone(&world.server.sync);
    let alice = world.alice;
    let connection = world.connection;
    let reconnecting = tokio::spawn(async move {
        sync.reconnect(&alice, connection, setup_token(RENEWED))
            .await
    });

    gate.entered().await;
    world
        .server
        .sync
        .remove_connection(&world.alice, world.connection)
        .await
        .unwrap();
    gate.release();
    let err = reconnecting.await.unwrap().err().unwrap();

    assert!(matches!(err, SyncError::Conflict(_)));
    let disconnects = world.fake.disconnects();
    assert_eq!(disconnects.len(), 2);
    let gave_up_old = disconnects[0].expose_secret() == PLAINTEXT;
    let gave_up_new = disconnects[1].expose_secret() == RENEWED;
    assert!(gave_up_old);
    assert!(gave_up_new);
    let stored = world
        .server
        .store
        .get_connection(world.connection)
        .await
        .unwrap();
    assert_eq!(stored.status, ConnectionStatus::Removed);
    assert!(stored.credentials.is_none());
}

/// Of two reconnects racing, the one that loses gives its credentials up
/// instead of overwriting the winner's.
#[tokio::test]
async fn losing_reconnect_gives_up_its_credentials() {
    let world = world().await;
    let gate = world.fake.hold_next_connect();
    let sync = Arc::clone(&world.server.sync);
    let alice = world.alice;
    let connection = world.connection;
    let slow = tokio::spawn(async move {
        sync.reconnect(&alice, connection, setup_token(PLAINTEXT))
            .await
    });

    gate.entered().await;
    world
        .server
        .sync
        .reconnect(&world.alice, world.connection, setup_token(RENEWED))
        .await
        .unwrap();
    gate.release();
    let err = slow.await.unwrap().err().unwrap();

    assert!(matches!(err, SyncError::Conflict(_)));
    let stored = world
        .server
        .store
        .get_connection(world.connection)
        .await
        .unwrap();
    assert!(world.opens_to(world.connection, stored.credentials.unwrap(), RENEWED));
    let disconnects = world.fake.disconnects();
    assert_eq!(disconnects.len(), 1);
    let gave_up_loser = disconnects[0].expose_secret() == PLAINTEXT;
    assert!(gave_up_loser, "the loser's credentials are given up");
    assert_eq!(world.audit_actions("connection.reconnect").await.len(), 1);
}

/// A run that started with the old credentials and fails after a reconnect
/// does not mark the reconnected connection as needing reauthentication.
#[tokio::test]
async fn reconnect_during_a_running_sync_is_not_undone() {
    let world = world().await;
    let gate = world.fake.hold_next_fetch();
    world.fake.push(Err(ProviderError::ReauthRequired));
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
        .reconnect(&world.alice, world.connection, setup_token(RENEWED))
        .await
        .unwrap();
    gate.release();
    let finished = running.await.unwrap().unwrap();

    assert_eq!(finished.status, SyncRunStatus::Failed);
    let stored = world
        .server
        .store
        .get_connection(world.connection)
        .await
        .unwrap();
    assert_eq!(stored.status, ConnectionStatus::Active);
    assert_eq!(stored.last_error, None);
}

/// A connection that cannot be stored gives the claimed credentials up
/// again, as a failed reconnect does.
#[tokio::test]
async fn failed_create_gives_up_its_credentials() {
    let world = world().await;
    world.server.inject_connection_insert_failure().await;
    let before = world.server.store.list_connections().await.unwrap().len();

    let err = world
        .server
        .sync
        .create_connection(
            &world.alice,
            ProviderKind::SimpleFin,
            ConnectionLabel::parse(INJECTED_FAILURE_LABEL).unwrap(),
            setup_token(RENEWED),
            None,
        )
        .await
        .err()
        .unwrap();

    assert!(matches!(err, SyncError::Store(_)));
    let disconnects = world.fake.disconnects();
    assert_eq!(disconnects.len(), 1);
    let gave_up_claimed = disconnects[0].expose_secret() == RENEWED;
    assert!(gave_up_claimed, "the claimed credentials are given up");
    assert_eq!(
        world.server.store.list_connections().await.unwrap().len(),
        before
    );
    assert_eq!(world.audit_actions("connection.create").await.len(), 0);
}

#[tokio::test]
async fn reconnect_on_a_removed_connection_is_refused() {
    let world = world().await;
    world
        .server
        .sync
        .remove_connection(&world.alice, world.connection)
        .await
        .unwrap();
    let err = world
        .server
        .sync
        .reconnect(&world.alice, world.connection, setup_token(RENEWED))
        .await
        .err()
        .unwrap();
    assert!(matches!(err, SyncError::Conflict(_)));
    let stored = world
        .server
        .store
        .get_connection(world.connection)
        .await
        .unwrap();
    assert!(stored.credentials.is_none());
    assert_eq!(world.audit_actions("connection.reconnect").await.len(), 0);
}

/// A run that succeeds with the old credentials after a reconnect records
/// nothing on the connection: no sync has used the new credentials yet.
#[tokio::test]
async fn reconnect_during_a_successful_sync_keeps_the_new_state() {
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
        .reconnect(&world.alice, world.connection, setup_token(RENEWED))
        .await
        .unwrap();
    gate.release();
    let finished = running.await.unwrap().unwrap();

    assert_eq!(finished.status, SyncRunStatus::Succeeded);
    assert_eq!(
        world.provider_accounts().await.len(),
        1,
        "the data is applied"
    );
    let stored = world
        .server
        .store
        .get_connection(world.connection)
        .await
        .unwrap();
    assert_eq!(stored.status, ConnectionStatus::Active);
    assert_eq!(stored.last_attempt_at, None);
    assert_eq!(stored.last_success_at, None);
}
