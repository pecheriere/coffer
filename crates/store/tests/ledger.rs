//! Accounts, balance snapshots and idempotency keys.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use coffer_domain::accounts::{AccountKind, AccountName, BalanceSource, LiquidityClass};
use coffer_domain::identity::{DisplayName, Role, Username};
use coffer_domain::ids::{AccountId, MemberId};
use coffer_domain::money::Currency;
use coffer_domain::policy::AccountScope;
use coffer_store::{
    AccountChanges, AccountRecord, BalanceSnapshot, NewAccount, NewMember, PageRequest,
    Reservation, Store, StoreError,
};
use jiff::civil::{Date, date};
use jiff::{SignedDuration, Timestamp};
use tempfile::TempDir;

fn at(seconds: i64) -> Timestamp {
    Timestamp::from_second(1_800_000_000 + seconds).unwrap()
}

async fn open() -> (TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("coffer.db")).await.unwrap();
    (dir, store)
}

async fn account(store: &Store, name: &str) -> AccountRecord {
    let mut tx = store.begin().await.unwrap();
    let record = tx
        .insert_account(NewAccount {
            id: AccountId::new(),
            name: AccountName::parse(name).unwrap(),
            kind: AccountKind::Checking,
            currency: Currency::parse("USD").unwrap(),
            liquidity: LiquidityClass::Cash,
            liquidity_overridden: false,
            include_in_net_worth: true,
            include_in_forecast: true,
            created_at: at(0),
        })
        .await
        .unwrap();
    tx.commit().await.unwrap();
    record
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

fn snapshot(account: AccountId, on: Date, minor: i64, source: BalanceSource) -> BalanceSnapshot {
    BalanceSnapshot {
        account,
        on,
        balance_minor: minor,
        source,
        recorded_at: at(1),
    }
}

#[tokio::test]
async fn balance_last_write_wins_per_day() {
    let (_dir, store) = open().await;
    let checking = account(&store, "Checking").await;
    let day = date(2026, 10, 1);

    let mut tx = store.begin().await.unwrap();
    let previous = tx
        .upsert_balance(snapshot(checking.id, day, 10_000, BalanceSource::Manual))
        .await
        .unwrap();
    assert_eq!(previous, None);
    tx.upsert_balance(snapshot(
        checking.id,
        date(2026, 9, 30),
        9_000,
        BalanceSource::Sync,
    ))
    .await
    .unwrap();
    let previous = tx
        .upsert_balance(snapshot(checking.id, day, -2_500, BalanceSource::Sync))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(previous.map(|p| p.balance_minor), Some(10_000));

    let latest = store.latest_balance(checking.id).await.unwrap().unwrap();
    assert_eq!(latest.on, day);
    assert_eq!(latest.balance_minor, -2_500);
    assert_eq!(latest.source, BalanceSource::Sync);

    let page = store
        .list_balances(checking.id, None, None, &PageRequest::default())
        .await
        .unwrap();
    let days: Vec<_> = page.items.iter().map(|b| (b.on, b.balance_minor)).collect();
    assert_eq!(
        days,
        [(day, -2_500), (date(2026, 9, 30), 9_000)],
        "one snapshot per day, newest first"
    );
    assert_eq!(page.next, None);
}

#[tokio::test]
async fn balances_page_by_date_within_a_range() {
    let (_dir, store) = open().await;
    let checking = account(&store, "Checking").await;
    let other = account(&store, "Other").await;
    let mut tx = store.begin().await.unwrap();
    for day in 1..=5 {
        tx.upsert_balance(snapshot(
            checking.id,
            date(2026, 10, day),
            i64::from(day),
            BalanceSource::Manual,
        ))
        .await
        .unwrap();
    }
    tx.upsert_balance(snapshot(
        other.id,
        date(2026, 10, 3),
        99,
        BalanceSource::Manual,
    ))
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let first = store
        .list_balances(
            checking.id,
            Some(date(2026, 10, 2)),
            Some(date(2026, 10, 5)),
            &PageRequest {
                after: None,
                limit: 2,
            },
        )
        .await
        .unwrap();
    let minors: Vec<_> = first.items.iter().map(|b| b.balance_minor).collect();
    assert_eq!(minors, [4, 3], "`from` inclusive, `to` exclusive");
    let second = store
        .list_balances(
            checking.id,
            Some(date(2026, 10, 2)),
            Some(date(2026, 10, 5)),
            &PageRequest {
                after: first.next,
                limit: 2,
            },
        )
        .await
        .unwrap();
    let minors: Vec<_> = second.items.iter().map(|b| b.balance_minor).collect();
    assert_eq!(minors, [2]);
    assert_eq!(second.next, None);

    let err = store
        .list_balances(
            checking.id,
            None,
            None,
            &PageRequest {
                after: Some("not-a-date".into()),
                limit: 2,
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::InvalidCursor));
}

#[tokio::test]
async fn list_accounts_honours_scope_and_closed() {
    let (_dir, store) = open().await;
    let open_one = account(&store, "Open").await;
    let closed = account(&store, "Closed").await;
    let third = account(&store, "Third").await;
    let mut tx = store.begin().await.unwrap();
    let updated = tx
        .update_account(
            closed.id,
            AccountChanges {
                closed_at: Some(Some(at(5))),
                ..AccountChanges::default()
            },
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(updated.closed_at, Some(at(5)));

    let ids = |records: Vec<AccountRecord>| records.into_iter().map(|r| r.id).collect::<Vec<_>>();
    assert_eq!(
        ids(store
            .list_accounts(&AccountScope::All, false)
            .await
            .unwrap()),
        [open_one.id, third.id]
    );
    assert_eq!(
        ids(store.list_accounts(&AccountScope::All, true).await.unwrap()),
        [open_one.id, closed.id, third.id]
    );
    assert_eq!(
        ids(store
            .list_accounts(&AccountScope::Only(vec![closed.id, third.id]), true)
            .await
            .unwrap()),
        [closed.id, third.id]
    );
    assert_eq!(
        ids(store
            .list_accounts(&AccountScope::Only(vec![closed.id, third.id]), false)
            .await
            .unwrap()),
        [third.id]
    );
    assert_eq!(
        store
            .list_accounts(&AccountScope::Only(Vec::new()), true)
            .await
            .unwrap(),
        []
    );
}

#[tokio::test]
async fn account_changes_apply_only_set_fields() {
    let (_dir, store) = open().await;
    let checking = account(&store, "Checking").await;
    let mut tx = store.begin().await.unwrap();
    let updated = tx
        .update_account(
            checking.id,
            AccountChanges {
                name: Some(AccountName::parse("Visa").unwrap()),
                kind: Some(AccountKind::CreditCard),
                liquidity: Some(LiquidityClass::ShortTermLiability),
                ..AccountChanges::default()
            },
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(updated.name.as_str(), "Visa");
    assert_eq!(updated.kind, AccountKind::CreditCard);
    assert_eq!(updated.liquidity, LiquidityClass::ShortTermLiability);
    assert!(!updated.liquidity_overridden);
    assert!(updated.include_in_net_worth);
    assert_eq!(updated.currency.as_str(), "USD");
    assert_eq!(store.get_account(checking.id).await.unwrap(), updated);

    let mut tx = store.begin().await.unwrap();
    let err = tx
        .update_account(AccountId::new(), AccountChanges::default())
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::NotFound));
    let err = store.get_account(AccountId::new()).await.unwrap_err();
    assert!(matches!(err, StoreError::NotFound));
}

#[tokio::test]
async fn idempotency_completion_rolls_back_with_its_tx() {
    let (_dir, store) = open().await;
    let alice = member(&store).await;
    let hash = [7_u8; 32];

    let mut tx = store.begin().await.unwrap();
    let reserved = tx
        .reserve_idempotency(alice, "key-1", hash, at(0))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(reserved, Reservation::Fresh);

    // The create's transaction completes the key, then fails and is dropped.
    let mut tx = store.begin().await.unwrap();
    tx.complete_idempotency(alice, "key-1", 201, r#"{"id":"x"}"#)
        .await
        .unwrap();
    drop(tx);

    let mut tx = store.begin().await.unwrap();
    let again = tx
        .reserve_idempotency(alice, "key-1", hash, at(1))
        .await
        .unwrap();
    assert_eq!(again, Reservation::InProgress, "the row is still pending");
    tx.complete_idempotency(alice, "key-1", 201, r#"{"id":"x"}"#)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let mut tx = store.begin().await.unwrap();
    assert_eq!(
        tx.reserve_idempotency(alice, "key-1", hash, at(2))
            .await
            .unwrap(),
        Reservation::Replay {
            status: 201,
            response_json: r#"{"id":"x"}"#.to_owned(),
        }
    );
    assert_eq!(
        tx.reserve_idempotency(alice, "key-1", [8; 32], at(2))
            .await
            .unwrap(),
        Reservation::Mismatch
    );
}

#[tokio::test]
async fn idempotency_keys_expire_and_abandoned_reservations_are_replaced() {
    let (_dir, store) = open().await;
    let alice = member(&store).await;
    let hash = [1_u8; 32];
    let mut tx = store.begin().await.unwrap();
    assert_eq!(
        tx.reserve_idempotency(alice, "pending", hash, at(0))
            .await
            .unwrap(),
        Reservation::Fresh
    );
    assert_eq!(
        tx.reserve_idempotency(alice, "done", hash, at(0))
            .await
            .unwrap(),
        Reservation::Fresh
    );
    tx.complete_idempotency(alice, "done", 201, "{}")
        .await
        .unwrap();

    let five_minutes = SignedDuration::from_mins(5).as_secs();
    assert_eq!(
        tx.reserve_idempotency(alice, "pending", hash, at(five_minutes))
            .await
            .unwrap(),
        Reservation::InProgress,
        "exactly five minutes is not yet abandoned"
    );
    assert_eq!(
        tx.reserve_idempotency(alice, "pending", hash, at(five_minutes + 1))
            .await
            .unwrap(),
        Reservation::Fresh,
        "an abandoned reservation is taken over"
    );

    let day = SignedDuration::from_hours(24).as_secs();
    assert_eq!(
        tx.reserve_idempotency(alice, "done", [2; 32], at(day))
            .await
            .unwrap(),
        Reservation::Mismatch,
        "a key lives for 24 hours"
    );
    assert_eq!(
        tx.reserve_idempotency(alice, "done", [2; 32], at(day + 1))
            .await
            .unwrap(),
        Reservation::Fresh,
        "then it is purged and reusable"
    );
}

#[tokio::test]
async fn released_reservation_frees_the_key() {
    let (_dir, store) = open().await;
    let alice = member(&store).await;
    let mut tx = store.begin().await.unwrap();
    tx.reserve_idempotency(alice, "k", [3; 32], at(0))
        .await
        .unwrap();
    tx.release_idempotency(alice, "k").await.unwrap();
    assert_eq!(
        tx.reserve_idempotency(alice, "k", [4; 32], at(1))
            .await
            .unwrap(),
        Reservation::Fresh
    );
    tx.complete_idempotency(alice, "k", 201, "{}")
        .await
        .unwrap();
    // Releasing never removes a completed key.
    tx.release_idempotency(alice, "k").await.unwrap();
    assert!(matches!(
        tx.reserve_idempotency(alice, "k", [4; 32], at(2))
            .await
            .unwrap(),
        Reservation::Replay { .. }
    ));
    let err = tx
        .complete_idempotency(alice, "unreserved", 201, "{}")
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Conflict(_)));
}
