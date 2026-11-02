//! Accounts, balance snapshots, idempotency keys, categories and
//! transactions.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use coffer_domain::accounts::{AccountKind, AccountName, BalanceSource, LiquidityClass};
use coffer_domain::identity::{DisplayName, Role, Username};
use coffer_domain::ids::{AccountId, AgentTokenId, CategoryId, MemberId, TransactionId};
use coffer_domain::money::Currency;
use coffer_domain::policy::AccountScope;
use coffer_domain::transactions::{
    CategoryGroupName, CategoryKind, CategoryName, Description, Notes, Payee, ReviewStatus,
    SplitInput, TransactionOrigin, UserState,
};
use coffer_store::{
    AccountChanges, AccountRecord, Attribution, BalanceSnapshot, CategoryChanges,
    CategoryGroupChanges, CategoryRecord, IdempotencyOwner, NewAccount, NewCategory,
    NewCategoryGroup, NewMember, NewTransaction, PageRequest, PayeeChange, Reservation,
    ReviewChange, Store, StoreError, UserFieldChanges,
};
use jiff::civil::{Date, date};
use jiff::{SignedDuration, Timestamp};
use tempfile::TempDir;

fn at(seconds: i64) -> Timestamp {
    Timestamp::from_second(1_800_000_000 + seconds).unwrap()
}

fn session(member: MemberId) -> IdempotencyOwner {
    IdempotencyOwner {
        member,
        agent: None,
    }
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
    let alice = session(member(&store).await);
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
    let alice = session(member(&store).await);
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
    let alice = session(member(&store).await);
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

#[tokio::test]
async fn idempotency_keys_belong_to_their_credential() {
    let (_dir, store) = open().await;
    let member = member(&store).await;
    let token = |agent| IdempotencyOwner {
        member,
        agent: Some(agent),
    };
    let (a, b) = (token(AgentTokenId::new()), token(AgentTokenId::new()));
    let hash = [5_u8; 32];
    let mut tx = store.begin().await.unwrap();
    assert_eq!(
        tx.reserve_idempotency(a, "k", hash, at(0)).await.unwrap(),
        Reservation::Fresh
    );
    tx.complete_idempotency(a, "k", 201, r#"{"id":"a"}"#)
        .await
        .unwrap();
    // The same key and request from another token of the same member, or
    // from the member's session, is a request of its own.
    for other in [b, session(member)] {
        assert_eq!(
            tx.reserve_idempotency(other, "k", hash, at(1))
                .await
                .unwrap(),
            Reservation::Fresh
        );
    }
    assert!(matches!(
        tx.reserve_idempotency(a, "k", hash, at(2)).await.unwrap(),
        Reservation::Replay { .. }
    ));
}

async fn category(store: &Store, group_name: &str, name: &str) -> CategoryRecord {
    let mut tx = store.begin().await.unwrap();
    let group = tx
        .insert_category_group(NewCategoryGroup {
            name: CategoryGroupName::parse(group_name).unwrap(),
            position: None,
            created_at: at(0),
        })
        .await
        .unwrap();
    let category = tx
        .insert_category(NewCategory {
            group: group.id,
            name: CategoryName::parse(name).unwrap(),
            kind: CategoryKind::Expense,
            position: None,
            created_at: at(0),
        })
        .await
        .unwrap();
    tx.commit().await.unwrap();
    category
}

fn new_transaction(
    account: &AccountRecord,
    origin: TransactionOrigin,
    minor: i64,
) -> NewTransaction {
    NewTransaction {
        origin,
        account: account.id,
        posted_on: date(2026, 10, 2),
        transacted_on: None,
        pending: false,
        amount_minor: minor,
        currency: account.currency,
        raw_description: Description::parse("CORNER SHOP 0042").unwrap(),
        imported_payee: Some(Payee::parse("Corner shop").unwrap()),
        memo: None,
        notes: None,
        created_at: at(5),
    }
}

/// Counts split rows, of `transaction` or of all, reading the table
/// directly: no store call can name an orphaned split.
async fn split_rows(dir: &TempDir, transaction: Option<TransactionId>) -> i64 {
    let pool = sqlx::SqlitePool::connect(&format!(
        "sqlite://{}",
        dir.path().join("coffer.db").display()
    ))
    .await
    .unwrap();
    let (count,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM transaction_splits WHERE ?1 IS NULL OR transaction_id = ?1",
    )
    .bind(transaction.map(|id| id.to_string()))
    .fetch_one(&pool)
    .await
    .unwrap();
    pool.close().await;
    count
}

#[tokio::test]
async fn deleting_transaction_cascades_splits() {
    let (dir, store) = open().await;
    let checking = account(&store, "Checking").await;
    let member = member(&store).await;
    let food = category(&store, "Living", "Food").await;
    let home = category(&store, "House", "Home").await;

    let mut tx = store.begin().await.unwrap();
    let kept = tx
        .insert_transaction(new_transaction(&checking, TransactionOrigin::Manual, -300))
        .await
        .unwrap();
    let gone = tx
        .insert_transaction(new_transaction(
            &checking,
            TransactionOrigin::Imported,
            -1_000,
        ))
        .await
        .unwrap();
    let by = Attribution {
        member: Some(member),
        agent: None,
        system: None,
        at: at(9),
    };
    let splits = [
        SplitInput {
            category: food.id,
            minor_units: -600,
            goal: None,
        },
        SplitInput {
            category: home.id,
            minor_units: -400,
            goal: None,
        },
    ];
    tx.replace_splits(gone.id, &splits, by.clone())
        .await
        .unwrap();
    tx.replace_splits(
        kept.id,
        &[SplitInput {
            category: food.id,
            minor_units: -300,
            goal: None,
        }],
        by.clone(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let stored = store.get_transaction(gone.id).await.unwrap();
    let parts: Vec<_> = stored
        .splits
        .iter()
        .map(|s| (s.category, s.minor_units))
        .collect();
    assert_eq!(
        parts,
        [(food.id, -600), (home.id, -400)],
        "in the order given"
    );
    assert_eq!(stored.categorized, Some(by));
    assert!(stored.user_state().splits);
    assert_eq!(
        stored.payee.as_ref().map(Payee::as_str),
        Some("Corner shop")
    );
    assert!(!stored.payee_edited);
    assert_eq!(stored.review, ReviewStatus::Unreviewed);
    assert_eq!(stored.updated_at, at(9));

    let mut tx = store.begin().await.unwrap();
    tx.delete_transaction(gone.id).await.unwrap();
    tx.commit().await.unwrap();

    assert!(matches!(
        store.get_transaction(gone.id).await,
        Err(StoreError::NotFound)
    ));
    assert_eq!(split_rows(&dir, Some(gone.id)).await, 0);
    assert_eq!(split_rows(&dir, None).await, 1, "only the kept one's split");
    assert_eq!(
        store.get_transaction(kept.id).await.unwrap().splits.len(),
        1
    );
    let mut tx = store.begin().await.unwrap();
    assert!(matches!(
        tx.delete_transaction(gone.id).await,
        Err(StoreError::NotFound)
    ));
}

#[tokio::test]
async fn user_field_changes_apply_only_set_fields() {
    let (_dir, store) = open().await;
    let checking = account(&store, "Checking").await;
    let member = member(&store).await;
    let mut tx = store.begin().await.unwrap();
    let record = tx
        .insert_transaction(new_transaction(
            &checking,
            TransactionOrigin::Imported,
            -1_000,
        ))
        .await
        .unwrap();
    assert!(record.user_state().is_empty());
    let by = Attribution {
        member: Some(member),
        agent: None,
        system: None,
        at: at(20),
    };
    let changed = tx
        .update_user_fields(
            record.id,
            UserFieldChanges {
                payee: Some(PayeeChange::Set(Payee::parse("Grocer").unwrap())),
                notes: Some(Some(Notes::parse("weekly\nshop").unwrap())),
                review: Some(ReviewChange::Reviewed(by.clone())),
                assignee: Some(Some(member)),
                at: at(20),
            },
        )
        .await
        .unwrap();
    assert_eq!(changed.payee.as_ref().map(Payee::as_str), Some("Grocer"));
    assert!(changed.payee_edited);
    assert_eq!(
        changed.notes.as_ref().map(Notes::as_str),
        Some("weekly\nshop")
    );
    assert_eq!(changed.review, ReviewStatus::Reviewed);
    assert_eq!(changed.reviewed, Some(by));
    assert_eq!(changed.assignee, Some(member));
    assert_eq!(changed.updated_at, at(20));
    assert_eq!(
        changed.user_state(),
        UserState {
            splits: false,
            notes: true,
            reviewed: true,
            assignee: true,
            payee_edited: true
        }
    );

    let restored = tx
        .update_user_fields(
            record.id,
            UserFieldChanges {
                payee: Some(PayeeChange::RestoreImported),
                review: Some(ReviewChange::Unreviewed),
                at: at(30),
                ..UserFieldChanges::new(at(30))
            },
        )
        .await
        .unwrap();
    assert_eq!(
        restored.payee.as_ref().map(Payee::as_str),
        Some("Corner shop")
    );
    assert!(!restored.payee_edited);
    assert_eq!(restored.review, ReviewStatus::Unreviewed);
    assert_eq!(restored.reviewed, None);
    assert_eq!(
        restored.notes.as_ref().map(Notes::as_str),
        Some("weekly\nshop"),
        "untouched"
    );
    assert_eq!(restored.assignee, Some(member), "untouched");
    assert!(matches!(
        tx.update_user_fields(TransactionId::new(), UserFieldChanges::new(at(31)))
            .await,
        Err(StoreError::NotFound)
    ));
}

#[tokio::test]
async fn categories_list_by_position_with_their_group() {
    let (_dir, store) = open().await;
    let mut tx = store.begin().await.unwrap();
    let mut groups = Vec::new();
    for (name, position) in [("Second", Some(5)), ("First", Some(1)), ("Third", None)] {
        groups.push(
            tx.insert_category_group(NewCategoryGroup {
                name: CategoryGroupName::parse(name).unwrap(),
                position,
                created_at: at(0),
            })
            .await
            .unwrap(),
        );
    }
    assert_eq!(groups[2].position, 6, "appended after the last group");
    let first = groups[1].id;
    let rent = tx
        .insert_category(NewCategory {
            group: first,
            name: CategoryName::parse("Rent").unwrap(),
            kind: CategoryKind::Expense,
            position: None,
            created_at: at(0),
        })
        .await
        .unwrap();
    let salary = tx
        .insert_category(NewCategory {
            group: first,
            name: CategoryName::parse("Salary").unwrap(),
            kind: CategoryKind::Income,
            position: Some(0),
            created_at: at(0),
        })
        .await
        .unwrap();
    assert_eq!(rent.position, 0);
    let moved = tx
        .update_category(
            rent.id,
            CategoryChanges {
                position: Some(3),
                archived_at: Some(Some(at(4))),
                ..CategoryChanges::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(moved.position, 3);
    assert_eq!(moved.archived_at, Some(at(4)));
    let renamed = tx
        .update_category_group(
            first,
            CategoryGroupChanges {
                name: Some(CategoryGroupName::parse("Primary").unwrap()),
                ..CategoryGroupChanges::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(renamed.name.as_str(), "Primary");
    assert_eq!(renamed.categories.len(), 2);
    tx.commit().await.unwrap();

    let tree = store.list_categories().await.unwrap();
    let names: Vec<_> = tree.iter().map(|g| g.name.as_str()).collect();
    assert_eq!(names, ["Primary", "Second", "Third"]);
    let in_first: Vec<_> = tree[0].categories.iter().map(|c| c.id).collect();
    assert_eq!(in_first, [salary.id, rent.id]);
    assert_eq!(tree[1].categories, []);
    assert!(matches!(
        store
            .begin()
            .await
            .unwrap()
            .update_category(CategoryId::new(), CategoryChanges::default())
            .await,
        Err(StoreError::NotFound)
    ));
}
