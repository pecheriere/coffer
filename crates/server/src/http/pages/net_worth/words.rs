//! A net worth report's blockers and cautions as sentences a person understands. Every
//! `match` here is exhaustive, so a new kind fails the build until it has words, and the
//! tests below fail until it has a sample to check them with.

use std::collections::BTreeMap;

use coffer_domain::accounts::{BelongsTo, LiquidityClass};
use coffer_domain::ids::AccountId;
use coffer_domain::money::Money;
use coffer_domain::net_worth::{
    AccountPoint, AccountSeries, ConversionState, NetWorthBlocker, NetWorthCaution,
    RateMissingReason,
};
use jiff::civil::Date;

use crate::http::pages::amounts::{day, magnitude};

/// What a report's accounts are called, and how they stood. An id with no name, which only
/// a change between two reads could cause, falls back to a plain description.
pub(super) struct Names<'a> {
    accounts: BTreeMap<AccountId, &'a AccountSeries>,
    /// The day of the report's latest point.
    latest: Option<Date>,
}

impl<'a> Names<'a> {
    pub(super) fn of_accounts(accounts: &'a [AccountSeries], latest: Option<Date>) -> Self {
        Self {
            accounts: accounts.iter().map(|line| (line.account, line)).collect(),
            latest,
        }
    }

    /// The account's name, to go inside a sentence.
    pub(super) fn account(&self, id: AccountId) -> &str {
        self.accounts
            .get(&id)
            .map_or("an account", |line| line.name.as_str())
    }

    /// The account's name, to start a sentence.
    fn account_first(&self, id: AccountId) -> &str {
        self.accounts
            .get(&id)
            .map_or("An account", |line| line.name.as_str())
    }

    /// Whether the account's latest point counts and meets `holds`, so that the present
    /// tense is true of it.
    fn holds_now(&self, id: AccountId, holds: impl Fn(&AccountPoint) -> bool) -> bool {
        self.accounts
            .get(&id)
            .and_then(|line| {
                line.points
                    .iter()
                    .find(|point| Some(point.on) == self.latest)
            })
            .is_some_and(|point| point.counted && holds(point))
    }

    /// The last point at which the account counted and met `holds`.
    fn last_held(&self, id: AccountId, holds: impl Fn(&AccountPoint) -> bool) -> Option<Date> {
        self.accounts
            .get(&id)
            .and_then(|line| {
                line.points
                    .iter()
                    .rfind(|point| point.counted && holds(point))
            })
            .map(|point| point.on)
    }
}

/// ` on 3 May 2026`, or nothing with no date to give.
fn on_day(date: Option<Date>) -> String {
    date.map_or_else(String::new, |date| format!(" on {}", day(date)))
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

/// Why a bucket has no number. Its accounts never count towards net worth, so that blocker's
/// usual words would say nothing about it, and the sums are promised only if it has some.
pub(super) fn bucket_blocker_sentence(blocker: NetWorthBlocker, has_sums: bool) -> &'static str {
    match blocker {
        NetWorthBlocker::NoCountedAccounts => {
            "None of these accounts has a value in this range, which is not a balance of zero."
        }
        NetWorthBlocker::DisplayCurrencyNotSet if !has_sums => {
            "No display currency is chosen, so there is no single total."
        }
        NetWorthBlocker::DisplayCurrencyNotSet | NetWorthBlocker::Rebuilding => {
            blocker_sentence(blocker)
        }
    }
}

/// A bucket's heading. A report's type doesn't rule out `Household`, which never has a bucket,
/// so it reads as what it is rather than panicking on a page.
pub(super) fn bucket_heading(belongs_to: BelongsTo, beneficiary: Option<&str>) -> String {
    match (belongs_to, beneficiary) {
        (BelongsTo::Household, _) => "The household's".to_owned(),
        (BelongsTo::SomeoneElse, Some(label)) => format!("Held for {label}"),
        (BelongsTo::SomeoneElse, None) => "Held for someone else".to_owned(),
        (BelongsTo::GivenAway, Some(label)) => format!("Given away: {label}"),
        (BelongsTo::GivenAway, None) => "Given away".to_owned(),
    }
}

fn rate_reason(reason: RateMissingReason) -> &'static str {
    match reason {
        RateMissingReason::NotFetchedYet => "coffer hasn't fetched its rates yet",
        RateMissingReason::NotPublished => {
            "the source of rates doesn't publish it, though an admin can enter a rate on the \
             household page"
        }
        RateMissingReason::FetchFailing => "fetching its rates is failing, and coffer will retry",
        RateMissingReason::ProviderOff => {
            "this server is set up with no source of rates, though an admin can enter a rate on \
             the household page"
        }
        RateMissingReason::NoneThatEarly => "coffer's rates for it don't go back that far",
    }
}

/// How a caution speaks of the money: counted in net worth, or in a bucket, which counts in
/// nothing and may have no total to speak of.
struct CautionWords {
    stands: &'static str,
    stood: &'static str,
    keeps: &'static str,
    last: &'static str,
    nothing_open: &'static str,
    all_closed: &'static str,
    from_before: &'static str,
    out_of_date: &'static str,
}

const IN_NET_WORTH: CautionWords = CautionWords {
    stands: "it counts as it stands",
    stood: "that counted as it stood",
    keeps: "which keeps counting",
    last: "when last counted",
    nothing_open: "Nothing counts",
    all_closed: "every account counted by then has closed",
    from_before: "The number here is",
    out_of_date: "the total",
};

const IN_A_BUCKET: CautionWords = CautionWords {
    stands: "it's in this bucket's figures as it stands",
    stood: "that was in this bucket's figures as it stood",
    keeps: "which stays in this bucket's figures",
    last: "when last in this bucket's figures",
    nothing_open: "This bucket has no open account",
    all_closed: "every account in it had closed by then",
    from_before: "The number here is",
    out_of_date: "the total",
};

const IN_A_BUCKET_WITH_NO_TOTAL: CautionWords = CautionWords {
    from_before: "The figures here are",
    out_of_date: "the figures",
    ..IN_A_BUCKET
};

/// What might change the numbers.
pub(super) fn caution_sentence(caution: &NetWorthCaution, names: &Names<'_>) -> String {
    worded_caution(caution, names, &IN_NET_WORTH)
}

/// What might change a bucket's numbers, in words true of money in no net worth, and of a
/// bucket showing no total if it has none.
pub(super) fn bucket_caution_sentence(
    caution: &NetWorthCaution,
    names: &Names<'_>,
    has_a_total: bool,
) -> String {
    let words = if has_a_total {
        &IN_A_BUCKET
    } else {
        &IN_A_BUCKET_WITH_NO_TOTAL
    };
    worded_caution(caution, names, words)
}

fn worded_caution(caution: &NetWorthCaution, names: &Names<'_>, words: &CautionWords) -> String {
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
        NetWorthCaution::RateManual {
            currency,
            first_on,
            last_on,
        } => format!(
            "The {} rate used {} was entered by hand, not published, so those amounts are only \
             as good as that entry.",
            currency.as_str(),
            stretch(*first_on, *last_on),
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
            "The balance of {} is {} old, from {}, so {} may be out of date.",
            names.account(*account),
            days(*age_days),
            day(*on),
            words.out_of_date,
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
            "{} {}, since {}. {} from before that, not the value now.",
            words.nothing_open,
            stretch(*from, *to),
            words.all_closed,
            words.from_before,
        ),
        NetWorthCaution::LiabilityBalancePositive { account, balance } => {
            positive_liability_sentence(*account, *balance, names, words)
        }
        NetWorthCaution::ClosedAccountOwing { account, balance } => {
            closed_owing_sentence(*account, *balance, names, words)
        }
    }
}

/// A caution's `balance` is from the last point it held, so it's said in the present only
/// if the latest point still shows it.
fn positive_liability_sentence(
    account: AccountId,
    balance: Money,
    names: &Names<'_>,
    words: &CautionWords,
) -> String {
    let in_hand = |point: &AccountPoint| point.balance.minor_units > 0;
    let name = names.account_first(account);
    let amount = magnitude(balance);
    if names.holds_now(account, in_hand) {
        format!(
            "{name} is a card or loan, but shows {amount} in hand rather than owed. It may have \
             been entered with the wrong sign, and {}.",
            words.stands,
        )
    } else {
        format!(
            "{name} showed {amount} in hand{}, though it is a card or loan. It may have been \
             entered with the wrong sign, and {}.",
            on_day(names.last_held(account, in_hand)),
            words.stood,
        )
    }
}

fn closed_owing_sentence(
    account: AccountId,
    balance: Money,
    names: &Names<'_>,
    words: &CautionWords,
) -> String {
    let owing = |point: &AccountPoint| point.balance.minor_units < 0;
    let name = names.account_first(account);
    let amount = magnitude(balance);
    if names.holds_now(account, owing) {
        format!("{name} is closed but still owes {amount}, {}.", words.keeps)
    } else {
        let when = names
            .last_held(account, owing)
            .map_or_else(String::new, |date| format!(", on {}", day(date)));
        format!("{name} is closed and owed {amount} {}{when}.", words.last)
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

    use coffer_domain::accounts::BelongsTo;
    use coffer_domain::ids::AccountId;
    use coffer_domain::money::{Currency, Money};
    use coffer_domain::net_worth::{NetWorthBlocker, NetWorthCaution, RateMissingReason};
    use jiff::civil::{Date, date};

    use super::{
        Names, blocker_sentence, bucket_blocker_sentence, bucket_caution_sentence, bucket_heading,
        caution_sentence,
    };
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
            NetWorthCaution::RateManual {
                currency: usd(),
                first_on: date(2026, 10, 1),
                last_on: ON,
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
        let names = Names::of_accounts(&[], None);
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

    #[test]
    fn each_blocker_has_words_for_a_bucket() {
        let kinds = schema_kinds::<NetWorthBlocker>();
        for blocker in blockers() {
            for has_sums in [true, false] {
                assert_worded(bucket_blocker_sentence(blocker, has_sums), &kinds);
            }
        }
        assert!(
            !bucket_blocker_sentence(NetWorthBlocker::NoCountedAccounts, true)
                .contains("net worth")
        );
    }

    /// Whatever a bucket's caution says, it never says the money counts.
    #[test]
    fn no_caution_says_a_bucket_counts() {
        let kinds = schema_kinds::<NetWorthCaution>();
        let names = Names::of_accounts(&[], None);
        for caution in &cautions() {
            for has_a_total in [true, false] {
                let sentence = bucket_caution_sentence(caution, &names, has_a_total);
                assert_worded(&sentence, &kinds);
                assert!(!sentence.replace("account", "").contains("count"));
            }
        }
    }

    #[test]
    fn a_bucket_is_headed_by_whose_it_is() {
        for (belongs_to, label, heading) in [
            (BelongsTo::SomeoneElse, Some("Child A"), "Held for Child A"),
            (BelongsTo::SomeoneElse, None, "Held for someone else"),
            (BelongsTo::GivenAway, None, "Given away"),
            (
                BelongsTo::GivenAway,
                Some("Family fund"),
                "Given away: Family fund",
            ),
            (BelongsTo::Household, None, "The household's"),
            (BelongsTo::Household, Some("Child A"), "The household's"),
        ] {
            assert_eq!(bucket_heading(belongs_to, label), heading);
        }
    }

    /// Reasons inside one kind say different things, so one can't be left as another's.
    #[test]
    fn each_sample_says_something_different() {
        let names = Names::of_accounts(&[], None);
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
            &Names::of_accounts(&[], None),
        );
        assert!(sentence.contains("owed 1,234.56 USD"));
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
            &Names::of_accounts(&[], None),
        );
        assert!(sentence.starts_with("USD can't be converted on 2 November 2026:"));
    }

    /// Nothing counted for a single day is said of that day, of the household and of a bucket.
    #[test]
    fn nothing_counted_for_a_single_day_reads_as_one() {
        let caution = NetWorthCaution::NothingCountedAfterLastPoint { from: ON, to: ON };
        let names = Names::of_accounts(&[], None);

        assert!(
            caution_sentence(&caution, &names).starts_with("Nothing counts on 2 November 2026,")
        );
        assert!(
            bucket_caution_sentence(&caution, &names, true)
                .starts_with("This bucket has no open account on 2 November 2026,")
        );
    }
}
