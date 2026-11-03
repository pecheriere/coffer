//! Rates an admin entered (spec §8.10 N16), against what a fetch does and what the daily
//! table holds.

use coffer_domain::fx::{FxSource, HeldRate, RateLeg};
use coffer_domain::net_worth::ConversionState;
use coffer_store::Store;
use jiff::civil::Date;

use super::{at, cur, dump, fx, holding, observations, oct, open, rebuild, row_on, show_in, snap};

async fn enter(store: &Store, code: &str, on: Date, rate: &str) {
    let mut tx = store.begin().await.unwrap();
    tx.set_manual_rate(cur(code), on, rate.parse().unwrap(), at())
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

async fn remove(store: &Store, code: &str, on: Date) {
    let mut tx = store.begin().await.unwrap();
    tx.remove_manual_rate(cur(code), on).await.unwrap();
    tx.commit().await.unwrap();
}

async fn held(store: &Store, code: &str, on: Date) -> Option<HeldRate> {
    store
        .fx_rates(cur(code), on, on)
        .await
        .unwrap()
        .first()
        .copied()
}

#[tokio::test]
async fn neither_kind_of_fetch_overwrites_or_removes_a_manual_rate() {
    let (_dir, store) = open().await;
    enter(&store, "USD", oct(5), "9").await;
    enter(&store, "USD", oct(6), "8").await;

    // A correcting answer for the day, and one that doesn't hold the other day at all.
    let answer = observations(&[(oct(5), "1.1"), (oct(7), "1.2")]);
    let mut tx = store.begin().await.unwrap();
    tx.replace_fx_rates(cur("USD"), FxSource::Ecb, oct(1), &answer, at())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    fx(&store, "USD", &[(oct(5), "1.3")]).await;

    let five = held(&store, "USD", oct(5)).await.unwrap();
    assert_eq!((five.rate.to_string().as_str(), five.manual), ("9", true));
    let six = held(&store, "USD", oct(6)).await.unwrap();
    assert_eq!((six.rate.to_string().as_str(), six.manual), ("8", true));
    let seven = held(&store, "USD", oct(7)).await.unwrap();
    assert_eq!(
        (seven.rate.to_string().as_str(), seven.manual),
        ("1.2", false)
    );
}

#[tokio::test]
async fn coverage_of_fetches_knows_nothing_of_manual_rates() {
    let (_dir, store) = open().await;
    enter(&store, "XAF", oct(5), "655.957").await;

    let (fetched, manual) = store.fx_coverage_with_manual().await.unwrap();
    assert_eq!(fetched, []);
    assert_eq!(manual.len(), 1);
    assert_eq!(
        (manual[0].currency, manual[0].first_on, manual[0].last_on),
        (cur("XAF"), oct(5), oct(5))
    );
}

#[tokio::test]
async fn removing_one_brings_back_the_published_rate_under_it() {
    let (_dir, store) = open().await;
    fx(&store, "USD", &[(oct(5), "1.1")]).await;
    enter(&store, "USD", oct(5), "9").await;

    remove(&store, "USD", oct(5)).await;

    let back = held(&store, "USD", oct(5)).await.unwrap();
    assert_eq!(
        (back.rate.to_string().as_str(), back.manual),
        ("1.1", false)
    );
    assert_eq!(store.fx_coverage_with_manual().await.unwrap().1, []);
}

/// The test of N3: what incremental changes leave is what a rebuild makes.
#[tokio::test]
async fn the_table_moves_with_a_manual_rate_and_matches_a_rebuild() {
    let (_dir, store) = open().await;
    let held_in_xaf = holding(&store, "XAF").await;
    let in_euros = holding(&store, "EUR").await;
    snap(&store, held_in_xaf, oct(1), 655_957).await;
    snap(&store, in_euros, oct(1), 100).await;
    show_in(&store, "EUR").await;
    rebuild(&store, oct(31)).await;
    let before = row_on(&store, held_in_xaf, oct(10)).await;
    assert_eq!(before.conversion, ConversionState::Missing);

    enter(&store, "XAF", oct(5), "655.957").await;

    let on_the_day = row_on(&store, held_in_xaf, oct(5)).await;
    assert_eq!(on_the_day.display_minor, Some(100_000));
    assert_eq!(on_the_day.manual_rate_leg, Some(RateLeg::Held));
    assert_eq!(on_the_day.rate_on, Some(oct(5)));
    assert_eq!(
        row_on(&store, held_in_xaf, oct(31)).await.rate_on,
        Some(oct(5))
    );
    assert_eq!(
        row_on(&store, held_in_xaf, oct(4)).await.conversion,
        ConversionState::Missing,
        "never a later rate"
    );
    assert_eq!(
        row_on(&store, in_euros, oct(10)).await.manual_rate_leg,
        None
    );

    // A later published rate takes over from that day.
    fx(&store, "XAF", &[(oct(20), "700")]).await;
    let taken_over = row_on(&store, held_in_xaf, oct(25)).await;
    assert_eq!(
        (taken_over.rate_on, taken_over.manual_rate_leg),
        (Some(oct(20)), None)
    );
    assert_eq!(
        row_on(&store, held_in_xaf, oct(19)).await.manual_rate_leg,
        Some(RateLeg::Held)
    );
    let kept = dump(&store).await;
    rebuild(&store, oct(31)).await;
    assert_eq!(dump(&store).await, kept);

    remove(&store, "XAF", oct(5)).await;

    let after = row_on(&store, held_in_xaf, oct(10)).await;
    assert_eq!(
        (after.conversion, after.manual_rate_leg),
        (ConversionState::Missing, None)
    );
    let kept = dump(&store).await;
    rebuild(&store, oct(31)).await;
    assert_eq!(dump(&store).await, kept);
}

/// A write past the limit of one transaction leaves the rows to the rebuild.
#[tokio::test]
async fn a_rate_moving_too_many_rows_asks_for_a_rebuild() {
    let (_dir, store) = open().await;
    let one = holding(&store, "XAF").await;
    let two = holding(&store, "XAF").await;
    let long_ago = jiff::civil::date(1970, 1, 1);
    snap(&store, one, long_ago, 1).await;
    snap(&store, two, long_ago, 1).await;
    show_in(&store, "EUR").await;
    rebuild(&store, oct(31)).await;
    assert!(!store.net_worth_state().await.unwrap().rebuild_needed);

    enter(&store, "XAF", long_ago, "655.957").await;

    assert!(store.net_worth_state().await.unwrap().rebuild_needed);
    rebuild(&store, oct(31)).await;
    assert_eq!(
        row_on(&store, one, oct(31)).await.manual_rate_leg,
        Some(RateLeg::Held)
    );
}
