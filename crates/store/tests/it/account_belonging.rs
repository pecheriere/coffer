//! Migration 0022, run by the real `Migrator` on a populated database, and whose
//! money an account is.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::Path;

use coffer_domain::accounts::{
    AccountKind, AccountName, BalanceSource, BelongsTo, Beneficiary, LiquidityClass,
};
use coffer_domain::ids::AccountId;
use coffer_domain::money::Currency;
use coffer_store::{AccountChanges, BalanceSnapshot, NewAccount, Store};
use jiff::Timestamp;
use jiff::civil::date;
use sqlx::migrate::Migrator;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{AssertSqlSafe, Row, SqlSafeStr, SqlitePool};
use tempfile::TempDir;

/// Every table a populated database holds rows in, with how many the seed puts there.
const SEEDED: [(&str, usize); 19] = [
    ("account_daily_values", 2),
    ("accounts", 15),
    ("agent_token_grants", 2),
    ("agent_tokens", 1),
    ("balance_snapshots", 2),
    ("categories", 1),
    ("category_groups", 1),
    ("connections", 1),
    ("duplicate_candidates", 1),
    ("kept_balances", 1),
    ("members", 1),
    ("net_worth_state", 1),
    ("planned_item_settlements", 1),
    ("planned_items", 2),
    ("provider_accounts", 2),
    ("sync_runs", 1),
    ("transaction_sources", 1),
    ("transaction_splits", 2),
    ("transactions", 2),
];

const FIRST: &str = "019a0000-0000-7000-8000-000000000001";

/// One row of each kind `0021` knew, one closed, one out of net worth and one with a
/// chosen class, and something in every table that refers to them.
const SEED: &str = "
INSERT INTO members (id, username, display_name, role, created_at)
    VALUES ('member-a', 'member-a', 'Member A', 'admin', 1);
INSERT INTO connections (id, provider, label, owner_id, credentials, provider_state, status,
        last_error, last_attempt_at, last_success_at, created_at, reauth_generation)
    VALUES ('bank', 'simplefin', 'Bank', 'member-a', x'0102ff', 'cursor', 'active', 'late', 2, 3,
        1, 4);
INSERT INTO accounts (id, name, kind, currency, liquidity, liquidity_overridden,
        include_in_net_worth, include_in_forecast, closed_at, created_at) VALUES
    ('019a0000-0000-7000-8000-000000000001', 'Everyday', 'checking', 'USD', 'cash', 0, 1, 1, NULL, 10),
    ('019a0000-0000-7000-8000-000000000002', 'Rainy day', 'savings', 'EUR', 'cash', 0, 1, 1, NULL, 11),
    ('019a0000-0000-7000-8000-000000000003', 'Card', 'credit_card', 'USD', 'short_term_liability', 0, 1, 1, NULL, 12),
    ('019a0000-0000-7000-8000-000000000004', 'Car loan', 'loan', 'USD', 'long_term_liability', 0, 1, 1, NULL, 13),
    ('019a0000-0000-7000-8000-000000000005', 'Home loan', 'mortgage', 'USD', 'long_term_liability', 0, 1, 1, NULL, 14),
    ('019a0000-0000-7000-8000-000000000006', 'Shares', 'brokerage', 'USD', 'invested', 0, 1, 0, NULL, 15),
    ('019a0000-0000-7000-8000-000000000007', 'Pension', 'retirement', 'USD', 'invested', 0, 1, 0, NULL, 16),
    ('019a0000-0000-7000-8000-000000000008', 'Options', 'equity_comp', 'USD', 'invested', 0, 1, 0, NULL, 17),
    ('019a0000-0000-7000-8000-000000000009', 'House', 'property', 'USD', 'illiquid', 0, 1, 0, NULL, 18),
    ('019a0000-0000-7000-8000-00000000000a', 'Car', 'vehicle', 'USD', 'illiquid', 0, 1, 0, NULL, 19),
    ('019a0000-0000-7000-8000-00000000000b', 'Art', 'other_asset', 'USD', 'illiquid', 0, 1, 0, NULL, 20),
    ('019a0000-0000-7000-8000-00000000000c', 'Owed', 'other_liability', 'USD', 'long_term_liability', 0, 1, 0, NULL, 21),
    ('019a0000-0000-7000-8000-00000000000d', 'Closed', 'checking', 'USD', 'cash', 0, 1, 1, 99, 22),
    ('019a0000-0000-7000-8000-00000000000e', 'Left out', 'checking', 'USD', 'cash', 0, 0, 1, NULL, 23),
    ('019a0000-0000-7000-8000-00000000000f', 'Chosen class', 'brokerage', 'USD', 'cash', 1, 1, 1, NULL, 24);
INSERT INTO provider_accounts (id, connection_id, provider_account_id, name, institution,
        currency_raw, kind_hint, account_id, last_balance_raw, last_balance_on, error,
        first_seen_at, last_seen_at, last_applied_at, held_syncs, hold_released,
        released_records, release_withdrawn, held_records) VALUES
    ('feed-bound', 'bank', 'ext-1', 'Bound', 'Institution', 'USD', 'savings',
        '019a0000-0000-7000-8000-000000000001', '12.34', '2026-10-01', 'odd', 5, 6, 7, 2, 1, 3, 0, 4),
    ('feed-free', 'bank', 'ext-2', 'Free', NULL, 'usd', NULL, NULL, NULL, NULL, NULL, 8, 9, NULL,
        0, 0, 0, 0, 0);
INSERT INTO balance_snapshots (account_id, on_date, balance_minor, source, recorded_at) VALUES
    ('019a0000-0000-7000-8000-000000000001', '2026-10-01', 1234, 'sync', 30),
    ('019a0000-0000-7000-8000-00000000000d', '2026-09-01', -5, 'manual', 31);
INSERT INTO kept_balances (provider_account_id, on_date, balance_minor, currency, recorded_at)
    VALUES ('feed-free', '2026-10-02', 777, 'USD', 32);
INSERT INTO account_daily_values (account_id, on_date, balance_minor, balance_on, balance_source,
        display_currency, display_minor, rate_on, rate_leg, conversion) VALUES
    ('019a0000-0000-7000-8000-000000000001', '2026-10-01', 1234, '2026-10-01', 'sync', 'USD', 1234,
        NULL, NULL, 'same'),
    ('019a0000-0000-7000-8000-000000000002', '2026-10-01', 50, '2026-09-30', 'manual', 'USD', 55,
        '2026-09-30', 'held', 'converted');
INSERT INTO category_groups (id, name, position, created_at) VALUES ('group', 'Home', 0, 1);
INSERT INTO categories (id, group_id, name, kind, position, created_at)
    VALUES ('category', 'group', 'Rent', 'expense', 0, 1);
INSERT INTO planned_items (id, name, category_id, amount_minor, currency, direction,
        account_hint_id, active, item_type, one_off_on, every, unit, anchor_on, spending,
        goal_start_on, goal_due_on, goal_completed, created_at, updated_at) VALUES
    ('bill', 'Rent', 'category', 100, 'USD', 'outflow', '019a0000-0000-7000-8000-000000000001', 1,
        'recurring', NULL, 1, 'months', '2026-01-01', 'bill', NULL, NULL, NULL, 40, 41),
    ('goal', 'Roof', 'category', 900, 'USD', 'outflow', NULL, 1,
        'goal', NULL, NULL, NULL, NULL, NULL, '2026-01-01', '2027-01-01', 0, 42, 43);
INSERT INTO planned_item_settlements (item_id, month, kind, settled_by_member, settled_at)
    VALUES ('bill', '2026-10', 'paid', 'member-a', 44);
INSERT INTO transactions (id, account_id, origin, posted_on, pending, amount_minor, currency,
        raw_description, payee_edited, review, flag_possible_duplicate, flag_source_removed,
        flag_split_mismatch, created_at, updated_at) VALUES
    ('txn-1', '019a0000-0000-7000-8000-000000000001', 'imported', '2026-10-01', 0, -100, 'USD',
        'Rent', 0, 'unreviewed', 1, 0, 0, 50, 51),
    ('txn-2', '019a0000-0000-7000-8000-00000000000d', 'manual', '2026-09-01', 1, -100, 'USD',
        'Rent again', 0, 'unreviewed', 0, 0, 0, 52, 53);
INSERT INTO transaction_sources (id, connection_id, provider_account_ref, provider_txn_id,
        transaction_id, posted_on, amount_raw, amount_minor, description, pending, fingerprint,
        fingerprint_version, first_seen_at, last_seen_at) VALUES
    ('source', 'bank', 'feed-bound', 'ext-txn', 'txn-1', '2026-10-01', '-1.00', -100, 'Rent', 0,
        'print', 1, 54, 55);
INSERT INTO transaction_splits (id, transaction_id, category_id, minor_units, position, goal_id)
    VALUES ('split-1', 'txn-1', 'category', -60, 0, NULL),
        ('split-2', 'txn-1', 'category', -40, 1, 'goal');
INSERT INTO duplicate_candidates (transaction_id, candidate_id) VALUES ('txn-1', 'txn-2');
INSERT INTO sync_runs (id, connection_id, trigger, status, queued_at)
    VALUES ('run', 'bank', 'manual', 'succeeded', 60);
INSERT INTO agent_tokens (id, member_id, name, scope, token_hash, prefix, created_at)
    VALUES ('token', 'member-a', 'Helper', 'read', x'aa', 'cof', 70);
INSERT INTO agent_token_grants (token_id, target, level) VALUES
    ('token', '019a0000-0000-7000-8000-000000000001', 'balances'),
    ('token', 'all', 'transactions');
";

fn at() -> Timestamp {
    Timestamp::from_second(1_800_000_000).unwrap()
}

/// A file database connected as `Store::open` connects.
async fn connect(path: &Path) -> SqlitePool {
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(path)
                .create_if_missing(true)
                .journal_mode(SqliteJournalMode::Wal)
                .synchronous(SqliteSynchronous::Normal)
                .foreign_keys(true)
                .pragma("temp_store", "memory"),
        )
        .await
        .unwrap()
}

fn every_migration() -> Migrator {
    sqlx::migrate!("./migrations")
}

fn migrations_up_to(last: i64) -> Migrator {
    let mut upto = every_migration();
    upto.migrations = Cow::Owned(
        upto.iter()
            .filter(|migration| migration.version <= last)
            .cloned()
            .collect(),
    );
    upto
}

fn before_belonging() -> Migrator {
    migrations_up_to(21)
}

/// A database at `0021`, populated, connected with foreign keys on.
async fn seeded() -> (TempDir, SqlitePool) {
    let dir = tempfile::tempdir().unwrap();
    let pool = connect(&dir.path().join("coffer.db")).await;
    before_belonging().run(&pool).await.unwrap();
    sqlx::raw_sql(SEED).execute(&pool).await.unwrap();
    (dir, pool)
}

async fn migrated_empty() -> (TempDir, SqlitePool) {
    let dir = tempfile::tempdir().unwrap();
    let pool = connect(&dir.path().join("coffer.db")).await;
    every_migration().run(&pool).await.unwrap();
    (dir, pool)
}

async fn strings(pool: &SqlitePool, sql: &str) -> Vec<String> {
    sqlx::query(AssertSqlSafe(sql.to_owned()))
        .fetch_all(pool)
        .await
        .unwrap()
        .iter()
        .map(|row| row.get::<String, _>(0))
        .collect()
}

async fn columns_of(pool: &SqlitePool, table: &str) -> Vec<String> {
    strings(
        pool,
        &format!("SELECT name FROM pragma_table_info('{table}') ORDER BY cid"),
    )
    .await
}

/// Every named column of every row, each as an SQL literal so a type change shows too.
async fn dump(pool: &SqlitePool, table: &str, columns: &[String]) -> Vec<String> {
    let quoted: Vec<String> = columns.iter().map(|c| format!("quote(\"{c}\")")).collect();
    strings(
        pool,
        &format!(
            "SELECT {} FROM \"{table}\" ORDER BY 1",
            quoted.join(" || ' | ' || ")
        ),
    )
    .await
}

/// Every table and its columns and rows, but for the migrator's own.
async fn dump_all(pool: &SqlitePool) -> BTreeMap<String, (Vec<String>, Vec<String>)> {
    let tables = strings(
        pool,
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' \
         AND name <> '_sqlx_migrations'",
    )
    .await;
    let mut all = BTreeMap::new();
    for table in tables {
        let columns = columns_of(pool, &table).await;
        let rows = dump(pool, &table, &columns).await;
        all.insert(table, (columns, rows));
    }
    all
}

async fn count(pool: &SqlitePool, sql: &str) -> i64 {
    sqlx::query(AssertSqlSafe(sql.to_owned()))
        .fetch_one(pool)
        .await
        .unwrap()
        .get::<i64, _>(0)
}

async fn run(pool: &SqlitePool, statement: &str) -> Result<(), sqlx::Error> {
    sqlx::query(AssertSqlSafe(statement.to_owned()))
        .execute(pool)
        .await
        .map(|_| ())
}

async fn refusal(pool: &SqlitePool, statement: &str) -> sqlx::Error {
    match run(pool, statement).await {
        Ok(()) => panic!("accepted: {statement}"),
        Err(refused) => refused,
    }
}

async fn assert_refused_by_check(pool: &SqlitePool, statement: &str) {
    let refused = refusal(pool, statement).await;
    let is_check = refused
        .as_database_error()
        .is_some_and(sqlx::error::DatabaseError::is_check_violation);
    assert!(is_check, "{statement}: {refused}");
}

async fn assert_refused_by_foreign_key(pool: &SqlitePool, statement: &str) {
    let refused = refusal(pool, statement).await;
    let is_foreign_key = refused
        .as_database_error()
        .is_some_and(sqlx::error::DatabaseError::is_foreign_key_violation);
    assert!(is_foreign_key, "{statement}: {refused}");
}

/// An insert into `accounts`; `values` are its name, kind, currency, liquidity, the three
/// flags, `belongs_to` and `beneficiary`, as SQL.
fn holding(values: &str) -> String {
    format!(
        "INSERT INTO accounts (id, name, kind, currency, liquidity, liquidity_overridden, \
         include_in_net_worth, include_in_forecast, belongs_to, beneficiary, created_at) \
         VALUES (lower(hex(randomblob(18))), {values}, 0)"
    )
}

/// An insert into `provider_accounts`; `values` are its external id, `kind_hint`,
/// `last_balance_raw` and `last_balance_on`, as SQL.
fn feed(values: &str) -> String {
    format!(
        "INSERT INTO provider_accounts (id, connection_id, provider_account_id, kind_hint, \
         last_balance_raw, last_balance_on, name, currency_raw, first_seen_at, last_seen_at) \
         VALUES (lower(hex(randomblob(18))), 'bank', {values}, 'Feed', 'USD', 0, 0)"
    )
}

#[tokio::test]
async fn migration_keeps_every_row() {
    let (_dir, pool) = seeded().await;
    let before = dump_all(&pool).await;
    for (table, rows) in SEEDED {
        assert_eq!(before[table].1.len(), rows, "the seed fills {table}");
    }

    // This migration alone: a later one may add a table of its own.
    migrations_up_to(22).run(&pool).await.unwrap();

    let after = dump_all(&pool).await;
    assert_eq!(
        before.keys().collect::<Vec<_>>(),
        after.keys().collect::<Vec<_>>(),
        "the same tables, and no holding table left"
    );

    // Whatever later migrations add, every row stays.
    every_migration().run(&pool).await.unwrap();
    for (table, (columns, rows)) in &before {
        assert_eq!(
            &dump(&pool, table, columns).await,
            rows,
            "{table} keeps every row and value"
        );
    }
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM accounts WHERE belongs_to = 'household' \
             AND belongs_to_overridden = 0 AND beneficiary IS NULL"
        )
        .await,
        15,
        "every row stays the household's, and nothing is guessed"
    );
    assert_eq!(
        count(&pool, "SELECT count(*) FROM pragma_foreign_key_check").await,
        0,
        "no row is orphaned"
    );
    assert_refused_by_foreign_key(
        &pool,
        "INSERT INTO balance_snapshots (account_id, on_date, balance_minor, source, recorded_at) \
         VALUES ('nowhere', '2026-10-05', 1, 'manual', 0)",
    )
    .await;
    assert_refused_by_foreign_key(
        &pool,
        "INSERT INTO kept_balances (provider_account_id, on_date, balance_minor, currency, \
         recorded_at) VALUES ('nowhere', '2026-10-05', 1, 'USD', 0)",
    )
    .await;
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM sqlite_master WHERE type = 'index' \
             AND name = 'provider_accounts_account_id' AND tbl_name = 'provider_accounts'"
        )
        .await,
        1,
        "the index on the binding is back"
    );
}

/// Everything but the two rebuilt tables is byte for byte what it was, so no child table
/// was rewritten; the two rebuilt tables keep their columns, keys and strictness.
#[tokio::test]
async fn migration_keeps_the_shape_of_the_schema() {
    let (_dir, pool) = seeded().await;
    let schema = "SELECT type || ' ' || name || ' ' || tbl_name || ' ' || coalesce(sql, '') \
                  FROM sqlite_master WHERE tbl_name NOT IN \
                  ('accounts', 'provider_accounts', '_sqlx_migrations') ORDER BY name";
    let columns = |table: &str| {
        format!(
            "SELECT name || ' ' || type || ' ' || \"notnull\" || ' ' || \
             coalesce(dflt_value, 'none') || ' ' || pk FROM pragma_table_info('{table}') \
             ORDER BY cid"
        )
    };
    let keys = "SELECT \"table\" || ' ' || \"from\" || ' ' || \"to\" || ' ' || on_delete \
                FROM pragma_foreign_key_list('provider_accounts') ORDER BY \"from\"";
    let indexes = "SELECT name FROM sqlite_master WHERE type = 'index' \
                   AND tbl_name IN ('accounts', 'provider_accounts') ORDER BY name";
    let others = strings(&pool, schema).await;
    let held = strings(&pool, &columns("accounts")).await;
    let fed = strings(&pool, &columns("provider_accounts")).await;
    let foreign = strings(&pool, keys).await;
    let found = strings(&pool, indexes).await;

    migrations_up_to(22).run(&pool).await.unwrap();

    assert_eq!(strings(&pool, schema).await, others);
    assert_eq!(strings(&pool, &columns("provider_accounts")).await, fed);
    let mut grown = held;
    grown.extend(
        [
            "belongs_to TEXT 1 'household' 0",
            "belongs_to_overridden INTEGER 1 0 0",
            "beneficiary TEXT 0 none 0",
        ]
        .map(String::from),
    );
    assert_eq!(strings(&pool, &columns("accounts")).await, grown);
    assert_eq!(strings(&pool, keys).await, foreign);
    assert_eq!(foreign.len(), 2, "both references of a provider row");
    assert_eq!(strings(&pool, indexes).await, found);
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM pragma_table_list \
             WHERE name IN ('accounts', 'provider_accounts') AND strict = 1"
        )
        .await,
        2,
        "both tables stay STRICT"
    );
}

#[tokio::test]
async fn migration_keeps_every_old_check() {
    let (_dir, pool) = seeded().await;
    every_migration().run(&pool).await.unwrap();
    for values in [
        "'', 'checking', 'USD', 'cash', 0, 1, 1, 'household', NULL",
        "printf('%101s', 'x'), 'checking', 'USD', 'cash', 0, 1, 1, 'household', NULL",
        "'Name', 'cheque', 'USD', 'cash', 0, 1, 1, 'household', NULL",
        "'Name', 'checking', 'US', 'cash', 0, 1, 1, 'household', NULL",
        "'Name', 'checking', 'USD', 'liquid', 0, 1, 1, 'household', NULL",
        "'Name', 'checking', 'USD', 'cash', 2, 1, 1, 'household', NULL",
        "'Name', 'checking', 'USD', 'cash', 0, 2, 1, 'household', NULL",
        "'Name', 'checking', 'USD', 'cash', 0, 1, 2, 'household', NULL",
    ] {
        assert_refused_by_check(&pool, &holding(values)).await;
    }
    for values in [
        "'', NULL, NULL, NULL",
        "'ext-3', 'cheque', NULL, NULL",
        "'ext-3', NULL, '1.00', NULL",
        "'ext-3', NULL, NULL, '2026-10-01'",
        "'ext-3', NULL, '1.00', 'October'",
    ] {
        assert_refused_by_check(&pool, &feed(values)).await;
    }
    let duplicate = run(&pool, &feed("'ext-1', NULL, NULL, NULL"))
        .await
        .unwrap_err();
    assert!(
        duplicate.as_database_error().unwrap().is_unique_violation(),
        "one provider row per external id in a connection"
    );
}

/// `0022` with `from` in its text replaced by `to`, the rest of the migrations as they are.
fn tampered(from: &str, to: &str) -> Migrator {
    let mut upto = every_migration();
    upto.migrations = Cow::Owned(
        upto.iter()
            .map(|migration| {
                let mut migration = migration.clone();
                if migration.version == 22 {
                    let sql = migration.sql.as_str();
                    assert!(sql.contains(from), "{from}");
                    migration.sql = AssertSqlSafe(sql.replace(from, to)).into_sql_str();
                }
                migration
            })
            .collect(),
    );
    upto
}

/// A row with something referring to it that doesn't come back fails the whole migration
/// at `COMMIT`, under the real migrator's transaction: nothing of it stays.
#[tokio::test]
async fn a_lost_row_stops_the_migration_and_changes_nothing() {
    for (from, to) in [
        (
            "created_at FROM accounts_held;",
            "created_at FROM accounts_held WHERE name <> 'Everyday';",
        ),
        (
            "SELECT * FROM provider_accounts_held;",
            "SELECT * FROM provider_accounts_held WHERE name <> 'Free';",
        ),
    ] {
        let (_dir, pool) = seeded().await;
        let before = dump_all(&pool).await;

        let refused = tampered(from, to).run(&pool).await;

        assert!(refused.is_err(), "{to}");
        assert_eq!(dump_all(&pool).await, before, "nothing of it stays");
        assert_eq!(
            count(&pool, "SELECT max(version) FROM _sqlx_migrations").await,
            21,
            "and it isn't marked as applied"
        );
    }
}

#[tokio::test]
async fn every_kind_passes_the_tables() {
    let (_dir, pool) = migrated_empty().await;
    run(
        &pool,
        "INSERT INTO members (id, username, display_name, role, created_at) \
         VALUES ('member-a', 'member-a', '', 'admin', 0)",
    )
    .await
    .unwrap();
    run(
        &pool,
        "INSERT INTO connections (id, provider, label, owner_id, status, created_at) \
         VALUES ('bank', 'simplefin', 'Bank', 'member-a', 'active', 0)",
    )
    .await
    .unwrap();
    for kind in AccountKind::ALL {
        let kind = kind.as_str();
        run(
            &pool,
            &holding(&format!(
                "'Name', '{kind}', 'USD', 'cash', 0, 1, 1, 'household', NULL"
            )),
        )
        .await
        .expect(kind);
        run(&pool, &feed(&format!("'{kind}', '{kind}', NULL, NULL")))
            .await
            .expect(kind);
    }
}

#[tokio::test]
async fn every_value_of_belongs_to_passes() {
    let (_dir, pool) = migrated_empty().await;
    for belongs in [
        BelongsTo::Household,
        BelongsTo::SomeoneElse,
        BelongsTo::GivenAway,
    ] {
        let belongs = belongs.as_str();
        run(
            &pool,
            &holding(&format!(
                "'Name', 'checking', 'USD', 'cash', 0, 0, 0, '{belongs}', NULL"
            )),
        )
        .await
        .expect(belongs);
    }
}

#[tokio::test]
async fn an_unknown_belongs_to_is_refused() {
    let (_dir, pool) = migrated_empty().await;
    assert_refused_by_check(
        &pool,
        &holding("'Name', 'checking', 'USD', 'cash', 0, 0, 0, 'nobody', NULL"),
    )
    .await;
}

#[tokio::test]
async fn not_the_household_can_t_be_counted() {
    let (_dir, pool) = migrated_empty().await;
    for values in [
        "'Name', 'education', 'USD', 'invested', 0, 1, 0, 'someone_else', NULL",
        "'Name', 'education', 'USD', 'invested', 0, 0, 1, 'someone_else', NULL",
        "'Name', 'charitable', 'USD', 'invested', 0, 1, 0, 'given_away', NULL",
        "'Name', 'charitable', 'USD', 'invested', 0, 0, 1, 'given_away', NULL",
    ] {
        assert_refused_by_check(&pool, &holding(values)).await;
    }
    run(
        &pool,
        &holding("'Name', 'education', 'USD', 'invested', 0, 0, 0, 'someone_else', NULL"),
    )
    .await
    .unwrap();
    for flag in ["include_in_net_worth", "include_in_forecast"] {
        assert_refused_by_check(&pool, &format!("UPDATE accounts SET {flag} = 1")).await;
    }
    run(
        &pool,
        &holding("'Name', 'checking', 'USD', 'cash', 0, 1, 1, 'household', NULL"),
    )
    .await
    .unwrap();
    assert_refused_by_check(
        &pool,
        "UPDATE accounts SET belongs_to = 'given_away' WHERE belongs_to = 'household'",
    )
    .await;
}

#[tokio::test]
async fn the_household_has_no_beneficiary() {
    let (_dir, pool) = migrated_empty().await;
    assert_refused_by_check(
        &pool,
        &holding("'Name', 'checking', 'USD', 'cash', 0, 1, 1, 'household', 'Child A'"),
    )
    .await;
    run(
        &pool,
        &holding("'Name', 'checking', 'USD', 'cash', 0, 1, 1, 'household', NULL"),
    )
    .await
    .unwrap();
    assert_refused_by_check(&pool, "UPDATE accounts SET beneficiary = 'Child A'").await;
}

#[tokio::test]
async fn belongs_to_overridden_is_a_flag() {
    let (_dir, pool) = migrated_empty().await;
    let with = |flag: i64| {
        format!(
            "INSERT INTO accounts (id, name, kind, currency, liquidity, liquidity_overridden, \
             include_in_net_worth, include_in_forecast, belongs_to_overridden, created_at) \
             VALUES (lower(hex(randomblob(18))), 'Name', 'checking', 'USD', 'cash', 0, 1, 1, \
             {flag}, 0)"
        )
    };
    run(&pool, &with(1)).await.unwrap();
    assert_refused_by_check(&pool, &with(2)).await;
}

#[tokio::test]
async fn a_stored_beneficiary_respects_its_maximum() {
    let (_dir, pool) = migrated_empty().await;
    let with = |label: &str| {
        holding(&format!(
            "'Name', 'custodial', 'USD', 'invested', 0, 0, 0, 'someone_else', {label}"
        ))
    };
    run(&pool, &with("printf('%64s', 'x')")).await.unwrap();
    assert_refused_by_check(&pool, &with("printf('%65s', 'x')")).await;
    assert_refused_by_check(&pool, &with("''")).await;
}

fn college_fund() -> NewAccount {
    NewAccount {
        id: AccountId::new(),
        name: AccountName::parse("College fund").unwrap(),
        kind: AccountKind::Education,
        currency: Currency::parse("USD").unwrap(),
        liquidity: LiquidityClass::Invested,
        liquidity_overridden: false,
        include_in_net_worth: false,
        include_in_forecast: false,
        created_at: at(),
        belongs_to: BelongsTo::SomeoneElse,
        belongs_to_overridden: true,
        beneficiary: Some(Beneficiary::parse("Child A").unwrap()),
    }
}

/// A record's or a change's `Debug` can reach a log line; a beneficiary may be a child's name.
#[tokio::test]
async fn debug_output_holds_no_beneficiary() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("coffer.db")).await.unwrap();
    let new = college_fund();
    let mut tx = store.begin().await.unwrap();
    let inserted = tx.insert_account(new.clone()).await.unwrap();
    let changes = AccountChanges {
        beneficiary: Some(Some(Beneficiary::parse("Child A").unwrap())),
        ..AccountChanges::default()
    };
    for shown in [
        format!("{inserted:?}"),
        format!("{new:?}"),
        format!("{changes:?}"),
    ] {
        assert!(!shown.contains("Child A"), "the label stays out");
        assert!(shown.contains("<redacted>"), "and says it was held back");
    }
}

#[tokio::test]
async fn belonging_roundtrips() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("coffer.db")).await.unwrap();
    let new = college_fund();
    let id = new.id;
    let mut tx = store.begin().await.unwrap();
    let inserted = tx.insert_account(new).await.unwrap();
    assert_eq!(
        tx.get_account(id).await.unwrap(),
        inserted,
        "reads as written"
    );
    assert_eq!(inserted.belongs_to, BelongsTo::SomeoneElse);
    assert!(inserted.belongs_to_overridden);
    assert_eq!(
        inserted.beneficiary.as_ref().map(Beneficiary::as_str),
        Some("Child A")
    );

    let untouched = tx
        .update_account(id, AccountChanges::default())
        .await
        .unwrap();
    assert_eq!(untouched, inserted, "no change leaves all three");

    let changed = tx
        .update_account(
            id,
            AccountChanges {
                belongs_to: Some(BelongsTo::GivenAway),
                belongs_to_overridden: Some(false),
                beneficiary: Some(Some(Beneficiary::parse("Family fund").unwrap())),
                ..AccountChanges::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(tx.get_account(id).await.unwrap(), changed);
    assert_eq!(changed.belongs_to, BelongsTo::GivenAway);
    assert!(!changed.belongs_to_overridden);
    assert_eq!(
        changed.beneficiary.as_ref().map(Beneficiary::as_str),
        Some("Family fund")
    );

    let cleared = tx
        .update_account(
            id,
            AccountChanges {
                beneficiary: Some(None),
                ..AccountChanges::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(cleared.beneficiary, None, "Some(None) clears");
    assert_eq!(cleared.belongs_to, BelongsTo::GivenAway, "and only that");
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn changing_belonging_writes_no_daily_row() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("coffer.db");
    let store = Store::open(&path).await.unwrap();
    let mut new = college_fund();
    new.belongs_to = BelongsTo::Household;
    new.belongs_to_overridden = false;
    new.beneficiary = None;
    let id = new.id;
    let mut tx = store.begin().await.unwrap();
    tx.insert_account(new).await.unwrap();
    tx.upsert_balance(BalanceSnapshot {
        account: id,
        on: date(2026, 10, 1),
        balance_minor: 100,
        source: BalanceSource::Manual,
        recorded_at: at(),
    })
    .await
    .unwrap();
    tx.commit().await.unwrap();
    store.rebuild_net_worth(date(2026, 10, 3)).await.unwrap();
    let pool = connect(&path).await;
    let columns = columns_of(&pool, "account_daily_values").await;
    let before = dump(&pool, "account_daily_values", &columns).await;
    assert_eq!(before.len(), 3, "the table holds the three days");

    let mut tx = store.begin().await.unwrap();
    tx.update_account(
        id,
        AccountChanges {
            belongs_to: Some(BelongsTo::SomeoneElse),
            belongs_to_overridden: Some(true),
            beneficiary: Some(Some(Beneficiary::parse("Child A").unwrap())),
            ..AccountChanges::default()
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(
        dump(&pool, "account_daily_values", &columns).await,
        before,
        "belonging applies at read"
    );
}

#[tokio::test]
async fn the_store_reads_a_migrated_row() {
    let (dir, pool) = seeded().await;
    every_migration().run(&pool).await.unwrap();
    pool.close().await;
    let store = Store::open(&dir.path().join("coffer.db")).await.unwrap();
    let mut tx = store.begin().await.unwrap();
    let read = tx.get_account(FIRST.parse().unwrap()).await.unwrap();
    assert_eq!(read.belongs_to, BelongsTo::Household);
    assert!(!read.belongs_to_overridden);
    assert_eq!(read.beneficiary, None);
}
