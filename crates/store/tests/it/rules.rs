//! Migration 0029 on a populated database, and reading what a manual run reads.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use std::borrow::Cow;

use coffer_domain::accounts::{AccountKind, AccountName, BelongsTo, LiquidityClass};
use coffer_domain::identity::{DisplayName, Role, Username};
use coffer_domain::ids::{AccountId, MemberId, TransactionId};
use coffer_domain::money::Currency;
use coffer_domain::policy::AccountScope;
use coffer_domain::transactions::{Description, TransactionOrigin};
use coffer_store::{
    AssigneeChange, Attribution, NewAccount, NewMember, NewTransaction, Store, UserFieldChanges,
};
use jiff::Timestamp;
use jiff::civil::date;
use sqlx::SqlitePool;
use sqlx::migrate::Migrator;
use sqlx::sqlite::SqliteConnectOptions;

const SEED: &str = "
INSERT INTO members (id, username, display_name, role, created_at)
    VALUES ('member-a', 'member-a', 'Member A', 'admin', 1);
INSERT INTO agent_tokens (id, member_id, name, scope, token_hash, prefix, created_at)
    VALUES ('token', 'member-a', 'Helper', 'write', x'aa', 'cof', 2);
INSERT INTO accounts (id, name, kind, currency, liquidity, liquidity_overridden,
        include_in_net_worth, include_in_forecast, created_at)
    VALUES ('account', 'Checking', 'checking', 'USD', 'cash', 0, 1, 1, 3);
INSERT INTO transactions (id, account_id, origin, posted_on, pending, amount_minor, currency,
        raw_description, payee_edited, assignee_id, review, flag_possible_duplicate,
        flag_source_removed, flag_split_mismatch, created_at, updated_at) VALUES
    ('assigned', 'account', 'manual', '2026-10-01', 0, -1, 'USD', 'A', 0, 'member-a',
        'unreviewed', 0, 0, 0, 4, 4),
    ('unassigned', 'account', 'manual', '2026-10-01', 0, -1, 'USD', 'B', 0, NULL,
        'unreviewed', 0, 0, 0, 4, 4),
    ('untouched', 'account', 'manual', '2026-10-01', 0, -1, 'USD', 'C', 0, NULL,
        'unreviewed', 0, 0, 0, 4, 4);
INSERT INTO audit_events (id, at, actor_kind, actor_member_id, actor_agent_id, action,
        entity_type, entity_id, changes_json, changes_version) VALUES
    ('019a0000-0000-7000-8000-000000000001', 10, 'member', 'member-a', NULL,
        'transaction.update', 'transaction', 'assigned',
        '{\"assignee\":{\"before\":null,\"after\":\"member-a\"}}', 2),
    ('019a0000-0000-7000-8000-000000000002', 20, 'member', 'member-a', NULL,
        'transaction.update', 'transaction', 'unassigned',
        '{\"assignee\":{\"before\":null,\"after\":\"member-a\"}}', 2),
    ('019a0000-0000-7000-8000-000000000003', 30, 'member', 'member-a', 'token',
        'transaction.update', 'transaction', 'unassigned',
        '{\"assignee\":{\"before\":\"member-a\",\"after\":null}}', 2),
    ('019a0000-0000-7000-8000-000000000004', 40, 'member', 'member-a', NULL,
        'transaction.update', 'transaction', 'unassigned',
        '{\"notes\":{\"before\":null,\"after\":\"later\"}}', 2),
    ('019a0000-0000-7000-8000-000000000005', 50, 'member', 'member-a', NULL,
        'transaction.update', 'transaction', 'untouched',
        '{\"notes\":{\"before\":null,\"after\":\"none\"}}', 2);
";

fn migrations_up_to(last: i64) -> Migrator {
    let mut upto = sqlx::migrate!("./migrations");
    upto.migrations = Cow::Owned(
        upto.iter()
            .filter(|migration| migration.version <= last)
            .cloned()
            .collect(),
    );
    upto
}

async fn assigned_columns(
    pool: &SqlitePool,
    id: &str,
) -> (Option<String>, Option<String>, Option<i64>) {
    sqlx::query_as(
        "SELECT assigned_by_member, assigned_by_agent, assigned_at FROM transactions WHERE id = ?",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn the_migration_reads_who_changed_an_assignee_from_the_audit_log() {
    let dir = tempfile::tempdir().unwrap();
    let pool = SqlitePool::connect_with(
        SqliteConnectOptions::new()
            .filename(dir.path().join("coffer.db"))
            .create_if_missing(true)
            .foreign_keys(true),
    )
    .await
    .unwrap();
    migrations_up_to(28).run(&pool).await.unwrap();
    sqlx::raw_sql(SEED).execute(&pool).await.unwrap();

    sqlx::migrate!("./migrations").run(&pool).await.unwrap();

    assert_eq!(
        assigned_columns(&pool, "assigned").await,
        (Some("member-a".to_owned()), None, Some(10))
    );
    assert_eq!(
        assigned_columns(&pool, "unassigned").await,
        (
            Some("member-a".to_owned()),
            Some("token".to_owned()),
            Some(30)
        ),
        "the last change of the assignee, not of anything else: someone cleared this one"
    );
    assert_eq!(
        assigned_columns(&pool, "untouched").await,
        (None, None, None)
    );
    let broken: i64 = sqlx::query_scalar("SELECT count(*) FROM pragma_foreign_key_check")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(broken, 0);
}

fn at(second: i64) -> Timestamp {
    Timestamp::from_second(1_800_000_000 + second).unwrap()
}

/// A store with one account, and a transaction in it for each of the given days.
async fn store_with_transactions(
    count: usize,
) -> (
    tempfile::TempDir,
    Store,
    AccountId,
    MemberId,
    Vec<TransactionId>,
) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("coffer.db")).await.unwrap();
    let mut tx = store.begin().await.unwrap();
    let member = tx
        .insert_member(NewMember {
            id: MemberId::new(),
            username: Username::parse("member").unwrap(),
            display_name: DisplayName::parse("Member").unwrap(),
            role: Role::Admin,
            created_at: at(0),
        })
        .await
        .unwrap()
        .id;
    let account = tx
        .insert_account(NewAccount {
            id: AccountId::new(),
            name: AccountName::parse("Checking").unwrap(),
            kind: AccountKind::Checking,
            currency: Currency::parse("USD").unwrap(),
            liquidity: LiquidityClass::Cash,
            liquidity_overridden: false,
            include_in_net_worth: true,
            include_in_forecast: true,
            created_at: at(0),
            belongs_to: BelongsTo::Household,
            belongs_to_overridden: false,
            beneficiary: None,
        })
        .await
        .unwrap()
        .id;
    let mut ids = Vec::new();
    for _ in 0..count {
        let record = tx
            .insert_transaction(NewTransaction {
                origin: TransactionOrigin::Manual,
                account,
                posted_on: date(2026, 10, 1),
                transacted_on: None,
                pending: false,
                amount_minor: -1,
                currency: Currency::parse("USD").unwrap(),
                raw_description: Description::parse("SHOP").unwrap(),
                imported_payee: None,
                memo: None,
                notes: None,
                created_at: at(1),
            })
            .await
            .unwrap();
        ids.push(record.id);
    }
    tx.commit().await.unwrap();
    (dir, store, account, member, ids)
}

#[tokio::test]
async fn open_fields_are_read_in_pages_within_the_scope_given() {
    let (_dir, store, account, _member, ids) = store_with_transactions(3).await;
    let first = store
        .transactions_with_open_fields(&AccountScope::All, None, None, 2)
        .await
        .unwrap();
    assert_eq!(
        first.iter().map(|record| record.id).collect::<Vec<_>>(),
        ids[..2]
    );
    let rest = store
        .transactions_with_open_fields(&AccountScope::All, None, Some(ids[1]), 2)
        .await
        .unwrap();
    assert_eq!(
        rest.iter().map(|record| record.id).collect::<Vec<_>>(),
        ids[2..]
    );
    for (scope, only) in [
        (AccountScope::Only(Vec::new()), None),
        (AccountScope::Only(vec![AccountId::new()]), None),
        (AccountScope::All, Some(AccountId::new())),
        (AccountScope::Only(vec![AccountId::new()]), Some(account)),
    ] {
        let found = store
            .transactions_with_open_fields(&scope, only, None, 10)
            .await
            .unwrap();
        assert_eq!(found.len(), 0);
    }
}

#[tokio::test]
async fn a_cleared_assignee_is_no_longer_open_and_who_cleared_it_is_kept() {
    let (_dir, store, _account, member, ids) = store_with_transactions(1).await;
    let by = Attribution {
        member: Some(member),
        agent: None,
        system: None,
        rule: None,
        at: at(2),
    };
    let mut tx = store.begin().await.unwrap();
    for value in [Some(member), None] {
        tx.update_user_fields(
            ids[0],
            UserFieldChanges {
                assignee: Some(AssigneeChange {
                    member: value,
                    by: by.clone(),
                }),
                ..UserFieldChanges::new(at(2))
            },
        )
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();

    let stored = store.get_transaction(ids[0]).await.unwrap();

    assert_eq!(stored.assignee, None);
    assert_eq!(stored.assigned, Some(by));
    let subject = stored.rule_subject();
    assert!(!subject.assignee_open);
    assert!(subject.category_open);
}
