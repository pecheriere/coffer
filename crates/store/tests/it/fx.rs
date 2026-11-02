//! Exchange rates and the display currency in the store: rates keep their
//! exact value, a re-fetch of an unchanged rate writes nothing, coverage
//! remembers what is known through a failure, and the display currency lives
//! beside the forecast settings without either touching the other.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use coffer_domain::accounts::{AccountKind, AccountName, LiquidityClass};
use coffer_domain::fx::{
    FxCoverage, FxErrorCode, FxFetchStatus, FxRate, FxSource, RateObservation,
};
use coffer_domain::ids::AccountId;
use coffer_domain::money::Currency;
use coffer_domain::plan::{ForecastSettings, SafetyFloor};
use coffer_store::{AccountChanges, NewAccount, Store};
use jiff::Timestamp;
use jiff::civil::{Date, date};
use tempfile::TempDir;

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

async fn open() -> (TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("coffer.db")).await.unwrap();
    (dir, store)
}

/// Upserts `rates` in one committed transaction and returns what it answered.
async fn upsert(
    store: &Store,
    code: &str,
    rates: &[RateObservation],
    now: Timestamp,
) -> Option<Date> {
    let mut tx = store.begin().await.unwrap();
    let changed = tx
        .upsert_fx_rates(cur(code), FxSource::Ecb, rates, now)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    changed
}

async fn coverage_of(store: &Store, code: &str) -> FxCoverage {
    store
        .fx_coverage()
        .await
        .unwrap()
        .into_iter()
        .find(|row| row.currency == cur(code))
        .unwrap()
}

#[tokio::test]
async fn rates_roundtrip_exactly() {
    let (_dir, store) = open().await;
    upsert(&store, "USD", &[obs(date(2026, 10, 30), "1.1172")], at(0)).await;
    upsert(&store, "JPY", &[obs(date(2026, 10, 30), "136")], at(0)).await;
    upsert(&store, "VND", &[obs(date(2026, 10, 30), "20275.17")], at(0)).await;
    for (code, text) in [("USD", "1.1172"), ("JPY", "136"), ("VND", "20275.17")] {
        let rates = store
            .fx_rates(cur(code), date(2026, 10, 1), date(2026, 10, 31))
            .await
            .unwrap();
        assert_eq!(rates, vec![obs(date(2026, 10, 30), text)]);
    }
}

#[tokio::test]
async fn rates_come_newest_first_within_the_range() {
    let (_dir, store) = open().await;
    upsert(
        &store,
        "USD",
        &[
            obs(date(2026, 10, 28), "1.10"),
            obs(date(2026, 10, 30), "1.12"),
            obs(date(2026, 10, 29), "1.11"),
            obs(date(2026, 11, 2), "1.13"),
        ],
        at(0),
    )
    .await;
    let rates = store
        .fx_rates(cur("USD"), date(2026, 10, 28), date(2026, 10, 30))
        .await
        .unwrap();
    let days: Vec<_> = rates.iter().map(|rate| rate.on).collect();
    assert_eq!(
        days,
        vec![date(2026, 10, 30), date(2026, 10, 29), date(2026, 10, 28)]
    );
}

#[tokio::test]
async fn a_call_returns_at_most_a_thousand_rates() {
    let (_dir, store) = open().await;
    let first = date(2020, 1, 1);
    let rates: Vec<_> = (0..1_005)
        .map(|day| {
            obs(
                first.checked_add(jiff::Span::new().days(day)).unwrap(),
                "1.1",
            )
        })
        .collect();
    upsert(&store, "USD", &rates, at(0)).await;
    let read = store
        .fx_rates(cur("USD"), first, date(2030, 1, 1))
        .await
        .unwrap();
    assert_eq!(read.len(), 1_000);
    assert_eq!(read[0].on, rates[1_004].on);
}

#[tokio::test]
async fn a_correction_returns_its_date() {
    let (_dir, store) = open().await;
    let day = date(2026, 10, 30);
    upsert(&store, "USD", &[obs(day, "1.1172")], at(0)).await;
    let changed = upsert(&store, "USD", &[obs(day, "1.1180")], at(3_600)).await;
    assert_eq!(changed, Some(day));
    let rates = store.fx_rates(cur("USD"), day, day).await.unwrap();
    assert_eq!(rates, vec![obs(day, "1.118")]);
}

#[tokio::test]
async fn earliest_changed_date_is_returned() {
    let (_dir, store) = open().await;
    let (a, b, c) = (date(2026, 10, 28), date(2026, 10, 29), date(2026, 10, 30));
    upsert(
        &store,
        "USD",
        &[obs(a, "1.10"), obs(b, "1.11"), obs(c, "1.12")],
        at(0),
    )
    .await;
    // The first is unchanged (written differently), the second is corrected,
    // the third is corrected too, and a fourth is new and later.
    let d = date(2026, 11, 2);
    let changed = upsert(
        &store,
        "USD",
        &[
            obs(d, "1.14"),
            obs(c, "1.121"),
            obs(b, "1.115"),
            obs(a, "1.100"),
        ],
        at(60),
    )
    .await;
    assert_eq!(changed, Some(b));
}

#[tokio::test]
async fn earliest_changed_date_is_returned_whatever_the_input_order() {
    let (_dir, store) = open().await;
    let (a, b, c) = (date(2026, 10, 28), date(2026, 10, 29), date(2026, 10, 30));
    upsert(
        &store,
        "USD",
        &[obs(a, "1.10"), obs(b, "1.11"), obs(c, "1.12")],
        at(0),
    )
    .await;
    let changed = upsert(
        &store,
        "USD",
        &[obs(a, "1.10"), obs(b, "1.115"), obs(c, "1.125")],
        at(60),
    )
    .await;
    assert_eq!(changed, Some(b));
}

#[tokio::test]
async fn the_earliest_change_may_be_in_a_later_chunk() {
    let (_dir, store) = open().await;
    let first = date(2024, 1, 1);
    let day = |n: i64| first.checked_add(jiff::Span::new().days(n)).unwrap();
    let stored: Vec<_> = (0..600).map(|n| obs(day(n), "1.1")).collect();
    upsert(&store, "USD", &stored, at(0)).await;
    // Newest first: the corrections of the oldest days come last, after more
    // than one chunk of later ones.
    let corrected: Vec<_> = (0..600).rev().map(|n| obs(day(n), "1.2")).collect();
    let changed = upsert(&store, "USD", &corrected, at(60)).await;
    assert_eq!(changed, Some(day(0)));
}

#[tokio::test]
async fn a_later_chunk_does_not_forget_an_earlier_change() {
    let (_dir, store) = open().await;
    let first = date(2024, 1, 1);
    let day = |n: i64| first.checked_add(jiff::Span::new().days(n)).unwrap();
    let stored: Vec<_> = (0..600).map(|n| obs(day(n), "1.1")).collect();
    upsert(&store, "USD", &stored, at(0)).await;
    // Oldest first: the earliest correction is in the first chunk.
    let corrected: Vec<_> = (0..600).map(|n| obs(day(n), "1.2")).collect();
    let changed = upsert(&store, "USD", &corrected, at(60)).await;
    assert_eq!(changed, Some(day(0)));
}

#[tokio::test]
async fn coverage_tracks_first_and_last() {
    let (_dir, store) = open().await;
    assert_eq!(store.fx_coverage().await.unwrap(), Vec::new());
    upsert(
        &store,
        "USD",
        &[
            obs(date(2026, 10, 29), "1.11"),
            obs(date(2026, 10, 28), "1.10"),
        ],
        at(0),
    )
    .await;
    upsert(&store, "USD", &[obs(date(2026, 10, 30), "1.12")], at(60)).await;
    let coverage = coverage_of(&store, "USD").await;
    assert_eq!(
        coverage,
        FxCoverage {
            currency: cur("USD"),
            status: FxFetchStatus::Ready,
            first_on: Some(date(2026, 10, 28)),
            last_on: Some(date(2026, 10, 30)),
            last_attempt_at: at(60),
            last_success_at: Some(at(60)),
            last_error: None,
        }
    );
}

#[tokio::test]
async fn an_empty_answer_with_rates_stored_stays_ready() {
    let (_dir, store) = open().await;
    upsert(&store, "USD", &[obs(date(2026, 10, 30), "1.12")], at(0)).await;
    assert_eq!(upsert(&store, "USD", &[], at(600)).await, None);
    let coverage = coverage_of(&store, "USD").await;
    assert_eq!(coverage.status, FxFetchStatus::Ready);
    assert_eq!(coverage.last_on, Some(date(2026, 10, 30)));
    assert_eq!(coverage.last_attempt_at, at(600));
    assert_eq!(coverage.last_success_at, Some(at(600)));
}

#[tokio::test]
async fn an_empty_answer_with_nothing_stored_is_not_published() {
    let (_dir, store) = open().await;
    assert_eq!(upsert(&store, "THB", &[], at(0)).await, None);
    let coverage = coverage_of(&store, "THB").await;
    assert_eq!(coverage.status, FxFetchStatus::NotPublished);
    assert_eq!(coverage.first_on, None);
    assert_eq!(coverage.last_on, None);
    assert_eq!(coverage.last_success_at, None);
}

#[tokio::test]
async fn failure_keeps_what_is_known() {
    let (_dir, store) = open().await;
    upsert(
        &store,
        "USD",
        &[
            obs(date(2026, 10, 28), "1.10"),
            obs(date(2026, 10, 30), "1.12"),
        ],
        at(0),
    )
    .await;
    let mut tx = store.begin().await.unwrap();
    tx.record_fx_failure(
        cur("USD"),
        FxFetchStatus::Failing,
        Some(FxErrorCode::Network),
        at(600),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        coverage_of(&store, "USD").await,
        FxCoverage {
            currency: cur("USD"),
            status: FxFetchStatus::Failing,
            first_on: Some(date(2026, 10, 28)),
            last_on: Some(date(2026, 10, 30)),
            last_attempt_at: at(600),
            last_success_at: Some(at(0)),
            last_error: Some(FxErrorCode::Network),
        }
    );
    // The next success clears the error.
    upsert(&store, "USD", &[obs(date(2026, 10, 30), "1.12")], at(1_200)).await;
    let healed = coverage_of(&store, "USD").await;
    assert_eq!(healed.status, FxFetchStatus::Ready);
    assert_eq!(healed.last_error, None);
}

#[tokio::test]
async fn a_failure_with_nothing_known_records_no_dates() {
    let (_dir, store) = open().await;
    let mut tx = store.begin().await.unwrap();
    tx.record_fx_failure(cur("THB"), FxFetchStatus::NotPublished, None, at(0))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        coverage_of(&store, "THB").await,
        FxCoverage {
            currency: cur("THB"),
            status: FxFetchStatus::NotPublished,
            first_on: None,
            last_on: None,
            last_attempt_at: at(0),
            last_success_at: None,
            last_error: None,
        }
    );
}

#[tokio::test]
async fn no_rate_for_the_euro() {
    let (_dir, store) = open().await;
    let mut tx = store.begin().await.unwrap();
    let refused = tx
        .upsert_fx_rates(
            cur("EUR"),
            FxSource::Ecb,
            &[obs(date(2026, 10, 30), "1")],
            at(0),
        )
        .await;
    assert!(refused.is_err());
}

#[tokio::test]
async fn corrupt_rate_is_a_backend_error() {
    let (_dir, store) = open().await;
    store
        .execute_unchecked_for_tests(
            "INSERT INTO fx_rates (quote, on_date, rate, source, fetched_at) \
             VALUES ('USD', '2026-10-30', ?, 'ecb', 0)",
            "1e5",
        )
        .await
        .unwrap();
    let err = store
        .fx_rates(cur("USD"), date(2026, 10, 1), date(2026, 10, 31))
        .await
        .unwrap_err();
    assert!(matches!(err, coffer_store::StoreError::Backend(_)));
}

async fn account_in(store: &Store, code: &str) -> AccountId {
    let id = AccountId::new();
    let mut tx = store.begin().await.unwrap();
    tx.insert_account(NewAccount {
        id,
        name: AccountName::parse(&format!("In {code}")).unwrap(),
        kind: AccountKind::Checking,
        currency: cur(code),
        liquidity: LiquidityClass::Cash,
        liquidity_overridden: false,
        include_in_net_worth: true,
        include_in_forecast: true,
        created_at: at(0),
    })
    .await
    .unwrap();
    tx.commit().await.unwrap();
    id
}

#[tokio::test]
async fn currencies_in_use_includes_closed_accounts() {
    let (_dir, store) = open().await;
    assert_eq!(store.currencies_in_use().await.unwrap(), Vec::new());
    account_in(&store, "USD").await;
    account_in(&store, "USD").await;
    let thb = account_in(&store, "THB").await;
    let mut tx = store.begin().await.unwrap();
    tx.update_account(
        thb,
        AccountChanges {
            closed_at: Some(Some(at(10))),
            ..AccountChanges::default()
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        store.currencies_in_use().await.unwrap(),
        vec![cur("THB"), cur("USD")]
    );
}

#[tokio::test]
async fn display_currency_starts_unset() {
    let (_dir, store) = open().await;
    assert_eq!(store.display_currency().await.unwrap(), None);
    let mut tx = store.begin().await.unwrap();
    assert_eq!(tx.display_currency().await.unwrap(), None);
}

#[tokio::test]
async fn setting_it_answers_the_previous_value() {
    let (_dir, store) = open().await;
    let mut tx = store.begin().await.unwrap();
    assert_eq!(tx.set_display_currency(cur("USD")).await.unwrap(), None);
    assert_eq!(
        tx.set_display_currency(cur("GBP")).await.unwrap(),
        Some(cur("USD"))
    );
    assert_eq!(
        tx.set_display_currency(cur("GBP")).await.unwrap(),
        Some(cur("GBP"))
    );
}

#[tokio::test]
async fn setting_it_with_no_settings_row_keeps_the_defaults() {
    let (_dir, store) = open().await;
    let mut tx = store.begin().await.unwrap();
    tx.set_display_currency(cur("EUR")).await.unwrap();
    assert_eq!(tx.display_currency().await.unwrap(), Some(cur("EUR")));
    tx.commit().await.unwrap();
    assert_eq!(store.display_currency().await.unwrap(), Some(cur("EUR")));
    assert_eq!(
        store.forecast_settings().await.unwrap(),
        ForecastSettings::default()
    );
}

fn custom_settings() -> ForecastSettings {
    ForecastSettings {
        floor: Some(SafetyFloor::MonthsOfOutflows { months: 3 }),
        include_income: false,
        default_horizon_months: 12,
        stale_after_days: 30,
    }
}

#[tokio::test]
async fn setting_it_leaves_an_existing_row_alone() {
    let (_dir, store) = open().await;
    let mut tx = store.begin().await.unwrap();
    tx.set_forecast_settings(&custom_settings()).await.unwrap();
    tx.set_display_currency(cur("USD")).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(store.forecast_settings().await.unwrap(), custom_settings());
    let mut tx = store.begin().await.unwrap();
    tx.set_display_currency(cur("GBP")).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(store.display_currency().await.unwrap(), Some(cur("GBP")));
    assert_eq!(store.forecast_settings().await.unwrap(), custom_settings());
}

#[tokio::test]
async fn forecast_settings_leave_the_display_currency() {
    let (_dir, store) = open().await;
    let mut tx = store.begin().await.unwrap();
    tx.set_display_currency(cur("USD")).await.unwrap();
    tx.set_forecast_settings(&custom_settings()).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(store.display_currency().await.unwrap(), Some(cur("USD")));
    assert_eq!(store.forecast_settings().await.unwrap(), custom_settings());
}

// An answer about a window replaces what it speaks for.

async fn replace(
    store: &Store,
    code: &str,
    from: Date,
    rates: &[RateObservation],
    now: Timestamp,
) -> Option<Date> {
    let mut tx = store.begin().await.unwrap();
    let changed = tx
        .replace_fx_rates(cur(code), FxSource::Ecb, from, rates, now)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    changed
}

/// The days holding a rate, oldest first.
async fn days_of(store: &Store, code: &str) -> Vec<Date> {
    let mut days: Vec<_> = store
        .fx_rates(cur(code), date(1990, 1, 1), date(2100, 1, 1))
        .await
        .unwrap()
        .iter()
        .map(|one| one.on)
        .collect();
    days.reverse();
    days
}

#[tokio::test]
async fn a_replacement_removes_what_the_answer_does_not_hold() {
    let (_dir, store) = open().await;
    let (fri, sat, mon) = (date(2026, 10, 30), date(2026, 10, 31), date(2026, 11, 2));
    upsert(
        &store,
        "USD",
        &[obs(fri, "1.1"), obs(sat, "9"), obs(mon, "1.2")],
        at(0),
    )
    .await;

    let changed = replace(
        &store,
        "USD",
        fri,
        &[obs(fri, "1.1"), obs(mon, "1.2")],
        at(1),
    )
    .await;

    assert_eq!(changed, Some(sat));
    assert_eq!(days_of(&store, "USD").await, [fri, mon]);
}

#[tokio::test]
async fn a_replacement_keeps_what_is_before_the_window() {
    let (_dir, store) = open().await;
    let (old, from, mon) = (date(2026, 9, 1), date(2026, 10, 26), date(2026, 11, 2));
    upsert(&store, "USD", &[obs(old, "1.1"), obs(mon, "1.2")], at(0)).await;

    let changed = replace(&store, "USD", from, &[obs(mon, "1.2")], at(1)).await;

    assert_eq!(changed, None);
    assert_eq!(days_of(&store, "USD").await, [old, mon]);
}

#[tokio::test]
async fn a_replacement_keeps_what_is_after_its_last_row() {
    let (_dir, store) = open().await;
    let (fri, mon) = (date(2026, 10, 30), date(2026, 11, 2));
    upsert(&store, "USD", &[obs(fri, "1.1"), obs(mon, "1.2")], at(0)).await;

    let changed = replace(&store, "USD", fri, &[obs(fri, "1.1")], at(1)).await;

    assert_eq!(changed, None);
    assert_eq!(days_of(&store, "USD").await, [fri, mon]);
}

#[tokio::test]
async fn an_empty_replacement_removes_nothing() {
    let (_dir, store) = open().await;
    let (fri, mon) = (date(2026, 10, 30), date(2026, 11, 2));
    upsert(&store, "USD", &[obs(fri, "1.1"), obs(mon, "1.2")], at(0)).await;

    let changed = replace(&store, "USD", fri, &[], at(1)).await;

    assert_eq!(changed, None);
    assert_eq!(days_of(&store, "USD").await, [fri, mon]);
    let coverage = coverage_of(&store, "USD").await;
    assert_eq!(coverage.status, FxFetchStatus::Ready);
    assert_eq!(coverage.last_attempt_at, at(1));
}

#[tokio::test]
async fn an_empty_replacement_with_nothing_stored_is_not_published() {
    let (_dir, store) = open().await;

    replace(&store, "USD", date(2026, 10, 30), &[], at(0)).await;

    assert_eq!(
        coverage_of(&store, "USD").await.status,
        FxFetchStatus::NotPublished
    );
}

#[tokio::test]
async fn a_replacement_writes_corrections_and_answers_the_earliest_change() {
    let (_dir, store) = open().await;
    let (thu, fri, sat, mon) = (
        date(2026, 10, 29),
        date(2026, 10, 30),
        date(2026, 10, 31),
        date(2026, 11, 2),
    );
    upsert(
        &store,
        "USD",
        &[obs(thu, "1.1"), obs(sat, "9"), obs(mon, "1.2")],
        at(0),
    )
    .await;

    let correction = replace(
        &store,
        "USD",
        thu,
        &[obs(thu, "1.15"), obs(sat, "9"), obs(mon, "1.2")],
        at(1),
    )
    .await;
    assert_eq!(correction, Some(thu));

    let removal = replace(
        &store,
        "USD",
        fri,
        &[obs(fri, "1.1"), obs(mon, "1.2")],
        at(2),
    )
    .await;
    assert_eq!(removal, Some(fri));

    let earlier = replace(
        &store,
        "USD",
        thu,
        &[obs(thu, "1.15"), obs(mon, "1.2")],
        at(3),
    )
    .await;
    assert_eq!(earlier, Some(fri), "the removal is the earliest change");
}

#[tokio::test]
async fn a_replacement_keeps_the_coverage_true() {
    let (_dir, store) = open().await;
    let (first, second, third) = (date(2026, 10, 28), date(2026, 10, 29), date(2026, 10, 30));
    upsert(
        &store,
        "USD",
        &[obs(first, "9"), obs(second, "1.1"), obs(third, "1.2")],
        at(0),
    )
    .await;

    replace(
        &store,
        "USD",
        first,
        &[obs(second, "1.1"), obs(third, "1.2")],
        at(1),
    )
    .await;

    let coverage = coverage_of(&store, "USD").await;
    assert_eq!(coverage.first_on, Some(second));
    assert_eq!(coverage.last_on, Some(third));
    assert_eq!(coverage.last_success_at, Some(at(1)));
}

#[tokio::test]
async fn a_replacement_leaves_other_currencies_alone() {
    let (_dir, store) = open().await;
    let (fri, sat, mon) = (date(2026, 10, 30), date(2026, 10, 31), date(2026, 11, 2));
    upsert(&store, "GBP", &[obs(sat, "0.9")], at(0)).await;
    upsert(
        &store,
        "USD",
        &[obs(fri, "1.1"), obs(sat, "9"), obs(mon, "1.2")],
        at(0),
    )
    .await;

    replace(
        &store,
        "USD",
        fri,
        &[obs(fri, "1.1"), obs(mon, "1.2")],
        at(1),
    )
    .await;

    assert_eq!(days_of(&store, "GBP").await, [sat]);
}

#[tokio::test]
async fn a_replacement_rolled_back_changes_nothing() {
    let (_dir, store) = open().await;
    let (fri, sat, mon) = (date(2026, 10, 30), date(2026, 10, 31), date(2026, 11, 2));
    upsert(
        &store,
        "USD",
        &[obs(fri, "1.1"), obs(sat, "9"), obs(mon, "1.2")],
        at(0),
    )
    .await;

    let mut tx = store.begin().await.unwrap();
    tx.replace_fx_rates(
        cur("USD"),
        FxSource::Ecb,
        fri,
        &[obs(fri, "1.1"), obs(mon, "1.2")],
        at(1),
    )
    .await
    .unwrap();
    drop(tx);

    assert_eq!(days_of(&store, "USD").await, [fri, sat, mon]);
    assert_eq!(coverage_of(&store, "USD").await.last_attempt_at, at(0));
}
