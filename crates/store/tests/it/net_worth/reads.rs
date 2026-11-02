//! A read answers from one moment, stops at the last day filled, and holds
//! nothing of an account outside its scope.

use coffer_domain::fx::RateLeg;
use coffer_domain::ids::AccountId;
use coffer_domain::money::Currency;
use coffer_domain::net_worth::{
    CurrencyState, NetWorthCaution, NetWorthInput, NetWorthInterval, NetWorthReport,
    net_worth_report,
};
use coffer_domain::plan::ForecastSettings;
use coffer_domain::policy::AccountScope;
use coffer_store::{AccountChanges, NetWorthData, Store, StoreError};
use jiff::civil::{Date, date};

use super::{cur, days, fx, holding, holding_with, oct, open, rebuild, show_in, snap};

async fn by_day(store: &Store, scope: &AccountScope, from: Date, to: Date) -> NetWorthData {
    store
        .net_worth_data(scope, from, to, NetWorthInterval::Day)
        .await
        .unwrap()
}

/// Who is listed, in order.
fn listed(data: &NetWorthData) -> Vec<AccountId> {
    data.accounts.iter().map(|one| one.id).collect()
}

/// Who holds a row, once each, in order.
fn holders(data: &NetWorthData) -> Vec<AccountId> {
    let mut seen: Vec<_> = data.rows.iter().map(|row| row.account).collect();
    seen.dedup();
    seen
}

fn on_days(data: &NetWorthData) -> Vec<Date> {
    let mut seen: Vec<_> = data.rows.iter().map(|row| row.on).collect();
    seen.dedup();
    seen
}

/// The report of `data`, as the service will ask for it.
fn report_of(data: NetWorthData, from: Date, to: Date) -> NetWorthReport {
    let accounts = data
        .accounts
        .iter()
        .map(|record| coffer_domain::net_worth::ReportAccount {
            id: record.id,
            name: record.name.to_string(),
            currency: record.currency,
            liquidity: record.liquidity,
            include_in_net_worth: record.include_in_net_worth,
            closed_on: None,
        })
        .collect();
    net_worth_report(&NetWorthInput {
        today: to,
        from,
        to,
        interval: NetWorthInterval::Day,
        through: data.state.filled_through,
        rebuild_needed: data.state.rebuild_needed,
        display: data.display,
        stale_after_days: data.stale_after_days,
        account_points: false,
        accounts,
        rows: data.rows,
        first_on: data.first_on,
        coverage: data
            .coverage
            .iter()
            .map(|one| CurrencyState {
                currency: one.currency,
                reason: None,
            })
            .collect(),
    })
    .unwrap()
}

#[tokio::test]
async fn reads_stop_at_through() {
    let (_dir, store) = open().await;
    let one = holding(&store, "USD").await;
    snap(&store, one, oct(1), 100).await;
    snap(&store, one, oct(9), 900).await;
    rebuild(&store, oct(5)).await;

    let data = by_day(&store, &AccountScope::All, oct(3), oct(20)).await;

    assert_eq!(on_days(&data), days(3, 5));
    assert_eq!(data.state.filled_through, Some(oct(5)));
}

#[tokio::test]
async fn a_range_after_through_holds_no_row() {
    let (_dir, store) = open().await;
    let one = holding(&store, "USD").await;
    snap(&store, one, oct(1), 100).await;
    rebuild(&store, oct(5)).await;

    let data = by_day(&store, &AccountScope::All, oct(10), oct(20)).await;

    assert_eq!(data.rows, []);
    assert_eq!(listed(&data), [one]);
}

/// Each period's last day only, the last of them where the table ends.
#[tokio::test]
async fn rows_are_those_of_the_period_ends() {
    let (_dir, store) = open().await;
    let one = holding(&store, "USD").await;
    snap(&store, one, date(2026, 8, 20), 100).await;
    rebuild(&store, oct(5)).await;

    let data = store
        .net_worth_data(
            &AccountScope::All,
            date(2026, 8, 1),
            date(2026, 12, 31),
            NetWorthInterval::Month,
        )
        .await
        .unwrap();

    assert_eq!(
        on_days(&data),
        [date(2026, 8, 31), date(2026, 9, 30), oct(5)]
    );
}

#[tokio::test]
async fn while_a_rebuild_is_needed_there_are_no_rows() {
    let (_dir, store) = open().await;
    let one = holding(&store, "USD").await;
    snap(&store, one, oct(1), 100).await;
    rebuild(&store, oct(5)).await;
    store
        .execute_unchecked_for_tests(
            "UPDATE net_worth_state SET rebuild_needed = 1 WHERE filled_through = ?",
            &oct(5).to_string(),
        )
        .await
        .unwrap();

    let data = by_day(&store, &AccountScope::All, oct(1), oct(5)).await;

    assert!(data.state.rebuild_needed);
    assert_eq!(data.rows, []);
}

#[tokio::test]
async fn scope_filters_in_the_query() {
    let (_dir, store) = open().await;
    let seen = holding(&store, "USD").await;
    let hidden = holding(&store, "JPY").await;
    snap(&store, seen, oct(2), 100).await;
    snap(&store, hidden, oct(1), 100).await;
    fx(&store, "USD", &[(oct(1), "1.1")]).await;
    fx(&store, "JPY", &[(oct(1), "170")]).await;
    fx(&store, "GBP", &[(oct(1), "0.8")]).await;
    show_in(&store, "GBP").await;
    rebuild(&store, oct(3)).await;

    let data = by_day(&store, &AccountScope::Only(vec![seen]), oct(1), oct(3)).await;

    assert_eq!(listed(&data), [seen]);
    assert_eq!(holders(&data), [seen]);
    assert_eq!(on_days(&data), [oct(2), oct(3)], "its own dates alone");
    assert_eq!(data.first_on, [(seen, oct(2))]);
    let coverage: Vec<Currency> = data.coverage.iter().map(|one| one.currency).collect();
    assert_eq!(coverage, [cur("GBP"), cur("USD")]);
}

#[tokio::test]
async fn a_scope_of_nothing_reads_only_the_setting() {
    let (_dir, store) = open().await;
    let hidden = holding(&store, "JPY").await;
    snap(&store, hidden, oct(1), 100).await;
    fx(&store, "JPY", &[(oct(1), "170")]).await;
    fx(&store, "USD", &[(oct(1), "1.1")]).await;
    show_in(&store, "USD").await;
    rebuild(&store, oct(3)).await;

    let data = by_day(&store, &AccountScope::Only(Vec::new()), oct(1), oct(3)).await;

    assert_eq!(listed(&data), []);
    assert_eq!(data.rows, []);
    assert_eq!(data.first_on, []);
    assert_eq!(data.display, Some(cur("USD")));
    assert_eq!(
        data.coverage
            .iter()
            .map(|one| one.currency)
            .collect::<Vec<_>>(),
        [cur("USD")]
    );
}

#[tokio::test]
async fn accounts_come_oldest_first() {
    let (_dir, store) = open().await;
    let first = holding(&store, "USD").await;
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    let second = holding(&store, "USD").await;

    let data = by_day(&store, &AccountScope::All, oct(1), oct(3)).await;

    assert_eq!(listed(&data), [first, second]);
}

#[tokio::test]
async fn excluded_accounts_have_no_rows_but_are_listed() {
    let (_dir, store) = open().await;
    let counted = holding(&store, "USD").await;
    let excluded = holding_with(&store, "USD", false).await;
    snap(&store, counted, oct(1), 100).await;
    snap(&store, excluded, oct(1), 999).await;
    rebuild(&store, oct(2)).await;

    let data = by_day(&store, &AccountScope::All, oct(1), oct(2)).await;

    assert_eq!(listed(&data).len(), 2);
    assert!(listed(&data).contains(&excluded));
    assert_eq!(holders(&data), [counted]);
    assert_eq!(data.rows.len(), 2);
}

/// Closing applies at read (N9): a closed account is listed and keeps its rows.
#[tokio::test]
async fn closed_accounts_are_read_like_any_other() {
    let (_dir, store) = open().await;
    let closed = holding(&store, "USD").await;
    snap(&store, closed, oct(1), -100).await;
    rebuild(&store, oct(2)).await;
    let mut tx = store.begin().await.unwrap();
    tx.update_account(
        closed,
        AccountChanges {
            closed_at: Some(Some(jiff::Timestamp::UNIX_EPOCH)),
            ..AccountChanges::default()
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let data = by_day(&store, &AccountScope::All, oct(1), oct(2)).await;

    assert_eq!(listed(&data), [closed]);
    assert_eq!(holders(&data), [closed]);
    assert_eq!(data.first_on, [(closed, oct(1))]);
}

#[tokio::test]
async fn a_row_in_another_currency_fails_loudly() {
    let (_dir, store) = open().await;
    let one = holding(&store, "USD").await;
    snap(&store, one, oct(1), 100).await;
    show_in(&store, "USD").await;
    rebuild(&store, oct(2)).await;
    store
        .execute_unchecked_for_tests(
            "UPDATE account_daily_values SET display_currency = 'EUR' WHERE on_date = ?",
            &oct(2).to_string(),
        )
        .await
        .unwrap();

    let refused = store
        .net_worth_data(&AccountScope::All, oct(1), oct(2), NetWorthInterval::Day)
        .await
        .unwrap_err();

    assert!(matches!(refused, StoreError::Backend(_)));
}

/// With rows made while none was set, and a setting since, even a read of no
/// conversion says nothing: the row and the setting must agree.
#[tokio::test]
async fn a_row_with_no_currency_under_a_setting_fails_loudly() {
    let (_dir, store) = open().await;
    let one = holding(&store, "USD").await;
    snap(&store, one, oct(1), 100).await;
    show_in(&store, "USD").await;
    rebuild(&store, oct(1)).await;
    store
        .execute_unchecked_for_tests(
            "UPDATE account_daily_values SET display_currency = NULL, display_minor = NULL, \
             conversion = 'no_display_currency' WHERE on_date = ?",
            &oct(1).to_string(),
        )
        .await
        .unwrap();

    let refused = store
        .net_worth_data(&AccountScope::All, oct(1), oct(1), NetWorthInterval::Day)
        .await;

    assert!(matches!(refused, Err(StoreError::Backend(_))));
}

#[tokio::test]
async fn reads_use_the_primary_key() {
    let (_dir, store) = open().await;
    let one = holding(&store, "USD").await;
    let two = holding(&store, "USD").await;
    let dates: Vec<Date> = date(2026, 1, 1)
        .series(jiff::ToSpan::days(1))
        .take(365)
        .collect();

    let plan = store
        .explain_net_worth_rows_for_tests(&[one, two], &dates)
        .await
        .unwrap()
        .join("\n");

    assert!(plan.contains("PRIMARY KEY"), "{plan}");
    assert!(!plan.contains("SCAN"), "{plan}");
}

/// An included account's row gone is a hole: no total, the account named, and
/// never read as money leaving.
#[tokio::test]
async fn a_hole_is_reported() {
    let (_dir, store) = open().await;
    let one = holding(&store, "USD").await;
    let two = holding(&store, "USD").await;
    snap(&store, one, oct(1), 100).await;
    snap(&store, two, oct(1), 200).await;
    show_in(&store, "USD").await;
    rebuild(&store, oct(3)).await;
    store
        .execute_unchecked_for_tests(
            "DELETE FROM account_daily_values WHERE on_date = ? AND balance_minor = 100",
            &oct(2).to_string(),
        )
        .await
        .unwrap();

    let data = by_day(&store, &AccountScope::All, oct(1), oct(3)).await;
    let report = report_of(data, oct(1), oct(3));

    let totals: Vec<_> = report
        .points
        .iter()
        .map(|point| point.total.map(|total| total.minor_units))
        .collect();
    assert_eq!(totals, [Some(300), None, Some(300)]);
    assert_eq!(report.points[1].unconverted, [one]);
    assert_eq!(
        report.cautions,
        [NetWorthCaution::DailyValueMissing {
            account: one,
            first_on: oct(2),
            last_on: oct(2),
        }]
    );
}

/// `first_on` comes from what the account holds, not from its rows, so a hole
/// on its very first day is one as well.
#[tokio::test]
async fn a_hole_on_the_first_day_is_reported() {
    let (_dir, store) = open().await;
    let one = holding(&store, "USD").await;
    snap(&store, one, oct(1), 100).await;
    show_in(&store, "USD").await;
    rebuild(&store, oct(2)).await;
    store
        .execute_unchecked_for_tests(
            "DELETE FROM account_daily_values WHERE on_date = ?",
            &oct(1).to_string(),
        )
        .await
        .unwrap();

    let data = by_day(&store, &AccountScope::All, oct(1), oct(2)).await;
    assert_eq!(data.first_on, [(one, oct(1))]);

    let report = report_of(data, oct(1), oct(2));
    assert_eq!(report.points[0].total, None);
    assert_eq!(report.points[0].unconverted, [one]);
}

/// The day an account arrives is no hole, and neither are the days before it.
#[tokio::test]
async fn an_account_arriving_later_is_no_hole() {
    let (_dir, store) = open().await;
    let one = holding(&store, "USD").await;
    let late = holding(&store, "USD").await;
    snap(&store, one, oct(1), 100).await;
    snap(&store, late, oct(2), 200).await;
    show_in(&store, "USD").await;
    rebuild(&store, oct(3)).await;

    let data = by_day(&store, &AccountScope::All, oct(1), oct(3)).await;
    let report = report_of(data, oct(1), oct(3));

    assert_eq!(report.cautions, []);
    let totals: Vec<_> = report
        .points
        .iter()
        .map(|point| point.total.map(|total| total.minor_units))
        .collect();
    assert_eq!(totals, [Some(100), Some(300), Some(300)]);
}

#[tokio::test]
async fn first_on_starts_no_earlier_than_1970() {
    let (_dir, store) = open().await;
    let one = holding(&store, "USD").await;
    snap(&store, one, date(1, 1, 1), 100).await;
    rebuild(&store, date(1970, 1, 3)).await;

    let data = by_day(
        &store,
        &AccountScope::All,
        date(1970, 1, 1),
        date(1970, 1, 3),
    )
    .await;

    assert_eq!(data.first_on, [(one, date(1970, 1, 1))]);
    assert_eq!(data.rows.len(), 3);
}

/// A snapshot dated after the last filled day has no value yet: it arrives with
/// the fill, and no earlier point may take it as one.
#[tokio::test]
async fn a_snapshot_after_through_gives_no_first_day() {
    let (_dir, store) = open().await;
    let one = holding(&store, "USD").await;
    snap(&store, one, oct(9), 100).await;
    rebuild(&store, oct(5)).await;

    let data = by_day(&store, &AccountScope::All, oct(1), oct(5)).await;

    assert_eq!(data.first_on, []);
    assert_eq!(data.rows, []);
}

/// The first date moves with a back-dated first snapshot, in the table's own
/// transaction: reading it right after has rows from there.
#[tokio::test]
async fn first_on_follows_a_back_dated_snapshot() {
    let (_dir, store) = open().await;
    let one = holding(&store, "USD").await;
    snap(&store, one, oct(9), 100).await;
    rebuild(&store, oct(12)).await;

    snap(&store, one, oct(3), 50).await;

    let data = by_day(&store, &AccountScope::All, oct(1), oct(12)).await;
    assert_eq!(data.first_on, [(one, oct(3))]);
    assert_eq!(on_days(&data), days(3, 12));
}

/// A dollar account shown in a discontinued currency: the stored row says the old
/// rate is the display currency's, so the report names that one.
#[tokio::test]
async fn a_stored_row_names_the_leg_of_its_rate() {
    let (_dir, store) = open().await;
    let one = holding(&store, "USD").await;
    snap(&store, one, oct(1), 100).await;
    fx(&store, "USD", &[(oct(1), "1.1")]).await;
    fx(&store, "RUB", &[(date(2022, 3, 1), "117.2")]).await;
    show_in(&store, "RUB").await;
    rebuild(&store, oct(1)).await;

    let data = by_day(&store, &AccountScope::All, oct(1), oct(1)).await;
    assert_eq!(data.rows[0].rate_leg, Some(RateLeg::Display));
    let report = report_of(data, oct(1), oct(1));

    assert!(matches!(
        report.cautions.as_slice(),
        [NetWorthCaution::RateStale { currency, .. }] if *currency == cur("RUB")
    ));
}

#[tokio::test]
async fn the_read_carries_the_settings() {
    let (_dir, store) = open().await;
    let data = by_day(&store, &AccountScope::All, oct(1), oct(2)).await;
    assert_eq!(data.display, None);
    assert_eq!(
        data.stale_after_days,
        ForecastSettings::default().stale_after_days
    );
}

/// The service refuses such a range before it asks; if one comes, the store does
/// not read it.
#[tokio::test]
async fn a_range_past_the_point_limit_is_refused() {
    let (_dir, store) = open().await;
    let one = holding(&store, "USD").await;
    snap(&store, one, oct(1), 100).await;
    rebuild(&store, oct(5)).await;

    let refused = store
        .net_worth_data(
            &AccountScope::All,
            date(2020, 1, 1),
            oct(5),
            NetWorthInterval::Day,
        )
        .await
        .unwrap_err();

    assert!(matches!(refused, StoreError::Invalid(_)));
}
