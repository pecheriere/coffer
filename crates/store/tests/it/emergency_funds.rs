//! The emergency fund, one target per currency: kept, replaced and removed, refused by the
//! schema when half made, and migration 0030 carrying the old single floor over.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use std::borrow::Cow;
use std::path::Path;

use coffer_domain::money::Currency;
use coffer_domain::plan::{EmergencyFundTarget, EmergencyFunds, ForecastSettings};
use coffer_store::Store;
use sqlx::SqlitePool;
use sqlx::migrate::Migrator;
use sqlx::sqlite::SqliteConnectOptions;
use tempfile::TempDir;

fn cur(code: &str) -> Currency {
    Currency::parse(code).unwrap()
}

fn fixed(minor_units: i64) -> EmergencyFundTarget {
    EmergencyFundTarget::Fixed { minor_units }
}

fn months(months: u8) -> EmergencyFundTarget {
    EmergencyFundTarget::MonthsOfOutflows { months }
}

async fn open() -> (TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("coffer.db")).await.unwrap();
    (dir, store)
}

#[tokio::test]
async fn put_get_and_delete() {
    let (_dir, store) = open().await;
    assert_eq!(
        store.emergency_funds().await.unwrap(),
        EmergencyFunds::new(),
        "none until one is set"
    );

    let mut tx = store.begin().await.unwrap();
    tx.put_emergency_fund(cur("USD"), &fixed(100_000))
        .await
        .unwrap();
    tx.put_emergency_fund(cur("EUR"), &months(3)).await.unwrap();
    assert_eq!(
        tx.emergency_funds().await.unwrap(),
        EmergencyFunds::from([(cur("EUR"), months(3)), (cur("USD"), fixed(100_000))]),
        "a transaction sees its own writes"
    );
    tx.commit().await.unwrap();
    assert_eq!(
        store.emergency_funds().await.unwrap(),
        EmergencyFunds::from([(cur("EUR"), months(3)), (cur("USD"), fixed(100_000))]),
        "both read back"
    );

    let mut tx = store.begin().await.unwrap();
    tx.put_emergency_fund(cur("USD"), &months(6)).await.unwrap();
    assert!(
        tx.delete_emergency_fund(cur("EUR")).await.unwrap(),
        "a row went"
    );
    assert!(
        !tx.delete_emergency_fund(cur("EUR")).await.unwrap(),
        "nothing left to remove"
    );
    tx.commit().await.unwrap();
    assert_eq!(
        store.emergency_funds().await.unwrap(),
        EmergencyFunds::from([(cur("USD"), months(6))]),
        "a second put replaces, a delete removes"
    );

    let mut tx = store.begin().await.unwrap();
    tx.put_emergency_fund(cur("GBP"), &fixed(0)).await.unwrap();
    drop(tx);
    assert_eq!(
        store.emergency_funds().await.unwrap(),
        EmergencyFunds::from([(cur("USD"), months(6))]),
        "a dropped transaction stores nothing"
    );
}

#[tokio::test]
async fn check_constraints_refuse_a_half_target() {
    let (dir, store) = open().await;
    let pool = SqlitePool::connect_with(
        SqliteConnectOptions::new().filename(dir.path().join("coffer.db")),
    )
    .await
    .unwrap();
    let insert = "INSERT INTO emergency_funds (currency, kind, minor, months) VALUES (?, ?, ?, ?)";
    let cases: [(&str, &str, Option<i64>, Option<i64>); 10] = [
        ("USD", "fixed", None, None),
        ("USD", "fixed", Some(1), Some(3)),
        ("USD", "months_of_outflows", None, None),
        ("USD", "months_of_outflows", Some(1), Some(3)),
        ("USD", "percent", None, Some(3)),
        ("USD", "fixed", Some(-1), None),
        ("USD", "fixed", Some(1_000_000_000_000_001), None),
        ("USD", "months_of_outflows", None, Some(0)),
        ("USD", "months_of_outflows", None, Some(61)),
        ("usd", "fixed", Some(1), None),
    ];
    for (currency, kind, minor, months) in cases {
        let result = sqlx::query(insert)
            .bind(currency)
            .bind(kind)
            .bind(minor)
            .bind(months)
            .execute(&pool)
            .await;
        assert!(
            result.is_err(),
            "{currency} {kind} {minor:?} {months:?} is refused"
        );
    }
    sqlx::query(insert)
        .bind("USD")
        .bind("fixed")
        .bind(Some(0_i64))
        .bind(None::<i64>)
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    assert_eq!(
        store.emergency_funds().await.unwrap(),
        EmergencyFunds::from([(cur("USD"), fixed(0))]),
        "a whole row reads back"
    );
}

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

const SETTINGS_INSERT: &str = "INSERT INTO household_settings (id, floor_kind, floor_minor, \
     floor_currency, floor_months, include_income, default_horizon_months, stale_after_days, \
     display_currency, timezone) VALUES (1, ?, ?, ?, ?, 1, 12, 30, ?, 'Europe/Paris')";

/// Accounts and planned items in several currencies. The forecast may use USD (an open
/// account counted in it), GBP (a closed card still in it, whose debt counts), EUR (an
/// active outflow) and NOK (active income). It never uses JPY (an account left out of it),
/// AUD (a closed account left out of it) or SEK (a paused outflow).
const IN_USE: &str = "
INSERT INTO accounts (id, name, kind, currency, liquidity, liquidity_overridden,
        include_in_net_worth, include_in_forecast, closed_at, created_at) VALUES
    ('usd', 'Checking', 'checking', 'USD', 'cash', 0, 1, 1, NULL, 1),
    ('gbp', 'Old card', 'credit_card', 'GBP', 'short_term_liability', 0, 1, 1, 5, 1),
    ('aud', 'Old', 'checking', 'AUD', 'cash', 0, 1, 0, 5, 1),
    ('jpy', 'Away', 'checking', 'JPY', 'cash', 0, 1, 0, NULL, 1);
INSERT INTO category_groups (id, name, position, created_at)
    VALUES ('group', 'Home', 0, 1);
INSERT INTO categories (id, group_id, name, kind, position, created_at)
    VALUES ('category', 'group', 'Rent', 'expense', 0, 1);
INSERT INTO planned_items (id, name, category_id, amount_minor, currency, direction, active,
        item_type, every, unit, anchor_on, spending, created_at, updated_at) VALUES
    ('eur', 'Rent', 'category', 100, 'EUR', 'outflow', 1, 'recurring', 1, 'months',
        '2026-01-01', 'bill', 1, 1),
    ('sek', 'Gym', 'category', 100, 'SEK', 'outflow', 0, 'recurring', 1, 'months',
        '2026-01-01', 'bill', 1, 1);
INSERT INTO planned_items (id, name, category_id, amount_minor, currency, direction, active,
        item_type, every, unit, anchor_on, created_at, updated_at) VALUES
    ('nok', 'Pay', 'category', 100, 'NOK', 'inflow', 1, 'recurring', 1, 'months',
        '2026-01-01', 1, 1);
";

/// The floor columns of the settings row, and the display currency.
struct OldFloor {
    kind: Option<&'static str>,
    minor: Option<i64>,
    currency: Option<&'static str>,
    months: Option<i64>,
    display_currency: Option<&'static str>,
}

/// A database at 0029 with `floor` seeded, then opened by the real `Migrator`.
async fn migrated_from(floor: &OldFloor) -> (TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("coffer.db");
    seed_before(&path, floor).await;
    let store = Store::open(&path).await.unwrap();
    (dir, store)
}

async fn seed_before(path: &Path, floor: &OldFloor) {
    let pool = SqlitePool::connect_with(
        SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true),
    )
    .await
    .unwrap();
    migrations_up_to(29).run(&pool).await.unwrap();
    sqlx::raw_sql(IN_USE).execute(&pool).await.unwrap();
    sqlx::query(SETTINGS_INSERT)
        .bind(floor.kind)
        .bind(floor.minor)
        .bind(floor.currency)
        .bind(floor.months)
        .bind(floor.display_currency)
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
}

/// The settings other than the floor are as seeded.
async fn assert_settings_kept(store: &Store, display_currency: Option<&str>) {
    let settings = store.household_settings().await.unwrap();
    assert_eq!(
        settings.forecast,
        ForecastSettings {
            include_income: true,
            default_horizon_months: 12,
            stale_after_days: 30,
        },
        "the forecast settings are kept"
    );
    assert_eq!(
        settings.display_currency,
        display_currency.map(cur),
        "the display currency is kept"
    );
    assert_eq!(
        settings.timezone.map(|zone| zone.name().to_owned()),
        Some("Europe/Paris".to_owned()),
        "the time zone is kept"
    );
}

#[tokio::test]
async fn migration_carries_the_floor_over() {
    let (_dir, store) = migrated_from(&OldFloor {
        kind: Some("fixed"),
        minor: Some(100_000),
        currency: Some("EUR"),
        months: None,
        display_currency: None,
    })
    .await;
    assert_eq!(
        store.emergency_funds().await.unwrap(),
        EmergencyFunds::from([(cur("EUR"), fixed(100_000))]),
        "a fixed floor is exactly its currency's target"
    );
    assert_settings_kept(&store, None).await;

    let (_dir, store) = migrated_from(&OldFloor {
        kind: Some("months_of_outflows"),
        minor: None,
        currency: None,
        months: Some(3),
        display_currency: Some("CHF"),
    })
    .await;
    assert_eq!(
        store.emergency_funds().await.unwrap(),
        EmergencyFunds::from([
            (cur("CHF"), months(3)),
            (cur("EUR"), months(3)),
            (cur("GBP"), months(3)),
            (cur("NOK"), months(3)),
            (cur("USD"), months(3)),
        ]),
        "a months floor is a months target for every currency the forecast may use, \
         income and a closed card still in it included"
    );
    assert_settings_kept(&store, Some("CHF")).await;

    let (_dir, store) = migrated_from(&OldFloor {
        kind: None,
        minor: None,
        currency: None,
        months: None,
        display_currency: Some("USD"),
    })
    .await;
    assert_eq!(
        store.emergency_funds().await.unwrap(),
        EmergencyFunds::new(),
        "no floor writes no target"
    );
    assert_settings_kept(&store, Some("USD")).await;
}

#[tokio::test]
async fn migration_without_settings_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("coffer.db");
    let pool = SqlitePool::connect_with(
        SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .foreign_keys(true),
    )
    .await
    .unwrap();
    migrations_up_to(29).run(&pool).await.unwrap();
    sqlx::raw_sql(IN_USE).execute(&pool).await.unwrap();
    pool.close().await;
    let store = Store::open(&path).await.unwrap();
    assert_eq!(
        store.emergency_funds().await.unwrap(),
        EmergencyFunds::new(),
        "no settings row, no target"
    );
    assert_eq!(
        store.forecast_settings().await.unwrap(),
        ForecastSettings::default(),
        "and the defaults still apply"
    );
}

#[tokio::test]
async fn the_forecast_reads_them() {
    let (_dir, store) = open().await;
    let mut tx = store.begin().await.unwrap();
    tx.put_emergency_fund(cur("EUR"), &months(2)).await.unwrap();
    tx.commit().await.unwrap();
    let data = store
        .forecast_data(jiff::civil::date(2026, 11, 15))
        .await
        .unwrap();
    assert_eq!(
        data.emergency_funds,
        EmergencyFunds::from([(cur("EUR"), months(2))]),
        "in the forecast's one read"
    );
}
