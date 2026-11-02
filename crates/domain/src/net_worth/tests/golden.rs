//! The golden household, worked out by hand.
//!
//! EUR to USD: 10-26 1.1188, 10-27 1.115, 10-28 1.1133, 10-29 1.113, 10-30
//! 1.1172, none at the weekend. Checking USD 5,000.00 from 10-26 and 5,500.00
//! from 10-30. Livret EUR 1,000.00 from 10-26. Card USD -300.00 from 10-28. Side
//! USD 999.00, not in net worth.
//!
//! | Day   | Checking | Livret  | Card    | Total   |
//! |-------|----------|---------|---------|---------|
//! | 10-26 | 500,000  | 111,880 |         | 611,880 |
//! | 10-27 | 500,000  | 111,500 |         | 611,500 |
//! | 10-28 | 500,000  | 111,330 | -30,000 | 581,330 |
//! | 10-29 | 500,000  | 111,300 | -30,000 | 581,300 |
//! | 10-30 | 550,000  | 111,720 | -30,000 | 631,720 |
//! | 10-31 | 550,000  | 111,720 | -30,000 | 631,720 |
//! | 11-01 | 550,000  | 111,720 | -30,000 | 631,720 |
//!
//! In euros on 10-30: 550,000 / 1.1172 = 492,302.18 and -30,000 / 1.1172 =
//! -26,852.85, so 492,302 - 26,853 + 100,000 = 565,449.

use jiff::civil::{Date, date};

use super::{Holding, TODAY, cash, holding, id, input, money, point_on, rates};
use crate::accounts::LiquidityClass;
use crate::fx::RateBook;
use crate::net_worth::{
    AccountSeries, ConversionState, LiquidityLine, NetWorthInput, NetWorthInterval, NetWorthReport,
    NetWorthStatus, NotCountedReason, net_worth_report,
};

pub(super) const CHECKING: u128 = 1;
const LIVRET: u128 = 2;
pub(super) const CARD: u128 = 3;
pub(super) const SIDE: u128 = 4;

pub(super) fn book() -> RateBook {
    rates(&[(
        "USD",
        &[
            (date(2026, 10, 26), "1.1188"),
            (date(2026, 10, 27), "1.115"),
            (date(2026, 10, 28), "1.1133"),
            (date(2026, 10, 29), "1.113"),
            (date(2026, 10, 30), "1.1172"),
        ],
    )])
}

/// The accounts of the golden household.
pub(super) fn holdings() -> Vec<Holding> {
    let side_excluded = {
        let mut side = cash(SIDE, "USD", &[(date(2026, 10, 26), 99_900)]);
        side.line.include_in_net_worth = false;
        side
    };
    vec![
        cash(
            CHECKING,
            "USD",
            &[(date(2026, 10, 26), 500_000), (date(2026, 10, 30), 550_000)],
        ),
        cash(LIVRET, "EUR", &[(date(2026, 10, 26), 100_000)]),
        holding(
            CARD,
            "USD",
            LiquidityClass::ShortTermLiability,
            &[(date(2026, 10, 28), -30_000)],
        ),
        side_excluded,
    ]
}

fn household(display: &str) -> NetWorthInput {
    input(&holdings(), Some(display), &book())
}

fn series(report: &NetWorthReport, n: u128) -> &AccountSeries {
    report
        .accounts
        .iter()
        .find(|line| line.account == id(n))
        .expect("a series")
}

fn seen_on(line: &AccountSeries, on: Date) -> crate::net_worth::AccountPoint {
    *line
        .points
        .iter()
        .find(|point| point.on == on)
        .expect("a point")
}

#[test]
fn golden_week_of_net_worth() {
    let report = net_worth_report(&household("USD")).unwrap();

    let totals: Vec<(Date, i64)> = report
        .points
        .iter()
        .map(|point| (point.on, point.total.unwrap().minor_units))
        .collect();
    assert_eq!(
        totals,
        [
            (date(2026, 10, 26), 611_880),
            (date(2026, 10, 27), 611_500),
            (date(2026, 10, 28), 581_330),
            (date(2026, 10, 29), 581_300),
            (date(2026, 10, 30), 631_720),
            (date(2026, 10, 31), 631_720),
            (date(2026, 11, 1), 631_720),
        ]
    );
    assert_eq!(report.status, NetWorthStatus::Firm);
    assert_eq!(report.blockers, []);
    assert_eq!(report.cautions, []);
    assert_eq!(report.display_currency, Some(super::currency("USD")));
    assert_eq!(report.through, Some(TODAY));

    let friday = date(2026, 10, 30);
    let point = point_on(&report, friday);
    assert_eq!(point.total, Some(money(631_720, "USD")));
    assert_eq!(
        point.by_liquidity,
        [
            LiquidityLine {
                liquidity: LiquidityClass::Cash,
                amount: Some(money(661_720, "USD")),
            },
            LiquidityLine {
                liquidity: LiquidityClass::ShortTermLiability,
                amount: Some(money(-30_000, "USD")),
            },
        ]
    );
    assert_eq!(
        point.by_currency,
        [money(100_000, "EUR"), money(520_000, "USD")]
    );
    assert_eq!(point.unconverted, []);
    assert_eq!(
        seen_on(series(&report, LIVRET), friday).rate_on,
        Some(friday)
    );

    let saturday = date(2026, 10, 31);
    assert_eq!(
        seen_on(series(&report, LIVRET), saturday).rate_on,
        Some(friday)
    );
    assert_eq!(
        seen_on(series(&report, CARD), saturday).balance_on,
        date(2026, 10, 28)
    );
    assert_eq!(
        seen_on(series(&report, LIVRET), saturday).balance,
        money(100_000, "EUR")
    );
}

#[test]
fn golden_excluded_account_is_listed_in_no_sum() {
    let report = net_worth_report(&household("USD")).unwrap();
    let side = series(&report, SIDE);
    assert!(!side.counted);
    assert_eq!(side.not_counted, Some(NotCountedReason::ExcludedByFlag));
    assert_eq!(side.points, []);
    assert!(series(&report, CHECKING).counted);
    assert_eq!(series(&report, CHECKING).not_counted, None);
}

#[test]
fn golden_by_week_and_month() {
    let mut by_week = household("USD");
    by_week.interval = NetWorthInterval::Week;
    let report = net_worth_report(&by_week).unwrap();
    assert_eq!(report.points.len(), 1);
    let point = &report.points[0];
    assert_eq!(point.on, date(2026, 11, 1));
    assert_eq!(point.period_start, date(2026, 10, 26));
    assert!(!point.partial);
    assert_eq!(point.total, Some(money(631_720, "USD")));

    let mut by_month = household("USD");
    by_month.interval = NetWorthInterval::Month;
    let report = net_worth_report(&by_month).unwrap();
    assert_eq!(report.points.len(), 2);
    let october = &report.points[0];
    assert_eq!(october.on, date(2026, 10, 31));
    assert_eq!(october.period_start, date(2026, 10, 26));
    assert!(!october.partial);
    assert_eq!(october.total, Some(money(631_720, "USD")));
    let november = &report.points[1];
    assert_eq!(november.on, date(2026, 11, 1));
    assert_eq!(november.period_start, date(2026, 11, 1));
    assert!(november.partial);
    assert_eq!(november.total, Some(money(631_720, "USD")));
}

#[test]
fn golden_in_euros() {
    let report = net_worth_report(&household("EUR")).unwrap();
    let friday = date(2026, 10, 30);
    let checking = seen_on(series(&report, CHECKING), friday);
    let card = seen_on(series(&report, CARD), friday);
    let livret = seen_on(series(&report, LIVRET), friday);
    assert_eq!(checking.amount, Some(money(492_302, "EUR")));
    assert_eq!(checking.conversion, ConversionState::Converted);
    assert_eq!(card.amount, Some(money(-26_853, "EUR")));
    assert_eq!(livret.amount, Some(money(100_000, "EUR")));
    assert_eq!(livret.conversion, ConversionState::Same);
    assert_eq!(livret.rate_on, None);
    assert_eq!(
        point_on(&report, friday).total,
        Some(money(492_302 - 26_853 + 100_000, "EUR"))
    );
    assert_eq!(point_on(&report, friday).total, Some(money(565_449, "EUR")));
}
