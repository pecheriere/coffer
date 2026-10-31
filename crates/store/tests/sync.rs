//! Connections, provider accounts, their bindings and sync runs.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use coffer_domain::accounts::{AccountKind, AccountName, ConnectionLabel, LiquidityClass};
use coffer_domain::identity::{DisplayName, Role, Username};
use coffer_domain::ids::{AccountId, ConnectionId, MemberId, SyncRunId};
use coffer_domain::money::Currency;
use coffer_domain::sync::{SyncRunStatus, SyncTrigger};
use coffer_store::{
    ConnectionStatus, ConnectionStatusChange, NewAccount, NewConnection, NewMember, NewSyncRun,
    PageRequest, ProviderAccountUpsert, RawBalance, RunCounts, Store, StoreError,
};
use jiff::Timestamp;
use jiff::civil::date;
use tempfile::TempDir;

fn at(seconds: i64) -> Timestamp {
    Timestamp::from_second(1_800_000_000 + seconds).unwrap()
}

async fn open() -> (TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("coffer.db")).await.unwrap();
    (dir, store)
}

async fn member(store: &Store) -> MemberId {
    let mut tx = store.begin().await.unwrap();
    let member = tx
        .insert_member(NewMember {
            id: MemberId::new(),
            username: Username::parse("alice").unwrap(),
            display_name: DisplayName::parse("Alice").unwrap(),
            role: Role::Admin,
            created_at: at(0),
        })
        .await
        .unwrap();
    tx.commit().await.unwrap();
    member.id
}

fn upsert(
    connection: coffer_domain::ids::ConnectionId,
    name: &str,
    balance: Option<RawBalance>,
    seen: i64,
) -> ProviderAccountUpsert {
    ProviderAccountUpsert {
        connection,
        provider_account_id: "acct-1".into(),
        name: name.into(),
        institution: Some("Example Bank".into()),
        currency_raw: "USD".into(),
        kind_hint: Some(AccountKind::Savings),
        balance,
        seen_at: at(seen),
    }
}

#[tokio::test]
async fn connection_lifecycle() {
    let (_dir, store) = open().await;
    let owner = member(&store).await;
    let mut tx = store.begin().await.unwrap();
    let created = tx
        .insert_connection(NewConnection {
            provider: "testbank",
            label: ConnectionLabel::parse("Family bank").unwrap(),
            owner,
            credentials: vec![9; 40],
            created_at: at(1),
        })
        .await
        .unwrap();
    assert_eq!(created.status, ConnectionStatus::Active);
    assert_eq!(created.credentials.as_deref(), Some([9; 40].as_slice()));
    assert!(
        !format!("{created:?}").contains("9, 9"),
        "Debug never shows the credentials"
    );
    tx.update_connection_status(
        created.id,
        ConnectionStatusChange {
            status: ConnectionStatus::Error,
            last_error: Some("upstream unavailable".into()),
            attempted_at: Some(at(2)),
            succeeded_at: None,
            at: at(2),
        },
    )
    .await
    .unwrap();
    tx.set_provider_state(created.id, Some("cursor-7".into()))
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let errored = store.get_connection(created.id).await.unwrap();
    assert_eq!(errored.status, ConnectionStatus::Error);
    assert_eq!(errored.last_error.as_deref(), Some("upstream unavailable"));
    assert_eq!(errored.last_attempt_at, Some(at(2)));
    assert_eq!(errored.last_success_at, None);
    assert_eq!(errored.provider_state.as_deref(), Some("cursor-7"));

    let mut tx = store.begin().await.unwrap();
    tx.update_connection_status(
        created.id,
        ConnectionStatusChange {
            status: ConnectionStatus::Removed,
            last_error: None,
            attempted_at: None,
            succeeded_at: None,
            at: at(3),
        },
    )
    .await
    .unwrap();
    tx.set_connection_credentials(created.id, None)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let removed = store.get_connection(created.id).await.unwrap();
    assert_eq!(removed.status, ConnectionStatus::Removed);
    assert_eq!(removed.removed_at, Some(at(3)));
    assert_eq!(removed.credentials, None);
    assert_eq!(removed.last_error, None);
    assert_eq!(removed.last_attempt_at, Some(at(2)), "kept when not given");
    assert_eq!(store.list_connections().await.unwrap(), [removed]);

    let mut tx = store.begin().await.unwrap();
    let malformed = tx
        .insert_connection(NewConnection {
            provider: "Test Bank",
            label: ConnectionLabel::parse("Other").unwrap(),
            owner,
            credentials: Vec::new(),
            created_at: at(4),
        })
        .await;
    assert!(matches!(malformed, Err(StoreError::Invalid(_))));
}

#[tokio::test]
async fn provider_account_refresh_keeps_binding_and_balance() {
    let (_dir, store) = open().await;
    let owner = member(&store).await;
    let mut tx = store.begin().await.unwrap();
    let connection = tx
        .insert_connection(NewConnection {
            provider: "testbank",
            label: ConnectionLabel::parse("Bank").unwrap(),
            owner,
            credentials: vec![1],
            created_at: at(0),
        })
        .await
        .unwrap();
    let account = tx
        .insert_account(NewAccount {
            id: AccountId::new(),
            name: AccountName::parse("Savings").unwrap(),
            kind: AccountKind::Savings,
            currency: Currency::parse("USD").unwrap(),
            liquidity: LiquidityClass::Cash,
            liquidity_overridden: false,
            include_in_net_worth: true,
            include_in_forecast: true,
            created_at: at(0),
        })
        .await
        .unwrap();
    let balance = RawBalance {
        on: date(2026, 10, 30),
        amount_raw: "1250.00".into(),
    };
    let first = tx
        .upsert_provider_account(upsert(connection.id, "Savings", Some(balance.clone()), 1))
        .await
        .unwrap();
    assert_eq!(first.account, None);
    assert!(!tx.account_is_linked(account.id).await.unwrap());
    tx.bind_provider_account(first.id, account.id)
        .await
        .unwrap();
    tx.set_provider_account_error(first.id, Some("stale data".into()))
        .await
        .unwrap();
    let refreshed = tx
        .upsert_provider_account(upsert(connection.id, "Savings plus", None, 2))
        .await
        .unwrap();
    assert_eq!(refreshed.id, first.id, "the same provider account");
    assert_eq!(refreshed.name, "Savings plus");
    assert_eq!(refreshed.account, Some(account.id), "the binding stays");
    assert_eq!(
        refreshed.last_balance,
        Some(balance),
        "kept without a new one"
    );
    assert_eq!(refreshed.error.as_deref(), Some("stale data"));
    assert_eq!(refreshed.first_seen_at, at(1));
    assert_eq!(refreshed.last_seen_at, at(2));

    let rebound = tx.bind_provider_account(first.id, account.id).await;
    assert!(matches!(rebound, Err(StoreError::Conflict(_))));
    tx.commit().await.unwrap();
    assert!(store.account_is_linked(account.id).await.unwrap());
    assert_eq!(
        store
            .list_provider_accounts(Some(connection.id))
            .await
            .unwrap(),
        std::slice::from_ref(&refreshed)
    );
    assert_eq!(
        store.list_provider_accounts(None).await.unwrap(),
        [refreshed]
    );
}

async fn add_connection(store: &Store, owner: MemberId) -> ConnectionId {
    let mut tx = store.begin().await.unwrap();
    let connection = tx
        .insert_connection(NewConnection {
            provider: "testbank",
            label: ConnectionLabel::parse("Bank").unwrap(),
            owner,
            credentials: vec![1],
            created_at: at(0),
        })
        .await
        .unwrap();
    tx.commit().await.unwrap();
    connection.id
}

async fn queue(
    store: &Store,
    connection: ConnectionId,
    trigger: SyncTrigger,
    requested_by: Option<MemberId>,
    queued: i64,
) -> SyncRunId {
    let mut tx = store.begin().await.unwrap();
    let run = tx
        .insert_sync_run(NewSyncRun {
            connection,
            trigger,
            requested_by: requested_by.map(|member| (member, None)),
            queued_at: at(queued),
        })
        .await
        .unwrap();
    tx.commit().await.unwrap();
    run.id
}

#[tokio::test]
async fn sync_run_lifecycle() {
    let (_dir, store) = open().await;
    let owner = member(&store).await;
    let connection = add_connection(&store, owner).await;
    let id = queue(&store, connection, SyncTrigger::Manual, Some(owner), 10).await;

    let queued = store.get_sync_run(id).await.unwrap();
    assert_eq!(queued.connection, connection);
    assert_eq!(queued.trigger, SyncTrigger::Manual);
    assert_eq!(queued.status, SyncRunStatus::Queued);
    assert_eq!(queued.requested_by, Some((owner, None)));
    assert_eq!(queued.queued_at, at(10));
    assert_eq!((queued.started_at, queued.finished_at), (None, None));
    assert_eq!(queued.detail, None);

    let mut tx = store.begin().await.unwrap();
    let running = tx.start_sync_run(id, at(11)).await.unwrap();
    assert_eq!(running.status, SyncRunStatus::Running);
    assert_eq!(running.started_at, Some(at(11)));
    assert!(
        matches!(
            tx.start_sync_run(id, at(12)).await,
            Err(StoreError::Conflict(_))
        ),
        "a run starts once"
    );
    assert!(
        matches!(
            tx.finish_sync_run(id, SyncRunStatus::Running, None, at(12))
                .await,
            Err(StoreError::Invalid(_))
        ),
        "a run finishes with an outcome"
    );
    let finished = tx
        .finish_sync_run(
            id,
            SyncRunStatus::Partial,
            Some("2 accounts".into()),
            at(13),
        )
        .await
        .unwrap();
    assert_eq!(finished.status, SyncRunStatus::Partial);
    assert_eq!(finished.detail.as_deref(), Some("2 accounts"));
    assert_eq!(finished.finished_at, Some(at(13)));
    assert!(
        matches!(
            tx.finish_sync_run(id, SyncRunStatus::Failed, None, at(14))
                .await,
            Err(StoreError::Conflict(_))
        ),
        "a run finishes once"
    );
    tx.commit().await.unwrap();
    assert_eq!(store.get_sync_run(id).await.unwrap(), finished);
    assert!(matches!(
        store.get_sync_run(SyncRunId::new()).await,
        Err(StoreError::NotFound)
    ));

    let unattributed = queue(&store, connection, SyncTrigger::Schedule, None, 20).await;
    assert_eq!(
        store.get_sync_run(unattributed).await.unwrap().requested_by,
        None
    );
}

#[tokio::test]
async fn sync_run_counts_queue_and_interruption() {
    let (_dir, store) = open().await;
    let owner = member(&store).await;
    let connection = add_connection(&store, owner).await;
    let other = add_connection(&store, owner).await;
    assert_eq!(
        store
            .begin()
            .await
            .unwrap()
            .sync_run_counts(connection, at(0))
            .await
            .unwrap(),
        RunCounts {
            started_or_pending: 0,
            last_started_at: None,
            in_flight: false,
        }
    );
    assert_eq!(store.next_queued_run().await.unwrap(), None);

    // An old finished run, a recent finished one, then a queued one.
    let old = queue(&store, connection, SyncTrigger::Initial, None, 1).await;
    let recent = queue(&store, connection, SyncTrigger::Schedule, None, 100).await;
    let mut tx = store.begin().await.unwrap();
    tx.start_sync_run(old, at(2)).await.unwrap();
    tx.finish_sync_run(old, SyncRunStatus::Succeeded, None, at(3))
        .await
        .unwrap();
    tx.start_sync_run(recent, at(101)).await.unwrap();
    tx.finish_sync_run(recent, SyncRunStatus::Failed, None, at(102))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        store
            .begin()
            .await
            .unwrap()
            .sync_run_counts(connection, at(50))
            .await
            .unwrap(),
        RunCounts {
            started_or_pending: 1,
            last_started_at: Some(at(101)),
            in_flight: false,
        }
    );
    let pending = queue(&store, connection, SyncTrigger::Manual, Some(owner), 200).await;
    let elsewhere = queue(&store, other, SyncTrigger::Manual, None, 150).await;
    assert_eq!(
        store
            .begin()
            .await
            .unwrap()
            .sync_run_counts(connection, at(50))
            .await
            .unwrap(),
        RunCounts {
            started_or_pending: 2,
            last_started_at: Some(at(101)),
            in_flight: true,
        }
    );
    assert_eq!(
        store.next_queued_run().await.unwrap().map(|run| run.id),
        Some(elsewhere),
        "the oldest queued run, of any connection"
    );

    let mut tx = store.begin().await.unwrap();
    tx.start_sync_run(elsewhere, at(201)).await.unwrap();
    assert_eq!(
        tx.fail_running_runs("interrupted", at(300)).await.unwrap(),
        1
    );
    tx.commit().await.unwrap();
    let interrupted = store.get_sync_run(elsewhere).await.unwrap();
    assert_eq!(interrupted.status, SyncRunStatus::Failed);
    assert_eq!(interrupted.detail.as_deref(), Some("interrupted"));
    assert_eq!(interrupted.finished_at, Some(at(300)));
    assert_eq!(
        store.get_sync_run(pending).await.unwrap().status,
        SyncRunStatus::Queued,
        "queued runs stay queued"
    );
    assert_eq!(
        store.next_queued_run().await.unwrap().map(|run| run.id),
        Some(pending)
    );
}

#[tokio::test]
async fn sync_runs_list_newest_first() {
    let (_dir, store) = open().await;
    let owner = member(&store).await;
    let connection = add_connection(&store, owner).await;
    let other = add_connection(&store, owner).await;
    let mut ids = Vec::new();
    for queued in 0..5 {
        ids.push(queue(&store, connection, SyncTrigger::Schedule, None, queued).await);
    }
    queue(&store, other, SyncTrigger::Schedule, None, 9).await;
    let page = PageRequest {
        after: None,
        limit: 3,
    };
    let first = store.list_sync_runs(connection, &page).await.unwrap();
    let listed: Vec<_> = first.items.iter().map(|run| run.id).collect();
    assert_eq!(listed, [ids[4], ids[3], ids[2]]);
    let second = store
        .list_sync_runs(
            connection,
            &PageRequest {
                after: first.next,
                limit: 3,
            },
        )
        .await
        .unwrap();
    let listed: Vec<_> = second.items.iter().map(|run| run.id).collect();
    assert_eq!(listed, [ids[1], ids[0]]);
    assert_eq!(second.next, None);
    assert!(matches!(
        store
            .list_sync_runs(
                connection,
                &PageRequest {
                    after: Some("nope".into()),
                    limit: 3,
                },
            )
            .await,
        Err(StoreError::InvalidCursor)
    ));
}

#[tokio::test]
async fn sync_run_start_since_counts_from_the_earliest() {
    let (_dir, store) = open().await;
    let owner = member(&store).await;
    let connection = add_connection(&store, owner).await;
    let other = add_connection(&store, owner).await;
    let first = queue(&store, connection, SyncTrigger::Initial, None, 1).await;
    let second = queue(&store, connection, SyncTrigger::Schedule, None, 100).await;
    queue(&store, connection, SyncTrigger::Manual, None, 200).await;
    let mut tx = store.begin().await.unwrap();
    tx.start_sync_run(first, at(2)).await.unwrap();
    tx.finish_sync_run(first, SyncRunStatus::Succeeded, None, at(3))
        .await
        .unwrap();
    tx.start_sync_run(second, at(101)).await.unwrap();
    tx.commit().await.unwrap();

    let mut tx = store.begin().await.unwrap();
    for (since, skip, expected) in [
        (0, 0, Some(at(2))),
        (0, 1, Some(at(101))),
        (0, 2, None),
        (50, 0, Some(at(101))),
        // Started exactly at `since`: outside the window.
        (2, 0, Some(at(101))),
    ] {
        assert_eq!(
            tx.sync_run_start_since(connection, at(since), skip)
                .await
                .unwrap(),
            expected
        );
    }
    assert_eq!(
        tx.sync_run_start_since(other, at(0), 0).await.unwrap(),
        None
    );
}
