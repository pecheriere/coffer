//! A day an account should have a value and doesn't, and a range outliving what
//! it counts: neither may read as money moving or as the value now.

use jiff::civil::{Date, date};

use super::{TODAY, cash, holding, id, input, money, point_on};
use crate::accounts::LiquidityClass;
use crate::fx::RateBook;
use crate::net_worth::{
    LiquidityLine, NetWorthBlocker, NetWorthCaution, NetWorthInput, NetWorthInterval,
    NetWorthStatus, net_worth_report,
};

const OCT_25: Date = date(2026, 10, 25);
const OCT_26: Date = date(2026, 10, 26);
const OCT_27: Date = date(2026, 10, 27);
const OCT_28: Date = date(2026, 10, 28);
const OCT_29: Date = date(2026, 10, 29);
const NOV_1: Date = date(2026, 11, 1);

fn no_rates() -> RateBook {
    RateBook::default()
}

fn without_row(asked: &mut NetWorthInput, n: u128, on: Date) {
    let before = asked.rows.len();
    asked
        .rows
        .retain(|row| !(row.account == id(n) && row.on == on));
    assert_eq!(asked.rows.len(), before - 1, "one row taken out");
}

#[test]
fn a_missing_row_nulls_the_total_and_names_the_account() {
    let held = [
        cash(1, "USD", &[(OCT_25, 100_000)]),
        cash(2, "USD", &[(OCT_25, 5_000)]),
    ];
    let mut asked = input(&held, Some("USD"), &no_rates());
    without_row(&mut asked, 1, OCT_28);
    let report = net_worth_report(&asked).unwrap();

    let point = point_on(&report, OCT_28);
    assert_eq!(point.total, None);
    assert_eq!(
        point.by_liquidity,
        [LiquidityLine {
            liquidity: LiquidityClass::Cash,
            amount: None,
        }]
    );
    assert_eq!(point.by_currency, [money(5_000, "USD")]);
    assert_eq!(point.unconverted, [id(1)]);
    assert_eq!(point_on(&report, OCT_27).total, Some(money(105_000, "USD")));
    assert_eq!(
        report.cautions,
        [NetWorthCaution::DailyValueMissing {
            account: id(1),
            first_on: OCT_28,
            last_on: OCT_28,
        }]
    );
    assert_eq!(report.status, NetWorthStatus::Provisional);
}

#[test]
fn a_missing_row_nulls_only_its_own_class() {
    let held = [
        cash(1, "USD", &[(OCT_25, 100)]),
        holding(
            2,
            "USD",
            LiquidityClass::ShortTermLiability,
            &[(OCT_25, -5)],
        ),
    ];
    let mut asked = input(&held, Some("USD"), &no_rates());
    without_row(&mut asked, 2, OCT_28);
    let report = net_worth_report(&asked).unwrap();
    assert_eq!(
        point_on(&report, OCT_28).by_liquidity,
        [
            LiquidityLine {
                liquidity: LiquidityClass::Cash,
                amount: Some(money(100, "USD")),
            },
            LiquidityLine {
                liquidity: LiquidityClass::ShortTermLiability,
                amount: None,
            },
        ]
    );
}

/// Left out, the period would read as a gap nothing explains.
#[test]
fn a_missing_row_keeps_a_period_it_is_alone_in() {
    let held = [cash(1, "USD", &[(OCT_25, 100)])];
    let mut asked = input(&held, Some("USD"), &no_rates());
    without_row(&mut asked, 1, OCT_28);
    let report = net_worth_report(&asked).unwrap();
    let point = point_on(&report, OCT_28);
    assert_eq!(point.total, None);
    assert_eq!(point.by_currency, []);
    assert_eq!(point.unconverted, [id(1)]);
    assert_eq!(report.blockers, []);
}

#[test]
fn no_row_on_the_first_day_itself_is_missing() {
    let held = [
        cash(1, "USD", &[(OCT_26, 100)]),
        cash(2, "USD", &[(OCT_25, 5)]),
    ];
    let mut asked = input(&held, Some("USD"), &no_rates());
    without_row(&mut asked, 1, OCT_26);
    let report = net_worth_report(&asked).unwrap();
    assert_eq!(point_on(&report, OCT_26).total, None);
    assert_eq!(point_on(&report, OCT_26).unconverted, [id(1)]);
}

#[test]
fn no_row_before_the_first_day_is_not_missing() {
    let held = [
        cash(1, "USD", &[(OCT_28, 100)]),
        cash(2, "USD", &[(OCT_25, 5)]),
    ];
    let report = net_worth_report(&input(&held, Some("USD"), &no_rates())).unwrap();
    assert_eq!(point_on(&report, OCT_26).total, Some(money(5, "USD")));
    assert_eq!(report.status, NetWorthStatus::Firm);
}

#[test]
fn an_excluded_account_with_no_row_is_not_missing() {
    let mut excluded = cash(1, "USD", &[(OCT_25, 100)]);
    excluded.line.include_in_net_worth = false;
    let held = [excluded, cash(2, "USD", &[(OCT_25, 5)])];
    let mut asked = input(&held, Some("USD"), &no_rates());
    without_row(&mut asked, 1, OCT_28);
    let report = net_worth_report(&asked).unwrap();
    assert_eq!(point_on(&report, OCT_28).total, Some(money(5, "USD")));
    assert_eq!(report.status, NetWorthStatus::Firm);
}

/// Its rows go on being filled after closing (N9), and a debt may be in the one
/// that is gone.
#[test]
fn a_closed_account_with_no_row_is_missing_too() {
    let mut closed = cash(1, "USD", &[(OCT_25, -100)]);
    closed.line.closed_on = Some(OCT_27);
    let held = [closed, cash(2, "USD", &[(OCT_25, 5)])];
    let mut asked = input(&held, Some("USD"), &no_rates());
    without_row(&mut asked, 1, OCT_28);
    let report = net_worth_report(&asked).unwrap();
    assert_eq!(point_on(&report, OCT_28).total, None);
    assert_eq!(point_on(&report, OCT_28).unconverted, [id(1)]);
}

/// The blocker explains the null total, not a balance absent from a native sum.
#[test]
fn a_missing_row_is_named_with_no_display_currency() {
    let held = [
        cash(1, "USD", &[(OCT_25, 100)]),
        cash(2, "USD", &[(OCT_25, 5)]),
    ];
    let mut asked = input(&held, None, &no_rates());
    without_row(&mut asked, 1, OCT_28);
    let report = net_worth_report(&asked).unwrap();
    assert_eq!(point_on(&report, OCT_28).unconverted, [id(1)]);
    assert_eq!(point_on(&report, OCT_27).unconverted, []);
    assert_eq!(
        report.cautions,
        [NetWorthCaution::DailyValueMissing {
            account: id(1),
            first_on: OCT_28,
            last_on: OCT_28,
        }]
    );
}

#[test]
fn a_hole_of_several_points_is_said_once_with_its_ends() {
    let held = [
        cash(1, "USD", &[(OCT_25, 100)]),
        cash(2, "USD", &[(OCT_25, 5)]),
    ];
    let mut asked = input(&held, Some("USD"), &no_rates());
    without_row(&mut asked, 1, OCT_27);
    without_row(&mut asked, 1, OCT_28);
    without_row(&mut asked, 1, OCT_29);
    let report = net_worth_report(&asked).unwrap();
    assert_eq!(
        report.cautions,
        [NetWorthCaution::DailyValueMissing {
            account: id(1),
            first_on: OCT_27,
            last_on: OCT_29,
        }]
    );
}

#[test]
fn unconverted_lists_every_reason_in_the_accounts_order() {
    let held = [
        cash(1, "THB", &[(OCT_25, 100)]),
        cash(2, "USD", &[(OCT_25, 100)]),
        cash(3, "GBP", &[(OCT_25, 100)]),
    ];
    let mut asked = input(&held, Some("USD"), &no_rates());
    without_row(&mut asked, 2, OCT_28);
    let report = net_worth_report(&asked).unwrap();
    assert_eq!(point_on(&report, OCT_28).unconverted, [id(1), id(2), id(3)]);
}

#[test]
fn a_series_says_when_it_closed() {
    let mut closed = cash(1, "USD", &[(OCT_25, 100)]);
    closed.line.closed_on = Some(OCT_29);
    let held = [closed, cash(2, "USD", &[(OCT_25, 5)])];
    let mut asked = input(&held, Some("USD"), &no_rates());
    asked.account_points = false;
    let report = net_worth_report(&asked).unwrap();
    assert_eq!(report.accounts[0].closed_on, Some(OCT_29));
    assert_eq!(report.accounts[1].closed_on, None);
    assert_eq!(report.accounts[0].points, []);
}

#[test]
fn a_range_outliving_a_sole_closed_account_says_so() {
    let mut closed = cash(1, "USD", &[(OCT_25, 100)]);
    closed.line.closed_on = Some(OCT_29);
    let report = net_worth_report(&input(&[closed], Some("USD"), &no_rates())).unwrap();
    assert_eq!(report.points.last().unwrap().on, OCT_28);
    assert_eq!(
        report.cautions,
        [NetWorthCaution::NothingCountedAfterLastPoint {
            from: OCT_29,
            to: NOV_1,
        }]
    );
    assert_eq!(report.status, NetWorthStatus::Provisional);
}

/// By month, a month's value is its last day's, so nothing counted is said from
/// the first month whose last day it missed.
#[test]
fn a_later_month_with_nothing_counted_is_said_from_its_start() {
    let mut closed = cash(1, "USD", &[(date(2026, 9, 1), 100)]);
    closed.line.closed_on = Some(OCT_29);
    let mut asked = input(&[closed], Some("USD"), &no_rates());
    asked.interval = NetWorthInterval::Month;
    asked.from = date(2026, 9, 1);
    asked.to = date(2026, 11, 30);
    asked.stale_after_days = u16::MAX;
    let report = net_worth_report(&asked).unwrap();
    assert_eq!(report.points.len(), 1);
    assert_eq!(report.points[0].on, date(2026, 9, 30));
    assert_eq!(
        report.cautions,
        [NetWorthCaution::NothingCountedAfterLastPoint {
            from: date(2026, 10, 1),
            to: TODAY,
        }]
    );
}

#[test]
fn a_last_point_at_the_end_of_what_is_filled_says_nothing_of_it() {
    let mut closed = cash(1, "USD", &[(OCT_25, 100)]);
    closed.line.closed_on = Some(OCT_29);
    let mut asked = input(&[closed], Some("USD"), &no_rates());
    asked.through = Some(OCT_28);
    let report = net_worth_report(&asked).unwrap();
    assert_eq!(report.points.last().unwrap().on, OCT_28);
    assert_eq!(
        report.cautions,
        [NetWorthCaution::ToDateNotYetFilled { through: OCT_28 }]
    );
}

#[test]
fn another_account_still_counting_says_nothing_of_it() {
    let mut closed = cash(1, "USD", &[(OCT_25, 100)]);
    closed.line.closed_on = Some(OCT_29);
    let held = [closed, cash(2, "USD", &[(OCT_25, 5)])];
    let report = net_worth_report(&input(&held, Some("USD"), &no_rates())).unwrap();
    assert_eq!(report.points.last().unwrap().on, NOV_1);
    assert_eq!(report.status, NetWorthStatus::Firm);
}

#[test]
fn no_points_at_all_is_a_blocker_and_nothing_more() {
    let mut closed = cash(1, "USD", &[(OCT_25, 100)]);
    closed.line.closed_on = Some(OCT_25);
    let report = net_worth_report(&input(&[closed], Some("USD"), &no_rates())).unwrap();
    assert_eq!(report.blockers, [NetWorthBlocker::NoCountedAccounts]);
    assert_eq!(report.cautions, []);
}
