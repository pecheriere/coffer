//! What only the crate can see: the stored `fetched_at`, and the loader that
//! the net worth tables will use.

use coffer_domain::fx::{FxRate, FxSource, RateLookup, RateObservation};
use coffer_domain::money::Currency;
use jiff::Timestamp;
use jiff::civil::{Date, date};

use super::load_rate_book;
use crate::Store;

fn at(seconds: i64) -> Timestamp {
    Timestamp::from_second(1_800_000_000 + seconds).unwrap()
}

fn cur(code: &str) -> Currency {
    Currency::parse(code).unwrap()
}

fn obs(on: Date, rate: &str) -> RateObservation {
    RateObservation {
        on,
        rate: rate.parse::<FxRate>().unwrap(),
    }
}

async fn open() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("coffer.db")).await.unwrap();
    (dir, store)
}

async fn put(store: &Store, code: &str, rates: &[RateObservation], now: Timestamp) -> Option<Date> {
    let mut tx = store.begin().await.unwrap();
    let changed = tx
        .upsert_fx_rates(cur(code), FxSource::Ecb, rates, now)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    changed
}

#[tokio::test]
async fn an_unchanged_rate_changes_nothing() {
    let (_dir, store) = open().await;
    let day = date(2026, 10, 30);
    assert_eq!(
        put(&store, "USD", &[obs(day, "1.115")], at(0)).await,
        Some(day)
    );
    assert_eq!(
        put(&store, "USD", &[obs(day, "1.1150")], at(3_600)).await,
        None
    );
    let fetched: i64 = sqlx::query_scalar("SELECT fetched_at FROM fx_rates WHERE quote = 'USD'")
        .fetch_one(&store.pool)
        .await
        .unwrap();
    assert_eq!(fetched, at(0).as_millisecond());
}

#[tokio::test]
async fn rate_book_holds_the_rate_before_the_window() {
    let (_dir, store) = open().await;
    let (thursday, friday, monday) = (date(2026, 10, 29), date(2026, 10, 30), date(2026, 11, 2));
    let sunday = date(2026, 11, 1);
    put(
        &store,
        "USD",
        &[
            obs(thursday, "1.10"),
            obs(friday, "1.11"),
            obs(monday, "1.12"),
        ],
        at(0),
    )
    .await;
    let mut conn = store.pool.acquire().await.unwrap();
    let book = load_rate_book(
        &mut conn,
        &[cur("USD"), cur("EUR"), cur("THB")],
        sunday,
        sunday,
    )
    .await
    .unwrap();
    let usd = cur("USD");
    assert_eq!(
        book.rate(usd, sunday),
        RateLookup::Found(friday, "1.11".parse().unwrap())
    );
    // Thursday was not loaded, and Monday is outside the window.
    assert_eq!(book.rate(usd, thursday), RateLookup::Missing);
    assert_eq!(
        book.rate(usd, monday),
        RateLookup::Found(friday, "1.11".parse().unwrap())
    );
    assert_eq!(book.rate(cur("THB"), sunday), RateLookup::Missing);
}

#[tokio::test]
async fn rate_book_holds_every_rate_inside_the_window_and_one_before() {
    let (_dir, store) = open().await;
    let days = [
        date(2026, 10, 27),
        date(2026, 10, 28),
        date(2026, 10, 29),
        date(2026, 10, 30),
    ];
    let rates: Vec<_> = days.iter().map(|&day| obs(day, "1.10")).collect();
    put(&store, "USD", &rates, at(0)).await;
    let mut conn = store.pool.acquire().await.unwrap();
    let book = load_rate_book(&mut conn, &[cur("USD")], days[2], days[3])
        .await
        .unwrap();
    let found = |day| RateLookup::Found(day, "1.1".parse().unwrap());
    assert_eq!(book.rate(cur("USD"), days[3]), found(days[3]));
    assert_eq!(book.rate(cur("USD"), days[2]), found(days[2]));
    assert_eq!(book.rate(cur("USD"), days[1]), found(days[1]));
    assert_eq!(book.rate(cur("USD"), days[0]), RateLookup::Missing);
}

#[tokio::test]
async fn the_rates_table_refuses_the_euro() {
    let (_dir, store) = open().await;
    let refused = sqlx::query(
        "INSERT INTO fx_rates (quote, on_date, rate, source, fetched_at) \
         VALUES ('EUR', '2026-10-30', '1', 'ecb', 0)",
    )
    .execute(&store.pool)
    .await;
    assert!(refused.is_err());
}

#[tokio::test]
async fn the_coverage_table_refuses_the_euro() {
    let (_dir, store) = open().await;
    let refused = sqlx::query(
        "INSERT INTO fx_coverage (currency, status, last_attempt_at) VALUES ('EUR', 'ready', 0)",
    )
    .execute(&store.pool)
    .await;
    assert!(refused.is_err());
}
