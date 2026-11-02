//! A net worth report's blockers and cautions as sentences a person understands. Every
//! `match` here is exhaustive, so a new kind fails the build until it has words, and the
//! tests below fail until it has a sample to check them with.

use std::collections::BTreeMap;

use coffer_domain::accounts::LiquidityClass;
use coffer_domain::ids::AccountId;
use coffer_domain::net_worth::{
    AccountSeries, ConversionState, NetWorthBlocker, NetWorthCaution, RateMissingReason,
};
use jiff::civil::Date;

use crate::http::pages::amounts::{day, magnitude};

/// What a report's accounts are called. An id with no name, which only a change between
/// two reads could cause, falls back to a plain description.
pub(super) struct Names<'a>(BTreeMap<AccountId, &'a str>);

impl<'a> Names<'a> {
    pub(super) fn of_accounts(accounts: &'a [AccountSeries]) -> Self {
        Self(
            accounts
                .iter()
                .map(|line| (line.account, line.name.as_str()))
                .collect(),
        )
    }

    /// The account's name, to go inside a sentence.
    pub(super) fn account(&self, id: AccountId) -> &str {
        self.0.get(&id).copied().unwrap_or("an account")
    }

    /// The account's name, to start a sentence.
    fn account_first(&self, id: AccountId) -> &str {
        self.0.get(&id).copied().unwrap_or("An account")
    }
}

fn days(count: u32) -> String {
    if count == 1 {
        "1 day".to_owned()
    } else {
        format!("{count} days")
    }
}

/// `on 3 May 2026`, or `from 3 May 2026 to 9 May 2026`.
fn stretch(first: Date, last: Date) -> String {
    if first == last {
        format!("on {}", day(first))
    } else {
        format!("from {} to {}", day(first), day(last))
    }
}

/// Why there is no number.
pub(super) fn blocker_sentence(blocker: NetWorthBlocker) -> &'static str {
    match blocker {
        NetWorthBlocker::DisplayCurrencyNotSet => {
            "No display currency is chosen, so there is no single total. The sums below are per \
             currency, and are never added together."
        }
        NetWorthBlocker::Rebuilding => {
            "coffer is working out its history again, so there is no number for now. Try again \
             shortly."
        }
        NetWorthBlocker::NoCountedAccounts => {
            "No account counts towards net worth in this range, which is not a net worth of zero."
        }
    }
}

fn rate_reason(reason: RateMissingReason) -> &'static str {
    match reason {
        RateMissingReason::NotFetchedYet => "coffer hasn't fetched its rates yet",
        RateMissingReason::NotPublished => "the source of rates doesn't publish it",
        RateMissingReason::FetchFailing => "fetching its rates is failing, and coffer will retry",
        RateMissingReason::ProviderOff => "this server is set up with no source of rates",
        RateMissingReason::NoneThatEarly => "coffer's rates for it don't go back that far",
    }
}

/// What might change the numbers.
pub(super) fn caution_sentence(caution: &NetWorthCaution, names: &Names<'_>) -> String {
    match caution {
        NetWorthCaution::RateMissing {
            currency,
            reason,
            first_on,
            last_on,
        } => format!(
            "{} can't be converted {}: {}. Those periods have no total, which isn't money leaving.",
            currency.as_str(),
            stretch(*first_on, *last_on),
            rate_reason(*reason),
        ),
        NetWorthCaution::RateStale {
            currency,
            on,
            rate_on,
            age_days,
        } => format!(
            "The {} rate used for {} is from {}, {} earlier, so that amount may be out of date.",
            currency.as_str(),
            day(*on),
            day(*rate_on),
            days(*age_days),
        ),
        NetWorthCaution::AmountTooLarge { account, on } => format!(
            "{} is too large to convert, last on {}. Those periods have no total, which isn't \
             money leaving.",
            names.account_first(*account),
            day(*on),
        ),
        NetWorthCaution::BalanceStale {
            account,
            on,
            age_days,
        } => format!(
            "The balance of {} is {} old, from {}, so the total may be out of date.",
            names.account(*account),
            days(*age_days),
            day(*on),
        ),
        NetWorthCaution::ToDateNotYetFilled { through } => format!(
            "coffer has worked out values up to {}, so later days aren't shown yet.",
            day(*through),
        ),
        NetWorthCaution::DailyValueMissing {
            account,
            first_on,
            last_on,
        } => format!(
            "{} has no value {}, though it had one before. Those periods have no total, which \
             isn't money leaving.",
            names.account_first(*account),
            stretch(*first_on, *last_on),
        ),
        NetWorthCaution::NothingCountedAfterLastPoint { from, to } => format!(
            "Nothing counts from {} to {}, since every account counted by then has closed. The \
             number here is from before that, not the value now.",
            day(*from),
            day(*to),
        ),
        NetWorthCaution::LiabilityBalancePositive { account, balance } => format!(
            "{} is a card or loan, but shows {} in hand rather than owed. It may have been \
             entered with the wrong sign, and it counts as it stands.",
            names.account_first(*account),
            magnitude(*balance),
        ),
        NetWorthCaution::ClosedAccountOwing { account, balance } => format!(
            "{} is closed but still owes {}, which keeps counting.",
            names.account_first(*account),
            magnitude(*balance),
        ),
    }
}

/// Why one account has no amount for a period, for that period's row of the table.
pub(super) fn no_amount_sentence(name: &str, conversion: Option<ConversionState>) -> String {
    match conversion {
        Some(ConversionState::Missing) => format!("{name} has no exchange rate for this day."),
        Some(ConversionState::TooLarge) => format!("{name} is too large to convert."),
        Some(ConversionState::NoDisplayCurrency) => {
            format!("{name} can't be converted with no display currency.")
        }
        Some(ConversionState::Same | ConversionState::Converted) => {
            format!("{name} has no amount for this day.")
        }
        None => format!("{name} has no value for this day, though it had one before."),
    }
}

/// What's in a liquidity class, in the words of a person rather than an accountant.
pub(super) fn liquidity_name(class: LiquidityClass) -> &'static str {
    match class {
        LiquidityClass::Cash => "Cash",
        LiquidityClass::ShortTermLiability => "Cards and short-term debts",
        LiquidityClass::Invested => "Invested",
        LiquidityClass::Illiquid => "Hard to sell, such as property",
        LiquidityClass::LongTermLiability => "Loans and long-term debts",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use coffer_domain::ids::AccountId;
    use coffer_domain::money::{Currency, Money};
    use coffer_domain::net_worth::{NetWorthBlocker, NetWorthCaution, RateMissingReason};
    use jiff::civil::{Date, date};

    use super::{Names, blocker_sentence, caution_sentence};
    use crate::http::pages::word_checks::{assert_worded, sampled_kinds, schema_kinds};

    const ON: Date = date(2026, 11, 2);

    fn usd() -> Currency {
        Currency::parse("USD").unwrap()
    }

    fn cautions() -> Vec<NetWorthCaution> {
        let account = AccountId::new();
        let balance = Money {
            minor_units: -30_000,
            currency: usd(),
        };
        let missing = |reason| NetWorthCaution::RateMissing {
            currency: usd(),
            reason,
            first_on: date(2026, 10, 1),
            last_on: ON,
        };
        vec![
            missing(RateMissingReason::NotFetchedYet),
            missing(RateMissingReason::NotPublished),
            missing(RateMissingReason::FetchFailing),
            missing(RateMissingReason::ProviderOff),
            missing(RateMissingReason::NoneThatEarly),
            NetWorthCaution::RateStale {
                currency: usd(),
                on: ON,
                rate_on: date(2026, 10, 1),
                age_days: 32,
            },
            NetWorthCaution::AmountTooLarge { account, on: ON },
            NetWorthCaution::BalanceStale {
                account,
                on: date(2026, 10, 1),
                age_days: 32,
            },
            NetWorthCaution::ToDateNotYetFilled { through: ON },
            NetWorthCaution::DailyValueMissing {
                account,
                first_on: ON,
                last_on: ON,
            },
            NetWorthCaution::NothingCountedAfterLastPoint {
                from: date(2026, 10, 1),
                to: ON,
            },
            NetWorthCaution::LiabilityBalancePositive { account, balance },
            NetWorthCaution::ClosedAccountOwing { account, balance },
        ]
    }

    fn blockers() -> Vec<NetWorthBlocker> {
        vec![
            NetWorthBlocker::DisplayCurrencyNotSet,
            NetWorthBlocker::Rebuilding,
            NetWorthBlocker::NoCountedAccounts,
        ]
    }

    /// A new caution fails the build at `caution_sentence`'s `match`, and then fails here
    /// until it has a sample, whose sentence is then read.
    #[test]
    fn each_caution_kind_has_words() {
        let kinds = schema_kinds::<NetWorthCaution>();
        let samples = cautions();
        assert_eq!(sampled_kinds(&samples), kinds);
        let names = Names::of_accounts(&[]);
        for caution in &samples {
            assert_worded(&caution_sentence(caution, &names), &kinds);
        }
    }

    #[test]
    fn each_blocker_kind_has_words() {
        let kinds = schema_kinds::<NetWorthBlocker>();
        let samples = blockers();
        assert_eq!(sampled_kinds(&samples), kinds);
        for blocker in &samples {
            assert_worded(blocker_sentence(*blocker), &kinds);
        }
    }

    /// Reasons inside one kind say different things, so one can't be left as another's.
    #[test]
    fn each_sample_says_something_different() {
        let names = Names::of_accounts(&[]);
        let samples = cautions();
        let said: BTreeSet<String> = samples
            .iter()
            .map(|caution| caution_sentence(caution, &names))
            .collect();
        assert_eq!(said.len(), samples.len());
    }

    #[test]
    fn a_debt_is_said_without_its_sign() {
        let sentence = caution_sentence(
            &NetWorthCaution::ClosedAccountOwing {
                account: AccountId::new(),
                balance: Money {
                    minor_units: -123_456,
                    currency: usd(),
                },
            },
            &Names::of_accounts(&[]),
        );
        assert!(sentence.contains("still owes 1,234.56 USD"));
    }

    #[test]
    fn a_single_day_reads_as_one() {
        let sentence = caution_sentence(
            &NetWorthCaution::RateMissing {
                currency: usd(),
                reason: RateMissingReason::NotFetchedYet,
                first_on: ON,
                last_on: ON,
            },
            &Names::of_accounts(&[]),
        );
        assert!(sentence.starts_with("USD can't be converted on 2 November 2026:"));
    }
}
