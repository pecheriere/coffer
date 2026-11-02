//! What keeps a number but stops it being firm.

use jiff::civil::{Date, date};

use super::{TODAY, cash, currency, holding, id, input, money, point_on, rates};
use crate::accounts::LiquidityClass;
use crate::fx::RateBook;
use crate::money::Currency;
use crate::net_worth::{
    CurrencyState, NetWorthCaution, NetWorthInterval, NetWorthStatus, RateMissingReason,
    net_worth_report,
};

const OCT_26: Date = date(2026, 10, 26);
const NOV_1: Date = date(2026, 11, 1);

fn usd_book() -> RateBook {
    rates(&[("USD", &[(date(2026, 10, 25), "1.1172")])])
}

#[test]
fn missing_rate_nulls_the_total_and_names_the_account() {
    let held = [
        cash(1, "USD", &[(OCT_26, 500)]),
        cash(2, "THB", &[(OCT_26, 900)]),
        holding(
            3,
            "USD",
            LiquidityClass::ShortTermLiability,
            &[(OCT_26, -30)],
        ),
    ];
    let mut asked = input(&held, Some("USD"), &usd_book());
    let thb = currency("THB");
    asked.coverage = vec![CurrencyState {
        currency: thb,
        reason: Some(RateMissingReason::NotFetchedYet),
    }];
    let report = net_worth_report(&asked).unwrap();

    assert_eq!(report.status, NetWorthStatus::Provisional);
    let point = point_on(&report, OCT_26);
    assert_eq!(point.total, None);
    assert_eq!(point.unconverted, [id(2)]);
    assert_eq!(point.by_liquidity[0].liquidity, LiquidityClass::Cash);
    assert_eq!(point.by_liquidity[0].amount, None);
    assert_eq!(
        point.by_liquidity[1].liquidity,
        LiquidityClass::ShortTermLiability
    );
    assert_eq!(point.by_liquidity[1].amount, Some(money(-30, "USD")));
    assert_eq!(point.by_currency, [money(900, "THB"), money(470, "USD")]);
    assert_eq!(
        report.cautions,
        [NetWorthCaution::RateMissing {
            currency: thb,
            reason: RateMissingReason::NotFetchedYet,
            first_on: OCT_26,
            last_on: NOV_1,
        }]
    );
}

#[test]
fn each_missing_reason_is_reported() {
    let thb = currency("THB");
    for reason in [
        RateMissingReason::NotFetchedYet,
        RateMissingReason::NotPublished,
        RateMissingReason::FetchFailing,
        RateMissingReason::ProviderOff,
    ] {
        let held = [cash(1, "THB", &[(OCT_26, 900)])];
        let mut asked = input(&held, Some("USD"), &usd_book());
        asked.coverage = vec![CurrencyState {
            currency: thb,
            reason: Some(reason),
        }];
        let report = net_worth_report(&asked).unwrap();
        assert_eq!(
            report.cautions,
            [NetWorthCaution::RateMissing {
                currency: thb,
                reason,
                first_on: OCT_26,
                last_on: NOV_1,
            }]
        );
    }
}

#[test]
fn no_reason_means_none_that_early() {
    let first_rate = date(2026, 10, 28);
    let book = rates(&[("THB", &[(first_rate, "37.5")])]);
    let held = [cash(1, "THB", &[(OCT_26, 900)])];
    let mut asked = input(&held, Some("EUR"), &book);
    let thb = currency("THB");
    asked.coverage = vec![CurrencyState {
        currency: thb,
        reason: None,
    }];
    let report = net_worth_report(&asked).unwrap();
    assert_eq!(
        report.cautions,
        [NetWorthCaution::RateMissing {
            currency: thb,
            reason: RateMissingReason::NoneThatEarly,
            first_on: OCT_26,
            last_on: date(2026, 10, 27),
        }]
    );
    assert_eq!(point_on(&report, OCT_26).total, None);
    assert_eq!(
        point_on(&report, first_rate).total,
        Some(money(24, "EUR")),
        "THB 9.00 / 37.5 = EUR 0.24"
    );
}

#[test]
fn a_display_currency_with_no_rates_is_the_one_named() {
    let held = [
        cash(1, "EUR", &[(OCT_26, 100)]),
        cash(2, "USD", &[(OCT_26, 200)]),
    ];
    let mut asked = input(&held, Some("THB"), &usd_book());
    let thb = currency("THB");
    asked.coverage = vec![
        CurrencyState {
            currency: thb,
            reason: Some(RateMissingReason::NotPublished),
        },
        CurrencyState {
            currency: currency("USD"),
            reason: None,
        },
    ];
    let report = net_worth_report(&asked).unwrap();
    assert_eq!(
        report.cautions,
        [NetWorthCaution::RateMissing {
            currency: thb,
            reason: RateMissingReason::NotPublished,
            first_on: OCT_26,
            last_on: NOV_1,
        }]
    );
    assert_eq!(point_on(&report, OCT_26).unconverted, [id(1), id(2)]);
}

#[test]
fn both_legs_with_a_reason_are_both_named() {
    let held = [cash(1, "GBP", &[(OCT_26, 100)])];
    let mut asked = input(&held, Some("THB"), &RateBook::default());
    let (gbp, thb) = (currency("GBP"), currency("THB"));
    asked.coverage = [gbp, thb]
        .map(|one| CurrencyState {
            currency: one,
            reason: Some(RateMissingReason::FetchFailing),
        })
        .to_vec();
    let report = net_worth_report(&asked).unwrap();
    let named: Vec<Currency> = report
        .cautions
        .iter()
        .filter_map(|caution| match caution {
            NetWorthCaution::RateMissing { currency, .. } => Some(*currency),
            _ => None,
        })
        .collect();
    assert_eq!(named, [gbp, thb]);
}

/// The euro has no rate to miss, whatever a caller's coverage says of it.
#[test]
fn the_euro_is_never_the_one_named() {
    let held = [cash(1, "EUR", &[(OCT_26, 100)])];
    let mut asked = input(&held, Some("THB"), &RateBook::default());
    let thb = currency("THB");
    asked.coverage = vec![
        CurrencyState {
            currency: currency("EUR"),
            reason: Some(RateMissingReason::NotFetchedYet),
        },
        CurrencyState {
            currency: thb,
            reason: Some(RateMissingReason::NotPublished),
        },
    ];
    assert_eq!(
        net_worth_report(&asked).unwrap().cautions,
        [NetWorthCaution::RateMissing {
            currency: thb,
            reason: RateMissingReason::NotPublished,
            first_on: OCT_26,
            last_on: NOV_1,
        }]
    );
}

/// A row keeps one rate date, not which leg it came from, so between two foreign
/// currencies the caution names the account's.
#[test]
fn between_two_foreign_currencies_the_account_s_is_named() {
    let rate_on = date(2026, 10, 1);
    let book = rates(&[("USD", &[(rate_on, "1.1")]), ("GBP", &[(rate_on, "0.8")])]);
    let held = [cash(1, "USD", &[(OCT_26, 100)])];
    let mut asked = input(&held, Some("GBP"), &book);
    asked.stale_after_days = u16::MAX;
    assert_eq!(
        net_worth_report(&asked).unwrap().cautions,
        [NetWorthCaution::RateStale {
            currency: currency("USD"),
            on: NOV_1,
            rate_on,
            age_days: 31,
        }]
    );
}

/// N2: a currency nothing visible needs isn't said, even if coverage holds it.
#[test]
fn coverage_of_a_currency_nothing_uses_is_not_said() {
    let held = [cash(1, "USD", &[(OCT_26, 100)])];
    let mut asked = input(&held, Some("USD"), &usd_book());
    asked.coverage = vec![CurrencyState {
        currency: currency("JPY"),
        reason: Some(RateMissingReason::FetchFailing),
    }];
    let report = net_worth_report(&asked).unwrap();
    assert_eq!(report.cautions, []);
    assert_eq!(report.status, NetWorthStatus::Firm);
}

fn stale_cautions(rate_on: Date, on: Date) -> Vec<NetWorthCaution> {
    let book = rates(&[("RUB", &[(rate_on, "90")])]);
    let held = [cash(1, "RUB", &[(rate_on, 9000)])];
    let mut asked = input(&held, Some("EUR"), &book);
    asked.from = on;
    asked.to = on;
    asked.stale_after_days = u16::MAX;
    net_worth_report(&asked).unwrap().cautions
}

#[test]
fn an_old_rate_is_stale() {
    assert_eq!(
        stale_cautions(date(2026, 10, 26), TODAY),
        [],
        "7 days is fine"
    );
    let rub = currency("RUB");
    assert_eq!(
        stale_cautions(date(2026, 10, 25), TODAY),
        [NetWorthCaution::RateStale {
            currency: rub,
            on: TODAY,
            rate_on: date(2026, 10, 25),
            age_days: 8,
        }]
    );
    // 2022-03-01 to 2026-03-01 is 1,461 days with 2024's leap day, and to
    // 2026-11-02 another 246.
    assert_eq!(
        stale_cautions(date(2022, 3, 1), TODAY),
        [NetWorthCaution::RateStale {
            currency: rub,
            on: TODAY,
            rate_on: date(2022, 3, 1),
            age_days: 1707,
        }]
    );
}

#[test]
fn a_stale_rate_of_the_display_currency_names_it() {
    let rate_on = date(2022, 3, 1);
    let book = rates(&[("RUB", &[(rate_on, "90")])]);
    let held = [cash(1, "EUR", &[(TODAY, 100)])];
    let mut asked = input(&held, Some("RUB"), &book);
    asked.from = TODAY;
    asked.to = TODAY;
    assert_eq!(
        net_worth_report(&asked).unwrap().cautions,
        [NetWorthCaution::RateStale {
            currency: currency("RUB"),
            on: TODAY,
            rate_on,
            age_days: 1707,
        }]
    );
}

#[test]
fn only_the_worst_stale_day_of_a_currency_is_said() {
    let rate_on = date(2026, 10, 1);
    let book = rates(&[("THB", &[(rate_on, "37")])]);
    let held = [cash(1, "THB", &[(rate_on, 3700)])];
    let mut asked = input(&held, Some("EUR"), &book);
    asked.stale_after_days = u16::MAX;
    let report = net_worth_report(&asked).unwrap();
    assert_eq!(
        report.cautions,
        [NetWorthCaution::RateStale {
            currency: currency("THB"),
            on: NOV_1,
            rate_on,
            age_days: 31,
        }]
    );
}

#[test]
fn a_stale_rate_on_an_uncounted_day_is_not_said() {
    let rate_on = date(2026, 10, 1);
    let book = rates(&[("THB", &[(rate_on, "37")])]);
    let mut closed = cash(1, "THB", &[(rate_on, 3700)]);
    closed.line.closed_on = Some(rate_on);
    let open = cash(2, "EUR", &[(OCT_26, 5)]);
    let mut asked = input(&[closed, open], Some("EUR"), &book);
    asked.stale_after_days = u16::MAX;
    assert_eq!(net_worth_report(&asked).unwrap().cautions, []);
}

#[test]
fn amount_too_large_is_said_once_on_its_last_day() {
    let on = date(2026, 10, 1);
    let book = rates(&[("KWD", &[(on, "9999999999")])]);
    let held = [cash(1, "EUR", &[(OCT_26, i64::MAX)])];
    let report = net_worth_report(&input(&held, Some("KWD"), &book)).unwrap();
    let stale_free: Vec<&NetWorthCaution> = report
        .cautions
        .iter()
        .filter(|caution| !matches!(caution, NetWorthCaution::BalanceStale { .. }))
        .collect();
    assert_eq!(
        stale_free,
        [&NetWorthCaution::AmountTooLarge {
            account: id(1),
            on: NOV_1,
        }]
    );
    assert_eq!(point_on(&report, NOV_1).total, None);
    assert_eq!(point_on(&report, NOV_1).unconverted, [id(1)]);
    assert_eq!(report.status, NetWorthStatus::Provisional);
}

#[test]
fn positive_liability_balance_is_a_caution() {
    for liquidity in [
        LiquidityClass::ShortTermLiability,
        LiquidityClass::LongTermLiability,
    ] {
        let held = [holding(1, "USD", liquidity, &[(OCT_26, 300)])];
        let report = net_worth_report(&input(&held, Some("USD"), &RateBook::default())).unwrap();
        assert_eq!(
            report.cautions,
            [NetWorthCaution::LiabilityBalancePositive {
                account: id(1),
                balance: money(300, "USD"),
            }]
        );
        assert_eq!(
            point_on(&report, OCT_26).total,
            Some(money(300, "USD")),
            "it changes no number"
        );
    }
}

#[test]
fn a_liability_at_zero_or_below_and_an_asset_are_not_cautioned() {
    let held = [
        holding(1, "USD", LiquidityClass::ShortTermLiability, &[(OCT_26, 0)]),
        holding(2, "USD", LiquidityClass::LongTermLiability, &[(OCT_26, -5)]),
        holding(3, "USD", LiquidityClass::Invested, &[(OCT_26, 5)]),
        holding(4, "USD", LiquidityClass::Illiquid, &[(OCT_26, 5)]),
        cash(5, "USD", &[(OCT_26, 5)]),
    ];
    let report = net_worth_report(&input(&held, Some("USD"), &RateBook::default())).unwrap();
    assert_eq!(report.cautions, []);
}

#[test]
fn a_positive_liability_earlier_in_the_range_names_its_last_balance() {
    let held = [holding(
        1,
        "USD",
        LiquidityClass::ShortTermLiability,
        &[
            (OCT_26, 300),
            (date(2026, 10, 27), 200),
            (date(2026, 10, 28), -50),
        ],
    )];
    let report = net_worth_report(&input(&held, Some("USD"), &RateBook::default())).unwrap();
    assert_eq!(
        report.cautions,
        [NetWorthCaution::LiabilityBalancePositive {
            account: id(1),
            balance: money(200, "USD"),
        }]
    );
}

#[test]
fn stale_balance_only_on_the_latest_point() {
    let stale_on = date(2026, 10, 1);
    let refreshed = date(2026, 10, 28);
    // Old on 10-26 and 10-27, then refreshed: nothing is stale on the latest point.
    let held = [cash(1, "USD", &[(stale_on, 5), (refreshed, 6)])];
    let report = net_worth_report(&input(&held, Some("USD"), &RateBook::default())).unwrap();
    assert_eq!(report.cautions, []);

    // Old to the end: said once, aged to the latest point.
    let held = [cash(1, "USD", &[(stale_on, 5)])];
    let report = net_worth_report(&input(&held, Some("USD"), &RateBook::default())).unwrap();
    assert_eq!(
        report.cautions,
        [NetWorthCaution::BalanceStale {
            account: id(1),
            on: stale_on,
            age_days: 31,
        }]
    );
    assert_eq!(report.status, NetWorthStatus::Provisional);
}

#[test]
fn an_uncounted_old_balance_on_the_latest_point_is_not_stale() {
    let old = date(2026, 9, 1);
    let mut closed = cash(1, "USD", &[(old, 100)]);
    closed.line.closed_on = Some(old);
    let open = cash(2, "USD", &[(OCT_26, 5)]);
    let report = net_worth_report(&input(&[closed, open], Some("USD"), &RateBook::default()));
    assert_eq!(report.unwrap().cautions, []);
}

#[test]
fn a_debt_on_the_closing_day_itself_is_said() {
    let closing = date(2026, 10, 29);
    let mut owing = cash(1, "USD", &[(OCT_26, -700)]);
    owing.line.closed_on = Some(closing);
    let mut asked = input(&[owing], Some("USD"), &RateBook::default());
    asked.to = closing;
    assert_eq!(
        net_worth_report(&asked).unwrap().cautions,
        [NetWorthCaution::ClosedAccountOwing {
            account: id(1),
            balance: money(-700, "USD"),
        }]
    );
}

/// The rows' order is the store's: the answer's is the accounts'.
#[test]
fn stale_balances_are_listed_in_the_accounts_order() {
    let old = date(2026, 9, 1);
    let held = [cash(1, "USD", &[(old, 5)]), cash(2, "USD", &[(old, 6)])];
    let mut asked = input(&held, Some("USD"), &RateBook::default());
    asked.rows.reverse();
    let stale_on = |n| NetWorthCaution::BalanceStale {
        account: id(n),
        on: old,
        age_days: 61,
    };
    assert_eq!(
        net_worth_report(&asked).unwrap().cautions,
        [stale_on(1), stale_on(2)]
    );
}

#[test]
fn a_balance_is_stale_only_past_its_setting() {
    let held = [cash(1, "USD", &[(date(2026, 10, 25), 5)])];
    let mut asked = input(&held, Some("USD"), &RateBook::default());
    // 10-25 to 11-01 is 7 days.
    asked.stale_after_days = 7;
    assert_eq!(net_worth_report(&asked).unwrap().cautions, []);
    asked.stale_after_days = 6;
    assert_eq!(
        net_worth_report(&asked).unwrap().cautions,
        [NetWorthCaution::BalanceStale {
            account: id(1),
            on: date(2026, 10, 25),
            age_days: 7,
        }]
    );
}

#[test]
fn a_stale_balance_is_aged_to_the_last_point_by_period() {
    let held = [cash(1, "USD", &[(date(2026, 9, 30), 5)])];
    let mut asked = input(&held, Some("USD"), &RateBook::default());
    asked.interval = NetWorthInterval::Month;
    asked.to = TODAY;
    assert_eq!(
        net_worth_report(&asked).unwrap().cautions,
        [NetWorthCaution::BalanceStale {
            account: id(1),
            on: date(2026, 9, 30),
            age_days: 33,
        }]
    );
}
