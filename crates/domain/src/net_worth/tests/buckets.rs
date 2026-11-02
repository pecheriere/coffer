//! Money that isn't the household's (spec §8.11 B6): the same calculation over each
//! bucket's accounts, and nothing of it beside the household's answer.
//!
//! The golden week's household is `golden.rs`'s, untouched. Beside it, with the same
//! rates:
//!
//! | Day   | College fund | UTMA (EUR 1,000.00) | Child A   | Giving  |
//! |-------|--------------|---------------------|-----------|---------|
//! | 10-26 | 2,000,000    | 111,880             | 2,111,880 |         |
//! | 10-27 | 2,000,000    | 111,500             | 2,111,500 |         |
//! | 10-28 | 2,000,000    | 111,330             | 2,111,330 | 500,000 |
//! | 10-29 | 2,000,000    | 111,300             | 2,111,300 | 500,000 |
//! | 10-30 | 2,000,000    | 111,720             | 2,111,720 | 500,000 |
//! | 10-31 | 2,000,000    | 111,720             | 2,111,720 | 500,000 |
//! | 11-01 | 2,000,000    | 111,720             | 2,111,720 | 500,000 |

use jiff::civil::{Date, date};

use super::golden::{CARD, CHECKING, SIDE, book, holdings};
use super::{Holding, cash, holding, id, input, money};
use crate::accounts::{BelongsTo, LiquidityClass};
use crate::ids::AccountId;
use crate::net_worth::{
    AccountSeries, ConversionState, LiquidityLine, NetWorthBlocker, NetWorthBucket,
    NetWorthCaution, NetWorthError, NetWorthInput, NetWorthPoint, NetWorthReport, NetWorthStatus,
    NotCountedReason, RateMissingReason, net_worth_report,
};

const COLLEGE: u128 = 11;
const UTMA: u128 = 12;
const GIVING: u128 = 13;
const OTHER: u128 = 14;

const fn oct(day: i8) -> Date {
    date(2026, 10, day)
}

const NOV_1: Date = date(2026, 11, 1);

/// `held` as an account that isn't the household's, whose flags are off as the table holds.
fn held_for(mut held: Holding, belongs_to: BelongsTo, label: Option<&str>) -> Holding {
    held.line.belongs_to = belongs_to;
    held.line.beneficiary = label.map(str::to_owned);
    held.line.include_in_net_worth = false;
    held
}

fn for_child(held: Holding) -> Holding {
    held_for(held, BelongsTo::SomeoneElse, Some("Child A"))
}

fn given(held: Holding) -> Holding {
    held_for(held, BelongsTo::GivenAway, None)
}

fn college() -> Holding {
    for_child(holding(
        COLLEGE,
        "USD",
        LiquidityClass::Invested,
        &[(oct(26), 2_000_000)],
    ))
}

fn utma() -> Holding {
    for_child(holding(
        UTMA,
        "EUR",
        LiquidityClass::Invested,
        &[(oct(26), 100_000)],
    ))
}

fn giving() -> Holding {
    given(holding(
        GIVING,
        "USD",
        LiquidityClass::Invested,
        &[(oct(28), 500_000)],
    ))
}

/// The golden household with `more` beside it.
fn golden_with(more: Vec<Holding>) -> NetWorthInput {
    let mut all = more;
    all.extend(holdings());
    input(&all, Some("USD"), &book())
}

/// The golden household's answer, with nothing else in the input.
fn golden_alone() -> NetWorthReport {
    net_worth_report(&input(&holdings(), Some("USD"), &book())).unwrap()
}

/// `report`'s top level alone, to compare with a report that really lacks the buckets.
fn without_buckets(mut report: NetWorthReport) -> NetWorthReport {
    report.buckets.clear();
    report
}

fn totals(points: &[NetWorthPoint]) -> Vec<(Date, Option<i64>)> {
    points
        .iter()
        .map(|point| (point.on, point.total.map(|total| total.minor_units)))
        .collect()
}

/// Who `bucket` lists, in order.
fn listed(bucket: &NetWorthBucket) -> Vec<AccountId> {
    bucket.accounts.iter().map(|line| line.account).collect()
}

/// Who `report` lists at the top level, in order.
fn listed_at_the_top(report: &NetWorthReport) -> Vec<AccountId> {
    report.accounts.iter().map(|line| line.account).collect()
}

/// Each bucket's key, in order.
fn keys(report: &NetWorthReport) -> Vec<(BelongsTo, Option<&str>)> {
    report
        .buckets
        .iter()
        .map(|bucket| (bucket.belongs_to, bucket.beneficiary.as_deref()))
        .collect()
}

/// The one bucket of `report`.
fn only_bucket(report: &NetWorthReport) -> &NetWorthBucket {
    let [only] = report.buckets.as_slice() else {
        panic!("expected one bucket");
    };
    only
}

fn series_in(bucket: &NetWorthBucket, n: u128) -> &AccountSeries {
    bucket
        .accounts
        .iter()
        .find(|line| line.account == id(n))
        .expect("a series")
}

/// `asked` with the row of `n` on `on` gone.
fn with_hole(mut asked: NetWorthInput, n: u128, on: Date) -> NetWorthInput {
    asked
        .rows
        .retain(|row| !(row.account == id(n) && row.on == on));
    asked
}

#[test]
fn golden_week_with_buckets() {
    let report = net_worth_report(&golden_with(vec![giving(), college(), utma()])).unwrap();

    assert_eq!(
        totals(&report.points),
        [
            (oct(26), Some(611_880)),
            (oct(27), Some(611_500)),
            (oct(28), Some(581_330)),
            (oct(29), Some(581_300)),
            (oct(30), Some(631_720)),
            (oct(31), Some(631_720)),
            (NOV_1, Some(631_720)),
        ]
    );
    assert_eq!(report.status, NetWorthStatus::Firm);
    assert_eq!(
        keys(&report),
        [
            (BelongsTo::SomeoneElse, Some("Child A")),
            (BelongsTo::GivenAway, None),
        ]
    );

    let child = &report.buckets[0];
    assert_eq!(child.status, NetWorthStatus::Firm);
    assert_eq!(
        totals(&child.points),
        [
            (oct(26), Some(2_111_880)),
            (oct(27), Some(2_111_500)),
            (oct(28), Some(2_111_330)),
            (oct(29), Some(2_111_300)),
            (oct(30), Some(2_111_720)),
            (oct(31), Some(2_111_720)),
            (NOV_1, Some(2_111_720)),
        ]
    );
    for point in &child.points {
        assert_eq!(
            point.by_currency,
            [money(100_000, "EUR"), money(2_000_000, "USD")]
        );
    }
    let saturday = series_in(child, UTMA)
        .points
        .iter()
        .find(|point| point.on == oct(31))
        .expect("a point");
    assert_eq!(saturday.rate_on, Some(oct(30)));
    assert_eq!(saturday.conversion, ConversionState::Converted);
    assert_eq!(saturday.amount, Some(money(111_720, "USD")));
    assert_eq!(listed(child), [id(COLLEGE), id(UTMA)]);

    let gift = &report.buckets[1];
    assert_eq!(gift.status, NetWorthStatus::Firm);
    assert_eq!(
        totals(&gift.points),
        [
            (oct(28), Some(500_000)),
            (oct(29), Some(500_000)),
            (oct(30), Some(500_000)),
            (oct(31), Some(500_000)),
            (NOV_1, Some(500_000)),
        ]
    );
}

#[test]
fn the_top_level_is_what_it_would_be_without_them() {
    let report = net_worth_report(&golden_with(vec![giving(), college(), utma()])).unwrap();

    assert_eq!(without_buckets(report), golden_alone());
}

/// A bucket's account is listed there as included, with no reason to give: true there says
/// nothing of net worth, whose sums it is in none of.
#[test]
fn a_bucket_s_series_is_counted_in_its_bucket() {
    let report = net_worth_report(&golden_with(vec![college()])).unwrap();

    let line = series_in(only_bucket(&report), COLLEGE);
    assert!(line.counted);
    assert_eq!(line.not_counted, None);
    assert_eq!(line.first_on, Some(oct(26)));
}

#[test]
fn a_bucket_s_missing_rate_leaves_the_household_firm() {
    let baht = for_child(holding(
        OTHER,
        "THB",
        LiquidityClass::Invested,
        &[(oct(26), 50_000)],
    ));

    let report = net_worth_report(&golden_with(vec![baht])).unwrap();

    let bucket = only_bucket(&report);
    assert_eq!(bucket.status, NetWorthStatus::Provisional);
    assert_eq!(
        bucket.cautions,
        [NetWorthCaution::RateMissing {
            currency: super::currency("THB"),
            reason: RateMissingReason::NoneThatEarly,
            first_on: oct(26),
            last_on: NOV_1,
        }]
    );
    assert!(bucket.points.iter().all(|point| point.total.is_none()));
    assert_eq!(bucket.points[0].by_currency, [money(50_000, "THB")]);
    assert_eq!(bucket.points[0].unconverted, [id(OTHER)]);
    assert_eq!(without_buckets(report), golden_alone());
}

#[test]
fn a_household_s_missing_rate_leaves_the_bucket_firm() {
    let baht = cash(OTHER, "THB", &[(oct(26), 50_000)]);
    let alone = net_worth_report(&golden_with(vec![college()])).unwrap();

    let report = net_worth_report(&golden_with(vec![college(), baht])).unwrap();

    assert_eq!(report.status, NetWorthStatus::Provisional);
    assert!(report.points.iter().all(|point| point.total.is_none()));
    assert_eq!(report.buckets, alone.buckets);
    assert_eq!(report.buckets[0].status, NetWorthStatus::Firm);
}

#[test]
fn a_hole_in_a_bucket_leaves_the_household_firm() {
    let asked = with_hole(golden_with(vec![college()]), COLLEGE, oct(29));

    let report = net_worth_report(&asked).unwrap();

    let bucket = only_bucket(&report);
    assert_eq!(bucket.status, NetWorthStatus::Provisional);
    assert_eq!(
        bucket.cautions,
        [NetWorthCaution::DailyValueMissing {
            account: id(COLLEGE),
            first_on: oct(29),
            last_on: oct(29),
        }]
    );
    assert_eq!(totals(&bucket.points)[3], (oct(29), None));
    assert_eq!(bucket.points[3].unconverted, [id(COLLEGE)]);
    assert_eq!(without_buckets(report), golden_alone());
}

#[test]
fn a_hole_in_the_household_leaves_the_bucket_firm() {
    let alone = net_worth_report(&golden_with(vec![college()])).unwrap();
    let asked = with_hole(golden_with(vec![college()]), CHECKING, oct(29));

    let report = net_worth_report(&asked).unwrap();

    assert_eq!(report.status, NetWorthStatus::Provisional);
    assert_eq!(totals(&report.points)[3], (oct(29), None));
    assert_eq!(report.buckets, alone.buckets);
    assert_eq!(report.buckets[0].status, NetWorthStatus::Firm);
}

#[test]
fn a_bucket_s_old_balance_is_its_own() {
    let old = for_child(holding(
        OTHER,
        "USD",
        LiquidityClass::Invested,
        &[(oct(1), 70_000)],
    ));

    let report = net_worth_report(&golden_with(vec![old])).unwrap();

    let bucket = only_bucket(&report);
    assert_eq!(bucket.status, NetWorthStatus::Provisional);
    assert_eq!(
        bucket.cautions,
        [NetWorthCaution::BalanceStale {
            account: id(OTHER),
            on: oct(1),
            age_days: 31,
        }]
    );
    assert_eq!(without_buckets(report), golden_alone());
}

#[test]
fn a_bucket_s_positive_liability_is_its_own() {
    let loan = for_child(holding(
        OTHER,
        "USD",
        LiquidityClass::LongTermLiability,
        &[(oct(26), 1_000)],
    ));

    let report = net_worth_report(&golden_with(vec![loan])).unwrap();

    let bucket = only_bucket(&report);
    assert_eq!(bucket.status, NetWorthStatus::Provisional);
    assert_eq!(
        bucket.cautions,
        [NetWorthCaution::LiabilityBalancePositive {
            account: id(OTHER),
            balance: money(1_000, "USD"),
        }]
    );
    assert_eq!(without_buckets(report), golden_alone());
}

/// Closed on 10-29: with money, it counts on the 26th to the 28th; owing, it goes on.
#[test]
fn a_closed_bucket_account_follows_n9() {
    let mut closed = for_child(holding(
        OTHER,
        "USD",
        LiquidityClass::Invested,
        &[(oct(26), 100_000)],
    ));
    closed.line.closed_on = Some(oct(29));
    let mut owing = given(holding(
        GIVING,
        "USD",
        LiquidityClass::Invested,
        &[(oct(26), -5_000)],
    ));
    owing.line.closed_on = Some(oct(29));

    let report = net_worth_report(&golden_with(vec![closed, owing])).unwrap();

    let child = &report.buckets[0];
    assert_eq!(
        totals(&child.points),
        [
            (oct(26), Some(100_000)),
            (oct(27), Some(100_000)),
            (oct(28), Some(100_000)),
        ]
    );
    assert_eq!(
        child.cautions,
        [NetWorthCaution::NothingCountedAfterLastPoint {
            from: oct(29),
            to: NOV_1,
        }]
    );
    let gift = &report.buckets[1];
    assert_eq!(gift.points.len(), 7);
    assert_eq!(gift.points[6].total, Some(money(-5_000, "USD")));
    assert_eq!(
        gift.cautions,
        [NetWorthCaution::ClosedAccountOwing {
            account: id(GIVING),
            balance: money(-5_000, "USD"),
        }]
    );
    assert_eq!(without_buckets(report), golden_alone());
}

#[test]
fn a_household_with_only_buckets_is_blocked_but_they_answer() {
    let report = net_worth_report(&input(&[college()], Some("USD"), &book())).unwrap();

    assert_eq!(report.status, NetWorthStatus::Unavailable);
    assert_eq!(report.blockers, [NetWorthBlocker::NoCountedAccounts]);
    assert_eq!(report.points, []);
    assert_eq!(listed_at_the_top(&report), []);
    let bucket = only_bucket(&report);
    assert_eq!(bucket.status, NetWorthStatus::Firm);
    assert_eq!(bucket.points.len(), 7);
    assert_eq!(bucket.points[6].total, Some(money(2_000_000, "USD")));
}

/// A range wholly after `through` has no period to find empty: only who is in the section
/// says the household has nothing, whatever its buckets hold.
#[test]
fn a_range_past_the_fill_leaves_a_household_of_buckets_blocked() {
    let mut asked = input(&[college()], Some("USD"), &book());
    asked.through = Some(oct(20));

    let report = net_worth_report(&asked).unwrap();

    assert_eq!(report.blockers, [NetWorthBlocker::NoCountedAccounts]);
    assert_eq!(only_bucket(&report).blockers, []);
    assert_eq!(
        only_bucket(&report).cautions,
        [NetWorthCaution::ToDateNotYetFilled { through: oct(20) }]
    );
}

#[test]
fn a_bucket_with_nothing_in_the_range_is_blocked_alone() {
    let late = for_child(holding(
        OTHER,
        "USD",
        LiquidityClass::Invested,
        &[(super::TODAY, 70_000)],
    ));

    let report = net_worth_report(&golden_with(vec![late])).unwrap();

    let bucket = only_bucket(&report);
    assert_eq!(bucket.status, NetWorthStatus::Unavailable);
    assert_eq!(bucket.blockers, [NetWorthBlocker::NoCountedAccounts]);
    assert_eq!(bucket.points, []);
    assert_eq!(listed(bucket), [id(OTHER)]);
    assert_eq!(without_buckets(report), golden_alone());
}

#[test]
fn display_currency_not_set_keeps_their_sums() {
    let report = net_worth_report(&input(&[college(), utma()], None, &book())).unwrap();

    let bucket = only_bucket(&report);
    assert_eq!(bucket.status, NetWorthStatus::Unavailable);
    assert_eq!(bucket.blockers, [NetWorthBlocker::DisplayCurrencyNotSet]);
    assert_eq!(bucket.points[0].total, None);
    assert_eq!(
        bucket.points[0].by_currency,
        [money(100_000, "EUR"), money(2_000_000, "USD")]
    );
    assert_eq!(bucket.points[0].unconverted, []);
}

#[test]
fn rebuild_needed_answers_no_bucket() {
    let mut asked = golden_with(vec![college(), giving()]);
    assert_eq!(net_worth_report(&asked).unwrap().buckets.len(), 2);
    asked.rebuild_needed = true;

    let report = net_worth_report(&asked).unwrap();

    assert_eq!(report.buckets, []);
    assert_eq!(report.points, []);
    assert_eq!(listed_at_the_top(&report), []);
    assert_eq!(report.blockers, [NetWorthBlocker::Rebuilding]);
    assert_eq!(report.status, NetWorthStatus::Unavailable);
}

#[test]
fn nothing_filled_answers_no_bucket() {
    let mut asked = golden_with(vec![college()]);
    assert_eq!(net_worth_report(&asked).unwrap().buckets.len(), 1);
    asked.through = None;

    let report = net_worth_report(&asked).unwrap();

    assert_eq!(report.buckets, []);
    assert_eq!(report.blockers, [NetWorthBlocker::Rebuilding]);
}

#[test]
fn two_spellings_are_two_buckets() {
    let first = held_for(
        cash(COLLEGE, "USD", &[(oct(26), 100)]),
        BelongsTo::SomeoneElse,
        Some("child a"),
    );
    let second = held_for(
        cash(UTMA, "USD", &[(oct(26), 200)]),
        BelongsTo::SomeoneElse,
        Some("Child A"),
    );

    let report = net_worth_report(&input(&[first, second], Some("USD"), &book())).unwrap();

    assert_eq!(
        keys(&report),
        [
            (BelongsTo::SomeoneElse, Some("Child A")),
            (BelongsTo::SomeoneElse, Some("child a")),
        ]
    );
    assert_eq!(report.buckets[0].points[0].total, Some(money(200, "USD")));
    assert_eq!(report.buckets[1].points[0].total, Some(money(100, "USD")));
}

/// Same label, other `belongs_to`: two buckets as well.
#[test]
fn a_label_is_only_one_part_of_a_bucket_s_key() {
    let first = held_for(
        cash(COLLEGE, "USD", &[(oct(26), 100)]),
        BelongsTo::GivenAway,
        Some("Family fund"),
    );
    let second = held_for(
        cash(UTMA, "USD", &[(oct(26), 200)]),
        BelongsTo::SomeoneElse,
        Some("Family fund"),
    );

    let report = net_worth_report(&input(&[first, second], Some("USD"), &book())).unwrap();

    assert_eq!(
        keys(&report),
        [
            (BelongsTo::SomeoneElse, Some("Family fund")),
            (BelongsTo::GivenAway, Some("Family fund")),
        ]
    );
}

/// By bytes, so an upper case letter comes before every lower case one.
#[test]
fn no_label_comes_first() {
    let bucket = |n: u128, label: Option<&str>| {
        held_for(
            cash(n, "USD", &[(oct(26), 100)]),
            BelongsTo::SomeoneElse,
            label,
        )
    };

    let report = net_worth_report(&input(
        &[
            bucket(21, Some("apple")),
            bucket(22, Some("Zed")),
            bucket(23, None),
            bucket(24, Some("Child A")),
        ],
        Some("USD"),
        &book(),
    ))
    .unwrap();

    assert_eq!(
        keys(&report),
        [
            (BelongsTo::SomeoneElse, None),
            (BelongsTo::SomeoneElse, Some("Child A")),
            (BelongsTo::SomeoneElse, Some("Zed")),
            (BelongsTo::SomeoneElse, Some("apple")),
        ]
    );
}

/// `given_away` with no label still follows `someone_else` with the last one.
#[test]
fn someone_else_comes_before_given_away() {
    let gift = given(cash(21, "USD", &[(oct(26), 100)]));
    let held = held_for(
        cash(22, "USD", &[(oct(26), 100)]),
        BelongsTo::SomeoneElse,
        Some("Zed"),
    );

    let report = net_worth_report(&input(&[gift, held], Some("USD"), &book())).unwrap();

    assert_eq!(
        keys(&report),
        [
            (BelongsTo::SomeoneElse, Some("Zed")),
            (BelongsTo::GivenAway, None),
        ]
    );
}

#[test]
fn the_order_is_fixed() {
    let all = || {
        vec![
            given(cash(21, "USD", &[(oct(26), 100)])),
            held_for(
                cash(22, "USD", &[(oct(26), 200)]),
                BelongsTo::SomeoneElse,
                Some("Child B"),
            ),
            held_for(
                cash(23, "USD", &[(oct(26), 300)]),
                BelongsTo::SomeoneElse,
                None,
            ),
            held_for(
                cash(24, "USD", &[(oct(26), 400)]),
                BelongsTo::SomeoneElse,
                Some("Child A"),
            ),
        ]
    };
    let forward = net_worth_report(&input(&all(), Some("USD"), &book())).unwrap();
    let mut shuffled = all();
    shuffled.swap(0, 2);
    shuffled.swap(1, 3);
    let mut asked = input(&shuffled, Some("USD"), &book());
    asked.rows.reverse();
    asked.first_on.reverse();

    assert_eq!(net_worth_report(&asked).unwrap(), forward);
    assert_eq!(
        keys(&forward),
        [
            (BelongsTo::SomeoneElse, None),
            (BelongsTo::SomeoneElse, Some("Child A")),
            (BelongsTo::SomeoneElse, Some("Child B")),
            (BelongsTo::GivenAway, None),
        ]
    );
}

#[test]
fn an_excluded_household_account_stays_at_the_top() {
    let report = net_worth_report(&golden_with(vec![college()])).unwrap();

    let side = report
        .accounts
        .iter()
        .find(|line| line.account == id(SIDE))
        .expect("a series");
    assert!(!side.counted);
    assert_eq!(side.not_counted, Some(NotCountedReason::ExcludedByFlag));
    assert_eq!(listed(only_bucket(&report)), [id(COLLEGE)]);
}

/// The same rows, read another way: the card leaves every past household point (see
/// `golden.rs`, whose total it was -30,000 of from 10-28) and is in every past bucket point.
#[test]
fn moving_an_account_changes_its_whole_history() {
    let mut asked = golden_with(Vec::new());
    let card = asked
        .accounts
        .iter_mut()
        .find(|line| line.id == id(CARD))
        .expect("a line");
    card.belongs_to = BelongsTo::GivenAway;
    card.include_in_net_worth = false;

    let report = net_worth_report(&asked).unwrap();

    assert_eq!(
        totals(&report.points),
        [
            (oct(26), Some(611_880)),
            (oct(27), Some(611_500)),
            (oct(28), Some(611_330)),
            (oct(29), Some(611_300)),
            (oct(30), Some(661_720)),
            (oct(31), Some(661_720)),
            (NOV_1, Some(661_720)),
        ]
    );
    assert_eq!(
        totals(&only_bucket(&report).points),
        [
            (oct(28), Some(-30_000)),
            (oct(29), Some(-30_000)),
            (oct(30), Some(-30_000)),
            (oct(31), Some(-30_000)),
            (NOV_1, Some(-30_000)),
        ]
    );
    assert_eq!(
        only_bucket(&report).points[0].by_liquidity,
        [LiquidityLine {
            liquidity: LiquidityClass::ShortTermLiability,
            amount: Some(money(-30_000, "USD")),
        }]
    );
}

#[test]
fn account_points_false_keeps_bucket_summaries() {
    let with_points = net_worth_report(&golden_with(vec![college()])).unwrap();
    let mut asked = golden_with(vec![college()]);
    asked.account_points = false;

    let report = net_worth_report(&asked).unwrap();

    let bucket = only_bucket(&report);
    assert_eq!(series_in(bucket, COLLEGE).points, []);
    assert_eq!(bucket.points, with_points.buckets[0].points);
    assert_eq!(bucket.status, NetWorthStatus::Firm);
}

/// A report is refused whole when a sum in it doesn't fit, a bucket's as a household's:
/// there is no word yet for one part being refused.
#[test]
fn a_bucket_s_sum_too_large_refuses_the_report() {
    let huge = |n: u128| {
        for_child(holding(
            n,
            "USD",
            LiquidityClass::Invested,
            &[(oct(26), i64::MAX)],
        ))
    };

    let refused = net_worth_report(&golden_with(vec![huge(21), huge(22)]));

    assert_eq!(refused, Err(NetWorthError::OutOfRange));
}

/// A label may be a child's name, and a `Debug` can reach a log line (B5).
#[test]
fn a_label_is_in_no_debug_text() {
    let asked = golden_with(vec![college()]);
    let report = net_worth_report(&asked).unwrap();
    assert_eq!(keys(&report)[0].1, Some("Child A"));

    assert!(!format!("{asked:?}").contains("Child A"));
    assert!(!format!("{report:?}").contains("Child A"));
}
