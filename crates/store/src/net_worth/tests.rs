//! What only the crate can see: the schema's own refusals.

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
