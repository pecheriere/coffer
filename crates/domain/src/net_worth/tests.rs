//! Expected values are worked out by hand from the dates and amounts in each
//! test, never taken from the code under test. The golden week's rates are the
//! ECB's, of 2026-10-26 to 2026-10-30.

use jiff::civil::{Date, date};

use super::{
    CurrencyState, DailyValue, NetWorthInput, NetWorthInterval, NetWorthPoint, NetWorthReport,
    ReportAccount, SnapshotPoint, daily_values,
};
use crate::accounts::{BalanceSource, LiquidityClass};
use crate::fx::{RateBook, RateHistory, RateObservation};
use crate::ids::AccountId;
use crate::money::{Currency, Money};

mod daily;
mod golden;
mod periods;
mod properties;
mod report_cautions;
mod report_rules;

const TODAY: Date = date(2026, 11, 2);
/// Far enough back to reach every snapshot in these tests.
const EARLIEST: Date = date(2020, 1, 1);

fn currency(code: &str) -> Currency {
    Currency::parse(code).unwrap()
}

fn money(minor_units: i64, code: &str) -> Money {
    Money {
        minor_units,
        currency: currency(code),
    }
}

fn id(n: u128) -> AccountId {
    AccountId::from_uuid(uuid::Uuid::from_u128(n))
}

fn snapshot(on: Date, balance_minor: i64) -> SnapshotPoint {
    SnapshotPoint {
        on,
        balance_minor,
        source: BalanceSource::Sync,
    }
}

fn rates(entries: &[(&str, &[(Date, &str)])]) -> RateBook {
    let mut book = RateBook::default();
    for &(code, days) in entries {
        let observations = days
            .iter()
            .map(|&(on, text)| RateObservation {
                on,
                rate: text.parse().unwrap(),
            })
            .collect();
        book.insert(currency(code), RateHistory::new(observations));
    }
    book
}

/// An account and its snapshots.
#[derive(Debug)]
struct Holding {
    line: ReportAccount,
    snapshots: Vec<SnapshotPoint>,
}

fn holding(n: u128, code: &str, liquidity: LiquidityClass, snapshots: &[(Date, i64)]) -> Holding {
    Holding {
        line: ReportAccount {
            id: id(n),
            name: format!("Holding {n}"),
            currency: currency(code),
            liquidity,
            include_in_net_worth: true,
            closed_on: None,
        },
        snapshots: snapshots
            .iter()
            .map(|&(on, minor)| snapshot(on, minor))
            .collect(),
    }
}

fn cash(n: u128, code: &str, snapshots: &[(Date, i64)]) -> Holding {
    holding(n, code, LiquidityClass::Cash, snapshots)
}

/// A report's input over `holdings`, through today, with daily values made by
/// [`daily_values`] as the store does.
fn input(holdings: &[Holding], display: Option<&str>, book: &RateBook) -> NetWorthInput {
    let display = display.map(currency);
    let rows: Vec<DailyValue> = holdings
        .iter()
        .flat_map(|held| {
            daily_values(
                held.line.id,
                held.line.currency,
                &held.snapshots,
                display,
                book,
                EARLIEST,
                TODAY,
            )
        })
        .collect();
    let first_on = holdings
        .iter()
        .filter_map(|held| {
            let first = rows.iter().find(|row| row.account == held.line.id)?;
            Some((held.line.id, first.on))
        })
        .collect();
    NetWorthInput {
        today: TODAY,
        from: date(2026, 10, 26),
        to: date(2026, 11, 1),
        interval: NetWorthInterval::Day,
        through: Some(TODAY),
        rebuild_needed: false,
        display,
        stale_after_days: 7,
        account_points: true,
        accounts: holdings.iter().map(|held| held.line.clone()).collect(),
        rows,
        first_on,
        coverage: Vec::<CurrencyState>::new(),
    }
}

fn point_on(report: &NetWorthReport, on: Date) -> &NetWorthPoint {
    report
        .points
        .iter()
        .find(|point| point.on == on)
        .expect("a point on that day")
}
