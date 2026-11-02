//! What only the crate can see: the schema's own refusals, and the migration run
//! on a database that already holds data.

use coffer_domain::net_worth::NetWorthInterval;
use coffer_domain::policy::AccountScope;
use jiff::civil::date;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use crate::Store;

const COLUMNS: &str = "INSERT INTO account_daily_values (account_id, on_date, balance_minor, \
     balance_on, balance_source, display_currency, display_minor, rate_on, rate_leg, conversion) \
     VALUES ('holding', '2026-10-02', 5, ";

async fn with_account() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("coffer.db")).await.unwrap();
    sqlx::query(
        "INSERT INTO accounts (id, name, kind, currency, liquidity, liquidity_overridden, \
         include_in_net_worth, include_in_forecast, created_at) \
         VALUES ('holding', 'Holding', 'checking', 'USD', 'cash', 0, 1, 1, 0)",
    )
    .execute(&store.pool)
    .await
    .unwrap();
    (dir, store)
}

async fn accepted(store: &Store, values: &str) -> bool {
    let statement = sqlx::AssertSqlSafe(format!("{COLUMNS}{values})"));
    sqlx::query(statement).execute(&store.pool).await.is_ok()
}

#[tokio::test]
async fn the_schema_takes_each_state_in_its_shape() {
    let (_dir, store) = with_account().await;
    for (day, conversion) in [
        (
            "2026-10-02', 'manual', 'USD', 5, NULL, NULL, 'same'",
            "same",
        ),
        (
            "2026-10-02', 'manual', 'USD', 5, '2026-10-01', 'held', 'converted'",
            "converted",
        ),
        (
            "2026-10-02', 'manual', 'USD', NULL, NULL, 'display', 'missing'",
            "missing",
        ),
        (
            "2026-10-02', 'manual', 'USD', NULL, NULL, NULL, 'too_large'",
            "too_large",
        ),
        (
            "2026-10-02', 'manual', NULL, NULL, NULL, NULL, 'no_display_currency'",
            "no display currency",
        ),
    ] {
        assert!(accepted(&store, &format!("'{day}")).await, "{conversion}");
        sqlx::query("DELETE FROM account_daily_values")
            .execute(&store.pool)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn the_schema_refuses_a_row_of_the_wrong_shape() {
    let (_dir, store) = with_account().await;
    for (values, why) in [
        (
            "'2026-10-02', 'manual', 'USD', NULL, NULL, NULL, 'same'",
            "same without an amount",
        ),
        (
            "'2026-10-02', 'manual', 'USD', 5, '2026-10-01', 'held', 'same'",
            "same with a rate",
        ),
        (
            "'2026-10-02', 'manual', 'USD', 5, NULL, 'held', 'converted'",
            "converted without a rate date",
        ),
        (
            "'2026-10-02', 'manual', 'USD', 5, '2026-10-01', NULL, 'converted'",
            "converted without its leg",
        ),
        (
            "'2026-10-02', 'manual', 'USD', 5, NULL, 'held', 'missing'",
            "missing with an amount",
        ),
        (
            "'2026-10-02', 'manual', 'USD', NULL, NULL, NULL, 'missing'",
            "missing without its leg",
        ),
        (
            "'2026-10-02', 'manual', 'USD', 5, NULL, NULL, 'too_large'",
            "too large with an amount",
        ),
        (
            "'2026-10-02', 'manual', NULL, NULL, NULL, 'held', 'missing'",
            "no currency unless none is set",
        ),
        (
            "'2026-10-02', 'manual', 'USD', NULL, NULL, NULL, 'no_display_currency'",
            "a currency though none is set",
        ),
        (
            "'2026-10-03', 'manual', 'USD', 5, NULL, NULL, 'same'",
            "a balance from a later day",
        ),
        (
            "'2026-10-02', 'manual', 'USD', 5, '2026-10-03', 'held', 'converted'",
            "a rate from a later day",
        ),
    ] {
        assert!(!accepted(&store, values).await, "{why}");
    }
}

#[tokio::test]
async fn the_state_can_t_say_current_with_no_day() {
    let (_dir, store) = with_account().await;
    let refused = sqlx::query("UPDATE net_worth_state SET rebuild_needed = 0")
        .execute(&store.pool)
        .await;
    assert!(refused.is_err());
}

/// Migration 0021 on a database holding accounts, snapshots and rates: all of it
/// stays, the new table starts empty and asks for a rebuild, no number is read
/// until then, and the rebuild makes the rows from what was there.
#[tokio::test]
async fn migration_keeps_a_populated_database_and_asks_for_a_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(dir.path().join("coffer.db"))
                .create_if_missing(true)
                .foreign_keys(true),
        )
        .await
        .unwrap();
    let migrator = sqlx::migrate!("./migrations");
    for migration in migrator.iter().filter(|migration| migration.version < 21) {
        sqlx::raw_sql(migration.sql.clone())
            .execute(&pool)
            .await
            .unwrap();
    }
    sqlx::raw_sql(
        "INSERT INTO accounts (id, name, kind, currency, liquidity, liquidity_overridden, \
             include_in_net_worth, include_in_forecast, created_at) \
             VALUES ('019a0000-0000-7000-8000-000000000001', 'Holding', 'checking', 'USD', \
             'cash', 0, 1, 1, 0); \
         INSERT INTO balance_snapshots (account_id, on_date, balance_minor, source, recorded_at) \
             VALUES ('019a0000-0000-7000-8000-000000000001', '2026-10-01', 5, 'manual', 0), \
             ('019a0000-0000-7000-8000-000000000001', '2026-10-03', 7, 'sync', 0); \
         INSERT INTO fx_rates (quote, on_date, rate, source, fetched_at) \
             VALUES ('USD', '2026-10-01', '1.1', 'ecb', 0);",
    )
    .execute(&pool)
    .await
    .unwrap();

    for migration in migrator.iter().filter(|migration| migration.version == 21) {
        sqlx::raw_sql(migration.sql.clone())
            .execute(&pool)
            .await
            .unwrap();
    }

    let counts: (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM balance_snapshots), (SELECT count(*) FROM fx_rates), \
         (SELECT count(*) FROM account_daily_values)",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(counts, (2, 1, 0));

    let store = Store { pool };
    let through = date(2026, 10, 4);
    let waiting = store
        .net_worth_data(
            &AccountScope::All,
            date(2026, 10, 1),
            through,
            NetWorthInterval::Day,
        )
        .await
        .unwrap();
    assert!(waiting.state.rebuild_needed);
    assert_eq!(waiting.state.filled_through, None);
    assert_eq!(waiting.rows, []);

    let mut tx = store.begin().await.unwrap();
    assert_eq!(tx.rebuild_net_worth(through).await.unwrap(), 4);
    tx.commit().await.unwrap();
    let balances: Vec<i64> = store
        .net_worth_data(
            &AccountScope::All,
            date(2026, 10, 1),
            through,
            NetWorthInterval::Day,
        )
        .await
        .unwrap()
        .rows
        .iter()
        .map(|row| row.balance_minor)
        .collect();
    assert_eq!(balances, [5, 5, 7, 7]);
}
