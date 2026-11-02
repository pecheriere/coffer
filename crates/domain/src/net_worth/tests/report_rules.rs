//! What counts, what is answered at all, and which points there are.

use jiff::civil::date;

use super::{TODAY, cash, currency, holding, id, input, money, point_on, rates};
use crate::accounts::{BalanceSource, LiquidityClass};
use crate::fx::RateBook;
use crate::net_worth::{
    ConversionState, DailyValue, LiquidityLine, NetWorthBlocker, NetWorthCaution, NetWorthError,
    NetWorthInterval, NetWorthStatus, net_worth_report,
};

const OCT_26: jiff::civil::Date = date(2026, 10, 26);

fn no_rates() -> RateBook {
    RateBook::default()
}

#[test]
fn display_currency_not_set_has_sums_but_no_total() {
    let held = [
        cash(1, "USD", &[(OCT_26, 100)]),
        cash(2, "EUR", &[(OCT_26, 200)]),
    ];
    let report = net_worth_report(&input(&held, None, &no_rates())).unwrap();
    assert_eq!(report.blockers, [NetWorthBlocker::DisplayCurrencyNotSet]);
    assert_eq!(report.status, NetWorthStatus::Unavailable);
    assert_eq!(report.display_currency, None);
    assert_eq!(report.points.len(), 7);
    let point = &report.points[0];
    assert_eq!(point.total, None);
    assert_eq!(
        point.by_liquidity,
        [LiquidityLine {
            liquidity: LiquidityClass::Cash,
            amount: None,
        }]
    );
    assert_eq!(point.by_currency, [money(200, "EUR"), money(100, "USD")]);
    assert_eq!(point.unconverted, [], "the blocker says it, for every line");
    assert_eq!(report.cautions, []);
    assert_eq!(
        report.accounts[0].points[0].conversion,
        ConversionState::NoDisplayCurrency
    );
}

#[test]
fn rebuild_needed_answers_no_number() {
    let held = [cash(1, "USD", &[(OCT_26, 100)])];
    let mut asked = input(&held, Some("USD"), &no_rates());
    asked.rebuild_needed = true;
    let report = net_worth_report(&asked).unwrap();
    assert_eq!(report.blockers, [NetWorthBlocker::Rebuilding]);
    assert_eq!(report.status, NetWorthStatus::Unavailable);
    assert_eq!(report.points, []);
    assert_eq!(report.accounts, []);
    assert_eq!(report.cautions, []);
}

/// A state the store never leaves, but no filled day means no number either way.
#[test]
fn nothing_filled_answers_no_number() {
    let held = [cash(1, "USD", &[(OCT_26, 100)])];
    let mut asked = input(&held, Some("USD"), &no_rates());
    asked.through = None;
    let report = net_worth_report(&asked).unwrap();
    assert_eq!(report.blockers, [NetWorthBlocker::Rebuilding]);
    assert_eq!(report.points, []);
}

#[test]
fn no_counted_accounts_has_no_number_not_zero() {
    let report = net_worth_report(&input(&[], Some("USD"), &no_rates())).unwrap();
    assert_eq!(report.blockers, [NetWorthBlocker::NoCountedAccounts]);
    assert_eq!(report.status, NetWorthStatus::Unavailable);
    assert_eq!(report.points, []);
    assert_eq!(report.accounts, []);
}

#[test]
fn only_excluded_accounts_have_no_number() {
    let mut excluded = cash(1, "USD", &[(OCT_26, 100)]);
    excluded.line.include_in_net_worth = false;
    let report = net_worth_report(&input(&[excluded], Some("USD"), &no_rates())).unwrap();
    assert_eq!(report.blockers, [NetWorthBlocker::NoCountedAccounts]);
    assert_eq!(report.points, []);
    assert_eq!(report.accounts.len(), 1);
}

/// Not `firm` with nothing in it: an account with no balance yet, or a range
/// holding none of its balances, has no number to be firm about.
#[test]
fn an_account_with_no_counted_row_in_the_range_has_no_number() {
    let not_yet = cash(1, "USD", &[(date(2026, 11, 2), 100)]);
    let report = net_worth_report(&input(&[not_yet], Some("USD"), &no_rates())).unwrap();
    assert_eq!(report.blockers, [NetWorthBlocker::NoCountedAccounts]);
    assert_eq!(report.status, NetWorthStatus::Unavailable);
    assert_eq!(report.points, []);
}

#[test]
fn both_blockers_can_hold_at_once() {
    let report = net_worth_report(&input(&[], None, &no_rates())).unwrap();
    assert_eq!(
        report.blockers,
        [
            NetWorthBlocker::DisplayCurrencyNotSet,
            NetWorthBlocker::NoCountedAccounts,
        ]
    );
}

#[test]
fn include_flag_applies_at_read() {
    let held = [
        cash(1, "USD", &[(OCT_26, 100)]),
        cash(2, "USD", &[(OCT_26, 40)]),
    ];
    let mut asked = input(&held, Some("USD"), &no_rates());
    let before = net_worth_report(&asked).unwrap();
    assert_eq!(before.points[0].total, Some(money(140, "USD")));

    asked.accounts[1].include_in_net_worth = false;
    let after = net_worth_report(&asked).unwrap();
    assert_eq!(after.points.len(), before.points.len());
    for point in &after.points {
        assert_eq!(point.total, Some(money(100, "USD")));
        assert_eq!(point.by_currency, [money(100, "USD")]);
    }
}

#[test]
fn closed_asset_stops_counting_on_its_closing_day() {
    let closing = date(2026, 10, 29);
    let mut closed = cash(1, "USD", &[(OCT_26, 100)]);
    closed.line.closed_on = Some(closing);
    let open = cash(2, "USD", &[(OCT_26, 5)]);
    let report = net_worth_report(&input(&[closed, open], Some("USD"), &no_rates())).unwrap();
    let the_day_before = date(2026, 10, 28);
    assert_eq!(
        point_on(&report, the_day_before).total,
        Some(money(105, "USD"))
    );
    assert_eq!(point_on(&report, closing).total, Some(money(5, "USD")));
    assert_eq!(point_on(&report, closing).by_currency, [money(5, "USD")]);
    assert_eq!(report.cautions, [], "only money owed is cautioned");

    // Its rows stay, so a reader can see it stopped.
    let line = &report.accounts[0];
    let on = |day| line.points.iter().find(|point| point.on == day).unwrap();
    assert!(on(the_day_before).counted);
    assert!(!on(closing).counted);
    assert!(line.counted);
}

#[test]
fn closed_account_rules() {
    let closing = date(2026, 10, 29);
    let mut asset = cash(1, "USD", &[(OCT_26, 100)]);
    asset.line.closed_on = Some(closing);
    let mut owing = holding(
        2,
        "USD",
        LiquidityClass::ShortTermLiability,
        &[(OCT_26, -700)],
    );
    owing.line.closed_on = Some(closing);
    let report = net_worth_report(&input(&[asset, owing], Some("USD"), &no_rates())).unwrap();

    assert_eq!(
        point_on(&report, date(2026, 10, 28)).total,
        Some(money(-600, "USD"))
    );
    assert_eq!(point_on(&report, closing).total, Some(money(-700, "USD")));
    assert_eq!(
        point_on(&report, date(2026, 11, 1)).total,
        Some(money(-700, "USD"))
    );
    assert_eq!(
        report.cautions,
        [NetWorthCaution::ClosedAccountOwing {
            account: id(2),
            balance: money(-700, "USD"),
        }]
    );
    assert_eq!(report.status, NetWorthStatus::Provisional);
}

#[test]
fn a_closed_account_at_zero_stops_counting() {
    let closing = date(2026, 10, 29);
    let mut zero = cash(1, "USD", &[(OCT_26, 0)]);
    zero.line.closed_on = Some(closing);
    let open = cash(2, "USD", &[(OCT_26, 5)]);
    let report = net_worth_report(&input(&[zero, open], Some("USD"), &no_rates())).unwrap();
    assert_eq!(point_on(&report, closing).by_currency, [money(5, "USD")]);
    assert_eq!(report.cautions, []);
    let line = &report.accounts[0];
    let closed_point = line
        .points
        .iter()
        .find(|point| point.on == closing)
        .unwrap();
    assert!(!closed_point.counted);
}

#[test]
fn a_day_before_the_first_row_has_no_line() {
    let late = cash(1, "USD", &[(date(2026, 10, 30), 100)]);
    let early = cash(2, "USD", &[(OCT_26, 5)]);
    let report = net_worth_report(&input(&[late, early], Some("USD"), &no_rates())).unwrap();
    assert_eq!(point_on(&report, OCT_26).total, Some(money(5, "USD")));
    assert_eq!(report.status, NetWorthStatus::Firm);
    assert_eq!(report.accounts[0].points[0].on, date(2026, 10, 30));
}

#[test]
fn first_on_marks_an_account_arriving() {
    let arriving = date(2026, 10, 28);
    let late = cash(1, "USD", &[(arriving, 100)]);
    let early = cash(2, "USD", &[(date(2026, 9, 1), 5)]);
    let report = net_worth_report(&input(&[late, early], Some("USD"), &no_rates())).unwrap();
    assert_eq!(report.accounts[0].first_on, Some(arriving));
    assert_eq!(report.accounts[1].first_on, Some(date(2026, 9, 1)));
}

#[test]
fn a_period_with_no_counted_row_is_left_out() {
    let held = [cash(1, "USD", &[(date(2026, 10, 30), 100)])];
    let mut asked = input(&held, Some("USD"), &no_rates());
    asked.interval = NetWorthInterval::Month;
    asked.from = date(1970, 1, 1);
    asked.to = date(2026, 11, 1);
    asked.first_on.clear();
    let report = net_worth_report(&asked).unwrap();
    let days: Vec<_> = report.points.iter().map(|point| point.on).collect();
    assert_eq!(days, [date(2026, 10, 31), date(2026, 11, 1)]);
    assert_eq!(report.status, NetWorthStatus::Firm);
}

#[test]
fn a_closed_asset_alone_leaves_its_later_periods_out() {
    let mut closed = cash(1, "USD", &[(OCT_26, 100)]);
    closed.line.closed_on = Some(date(2026, 10, 29));
    let report = net_worth_report(&input(&[closed], Some("USD"), &no_rates())).unwrap();
    assert_eq!(report.points.last().unwrap().on, date(2026, 10, 28));
    let line = &report.accounts[0];
    assert_eq!(line.points.last().unwrap().on, date(2026, 10, 28));
}

#[test]
fn dates_after_through_are_left_out_and_said() {
    let through = date(2026, 10, 30);
    let held = [cash(1, "USD", &[(OCT_26, 100)])];
    let mut asked = input(&held, Some("USD"), &no_rates());
    asked.to = TODAY;
    asked.through = Some(through);
    let report = net_worth_report(&asked).unwrap();
    assert_eq!(report.points.last().unwrap().on, through);
    assert_eq!(report.through, Some(through));
    assert_eq!(
        report.cautions,
        [NetWorthCaution::ToDateNotYetFilled { through }]
    );
    assert_eq!(report.status, NetWorthStatus::Provisional);
    assert_eq!(report.to, TODAY);
}

#[test]
fn the_last_point_before_through_says_partial() {
    let through = date(2026, 10, 30);
    let held = [cash(1, "USD", &[(OCT_26, 100)])];
    let mut asked = input(&held, Some("USD"), &no_rates());
    asked.interval = NetWorthInterval::Month;
    asked.through = Some(through);
    let report = net_worth_report(&asked).unwrap();
    assert_eq!(report.points.len(), 1);
    assert_eq!(report.points[0].on, through);
    assert!(report.points[0].partial);
}

#[test]
fn a_range_wholly_after_through_is_said_with_no_points() {
    let held = [cash(1, "USD", &[(OCT_26, 100)])];
    let mut asked = input(&held, Some("USD"), &no_rates());
    let through = date(2026, 10, 30);
    asked.from = date(2026, 11, 1);
    asked.to = TODAY;
    asked.through = Some(through);
    let report = net_worth_report(&asked).unwrap();
    assert_eq!(report.points, []);
    assert_eq!(report.blockers, []);
    assert_eq!(
        report.cautions,
        [NetWorthCaution::ToDateNotYetFilled { through }]
    );
    assert_eq!(report.status, NetWorthStatus::Provisional);
}

#[test]
fn to_at_or_before_through_is_not_said() {
    let held = [cash(1, "USD", &[(OCT_26, 100)])];
    let mut asked = input(&held, Some("USD"), &no_rates());
    asked.to = date(2026, 10, 30);
    asked.through = Some(date(2026, 10, 30));
    assert_eq!(net_worth_report(&asked).unwrap().cautions, []);
}

#[test]
fn to_after_today_is_not_said_and_stops_at_today() {
    let held = [cash(1, "USD", &[(OCT_26, 100)])];
    let mut asked = input(&held, Some("USD"), &no_rates());
    asked.to = date(2026, 11, 30);
    let report = net_worth_report(&asked).unwrap();
    assert_eq!(report.points.last().unwrap().on, TODAY);
    assert_eq!(report.cautions, []);
}

/// A changed time zone can leave the filled days ahead of the household's day.
#[test]
fn a_filled_day_after_today_is_not_answered() {
    let held = [cash(1, "USD", &[(OCT_26, 100)])];
    let mut asked = input(&held, Some("USD"), &no_rates());
    asked.today = date(2026, 10, 30);
    asked.to = TODAY;
    let report = net_worth_report(&asked).unwrap();
    assert_eq!(report.points.last().unwrap().on, date(2026, 10, 30));
    assert_eq!(report.cautions, []);
}

#[test]
fn no_accounts_after_through_still_has_no_number() {
    let through = date(2026, 10, 30);
    let mut asked = input(&[], Some("USD"), &no_rates());
    asked.from = date(2026, 11, 1);
    asked.to = TODAY;
    asked.through = Some(through);
    let report = net_worth_report(&asked).unwrap();
    assert_eq!(report.blockers, [NetWorthBlocker::NoCountedAccounts]);
    assert_eq!(report.status, NetWorthStatus::Unavailable);
}

#[test]
fn a_point_lists_its_accounts_in_their_order() {
    let held = [
        cash(1, "THB", &[(OCT_26, 100)]),
        cash(2, "GBP", &[(OCT_26, 100)]),
    ];
    let mut asked = input(&held, Some("USD"), &no_rates());
    asked.rows.reverse();
    let report = net_worth_report(&asked).unwrap();
    assert_eq!(report.points[0].unconverted, [id(1), id(2)]);
}

#[test]
fn a_row_after_through_is_not_answered() {
    let held = [cash(1, "USD", &[(OCT_26, 100), (date(2026, 10, 31), 999)])];
    let mut asked = input(&held, Some("USD"), &no_rates());
    assert!(asked.rows.iter().any(|row| row.on == date(2026, 11, 1)));
    asked.through = Some(date(2026, 10, 30));
    let report = net_worth_report(&asked).unwrap();
    assert_eq!(report.points.last().unwrap().on, date(2026, 10, 30));
    assert_eq!(report.points.last().unwrap().total, Some(money(100, "USD")));
}

#[test]
fn a_refused_range_does_not_depend_on_any_state() {
    let held = [cash(1, "USD", &[(OCT_26, 100)])];
    let inverted = |rebuild_needed, through| {
        let mut asked = input(&held, Some("USD"), &no_rates());
        asked.from = date(2026, 11, 2);
        asked.to = date(2026, 11, 1);
        asked.rebuild_needed = rebuild_needed;
        asked.through = through;
        net_worth_report(&asked)
    };
    for (rebuild_needed, through) in [
        (false, Some(TODAY)),
        (true, None),
        (true, Some(TODAY)),
        (false, Some(date(2026, 1, 1))),
    ] {
        assert_eq!(
            inverted(rebuild_needed, through),
            Err(NetWorthError::RangeInverted)
        );
    }
    let too_many = |through| {
        let mut asked = input(&held, Some("USD"), &no_rates());
        asked.from = date(2020, 1, 1);
        asked.to = TODAY;
        asked.through = through;
        net_worth_report(&asked)
    };
    for through in [Some(TODAY), Some(date(2020, 1, 5)), None] {
        assert_eq!(too_many(through), Err(NetWorthError::TooManyPoints));
    }
}

#[test]
fn a_start_after_today_is_refused() {
    let mut asked = input(&[], Some("USD"), &no_rates());
    asked.from = date(2026, 11, 3);
    asked.to = date(2026, 11, 4);
    assert_eq!(net_worth_report(&asked), Err(NetWorthError::RangeInverted));
}

#[test]
fn totals_past_i64_are_out_of_range() {
    let held = [
        cash(1, "USD", &[(OCT_26, i64::MAX)]),
        cash(2, "USD", &[(OCT_26, 1)]),
    ];
    assert_eq!(
        net_worth_report(&input(&held, Some("USD"), &no_rates())),
        Err(NetWorthError::OutOfRange)
    );
    assert_eq!(
        net_worth_report(&input(&held, None, &no_rates())),
        Err(NetWorthError::OutOfRange),
        "a sum per currency, with no total to blame"
    );
}

#[test]
fn a_class_past_i64_is_out_of_range_though_the_total_fits() {
    let held = [
        cash(1, "USD", &[(OCT_26, i64::MAX)]),
        cash(2, "USD", &[(OCT_26, 1)]),
        holding(3, "USD", LiquidityClass::LongTermLiability, &[(OCT_26, -5)]),
    ];
    assert_eq!(
        net_worth_report(&input(&held, Some("USD"), &no_rates())),
        Err(NetWorthError::OutOfRange)
    );
}

/// Accounts add up in `i128`, so their order can't refuse an answer that fits.
#[test]
fn a_sum_passing_i64_on_the_way_is_fine() {
    let held = [
        cash(1, "USD", &[(OCT_26, i64::MAX)]),
        cash(2, "USD", &[(OCT_26, i64::MAX)]),
        cash(3, "USD", &[(OCT_26, -i64::MAX)]),
    ];
    let report = net_worth_report(&input(&held, Some("USD"), &no_rates())).unwrap();
    assert_eq!(report.points[0].total, Some(money(i64::MAX, "USD")));
    assert_eq!(report.points[0].by_currency, [money(i64::MAX, "USD")]);
}

#[test]
fn account_points_false_keeps_the_summaries() {
    let held = [cash(1, "USD", &[(OCT_26, 100)])];
    let mut asked = input(&held, Some("USD"), &no_rates());
    let with_points = net_worth_report(&asked).unwrap();
    asked.account_points = false;
    let without = net_worth_report(&asked).unwrap();
    assert_eq!(without.points, with_points.points);
    assert_eq!(without.status, with_points.status);
    assert_eq!(without.accounts[0].points, []);
    assert_eq!(without.accounts[0].first_on, Some(OCT_26));
    assert_eq!(without.accounts[0].name, "Holding 1");
    assert_eq!(without.accounts[0].currency, currency("USD"));
    assert_eq!(without.accounts[0].liquidity, LiquidityClass::Cash);
    assert!(without.accounts[0].counted);
}

/// N2: it answers from the accounts it is given, whatever else it is handed.
#[test]
fn what_an_unlisted_account_brings_changes_nothing() {
    let book = rates(&[]);
    let held = [cash(1, "USD", &[(OCT_26, 100)])];
    let plain = input(&held, Some("USD"), &book);
    let mut handed_more = plain.clone();
    let foreign = id(9);
    handed_more.rows.push(DailyValue {
        account: foreign,
        on: OCT_26,
        balance_minor: 77,
        balance_on: OCT_26,
        balance_source: BalanceSource::Sync,
        display: Some(currency("USD")),
        display_minor: None,
        rate_on: None,
        conversion: ConversionState::Missing,
    });
    handed_more.first_on.push((foreign, OCT_26));
    assert_eq!(net_worth_report(&handed_more), net_worth_report(&plain));
}

/// The store refuses such a row; a sum must not add other units even so.
#[test]
fn a_row_in_another_display_currency_is_no_amount() {
    let held = [cash(1, "USD", &[(OCT_26, 100)])];
    let mut asked = input(&held, Some("USD"), &no_rates());
    for row in &mut asked.rows {
        row.display = Some(currency("EUR"));
    }
    let report = net_worth_report(&asked).unwrap();
    assert_eq!(report.points[0].total, None);
    assert_eq!(report.points[0].unconverted, [id(1)]);
    assert_eq!(report.status, NetWorthStatus::Provisional);
}

#[test]
fn the_answer_serializes_as_documented() {
    let held = [cash(1, "USD", &[(OCT_26, 100)])];
    let mut asked = input(&held, None, &no_rates());
    asked.to = OCT_26;
    let report = net_worth_report(&asked).unwrap();
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["display_currency"], serde_json::Value::Null);
    assert_eq!(json["status"], "unavailable");
    assert_eq!(json["interval"], "day");
    assert_eq!(json["blockers"][0]["kind"], "display_currency_not_set");
    assert_eq!(json["points"][0]["on"], "2026-10-26");
    assert_eq!(json["points"][0]["total"], serde_json::Value::Null);
    assert_eq!(json["points"][0]["by_currency"][0]["minor_units"], 100);
    assert_eq!(json["points"][0]["by_liquidity"][0]["liquidity"], "cash");
    assert_eq!(
        json["accounts"][0]["points"][0]["conversion"],
        "no_display_currency"
    );
    assert_eq!(json["accounts"][0]["points"][0]["balance_source"], "sync");

    let mut excluded = cash(2, "USD", &[(OCT_26, 100)]);
    excluded.line.include_in_net_worth = false;
    let report = net_worth_report(&input(&[excluded], Some("USD"), &no_rates())).unwrap();
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["accounts"][0]["not_counted"], "excluded_by_flag");
}

#[cfg(feature = "openapi")]
#[test]
fn its_reason_has_a_schema_name_of_its_own() {
    use utoipa::ToSchema;
    assert_eq!(
        crate::net_worth::NotCountedReason::name(),
        "NetWorthNotCountedReason",
        "the forecast has a NotCountedReason of its own"
    );
}
