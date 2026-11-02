//! The forecast's blockers and cautions as sentences a person understands. Every
//! `match` here is exhaustive, so a new kind fails the build until it has words, and
//! the tests below fail until it has a sample to check them with.

use std::collections::BTreeMap;

use coffer_domain::forecast::{
    AccountLine, Blocker, Caution, FeedProblemReason, FeedStatus, HintEffect, LineSource,
    SettledLine,
};
use coffer_domain::ids::{AccountId, CategoryId, PlannedItemId};
use coffer_domain::money::{Currency, Money};
use coffer_domain::plan::{CalendarMonth, SettlementKind};
use jiff::civil::Date;

use crate::http::pages::amounts::{day, magnitude, money};

/// What the forecast's ids are called. An id with no name, which only a change between
/// two reads can cause, falls back to a plain description.
pub(super) struct Names<'a> {
    accounts: BTreeMap<AccountId, &'a str>,
    pub(super) categories: BTreeMap<CategoryId, String>,
    pub(super) items: BTreeMap<PlannedItemId, String>,
}

impl<'a> Names<'a> {
    pub(super) fn of_accounts(accounts: &'a [AccountLine]) -> Self {
        Self {
            accounts: accounts
                .iter()
                .map(|line| (line.account, line.name.as_str()))
                .collect(),
            categories: BTreeMap::new(),
            items: BTreeMap::new(),
        }
    }

    /// The account's name, to go inside a sentence.
    fn account(&self, id: AccountId) -> &str {
        self.accounts.get(&id).copied().unwrap_or("an account")
    }

    /// The account's name, to start a sentence.
    fn account_first(&self, id: AccountId) -> &str {
        self.accounts.get(&id).copied().unwrap_or("An account")
    }

    fn category(&self, id: CategoryId) -> Option<&str> {
        self.categories.get(&id).map(String::as_str)
    }

    /// What a planned item is called. An imagined item has no stored name, and this page
    /// never shows one.
    fn item(&self, source: LineSource) -> Option<&str> {
        match source {
            LineSource::PlannedItem { id } => self.items.get(&id).map(String::as_str),
            LineSource::Hypothetical { .. } => None,
        }
    }

    /// The goal, to start a sentence.
    fn goal(&self, source: LineSource) -> String {
        self.item(source)
            .map_or_else(|| "A goal".to_owned(), |name| format!("The goal {name}"))
    }

    /// The one-off, to go inside a sentence.
    fn one_off(&self, source: LineSource) -> String {
        self.item(source).map_or_else(
            || "a one-off".to_owned(),
            |name| format!("the one-off {name}"),
        )
    }
}

pub(super) fn month_name(month: CalendarMonth) -> String {
    month.first_day().strftime("%B %Y").to_string()
}

pub(super) fn months(count: u8) -> String {
    if count == 1 {
        "1 month".to_owned()
    } else {
        format!("{count} months")
    }
}

fn days(count: u32) -> String {
    if count == 1 {
        "1 day".to_owned()
    } else {
        format!("{count} days")
    }
}

fn transactions(count: u32) -> String {
    if count == 1 {
        "1 transaction".to_owned()
    } else {
        format!("{count} transactions")
    }
}

/// `a`, `a and b`, or `a, b and c`.
fn listed(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [only] => (*only).to_owned(),
        [first @ .., last] => format!("{} and {last}", first.join(", ")),
    }
}

/// What a person settled, and so what the number leaves out.
pub(super) fn settled_sentence(line: &SettledLine) -> String {
    let word = match line.kind {
        SettlementKind::Paid => "paid",
        SettlementKind::Skipped => "skipped",
    };
    format!(
        "{}: {} for {} isn't counted, marked {word}.",
        line.name,
        money(line.amount),
        month_name(line.month),
    )
}

/// Why there is no number.
pub(super) fn blocker_sentence(blocker: &Blocker) -> String {
    match blocker {
        Blocker::MixedCurrencies { currencies } => {
            let codes: Vec<&str> = currencies.iter().map(Currency::as_str).collect();
            format!(
                "The household uses {}. A forecast doesn't convert between currencies, so there \
                 is no single total. Any sums below are per currency, and are never added \
                 together.",
                listed(&codes)
            )
        }
        Blocker::FloorNotSet => "No safety floor is set. Without one, coffer would overstate \
                                 what could be invested. It is set through the API."
            .to_owned(),
        Blocker::NoCountedAccounts => {
            "No account counts as cash yet, so there is nothing to measure the plan against."
                .to_owned()
        }
        Blocker::BalanceMissing { name, .. } => {
            format!("{name} has no balance yet, so it could hold anything.")
        }
    }
}

/// A provider account bound to no account.
fn unbound_sentence(
    name: &str,
    institution: Option<&str>,
    currency_raw: &str,
    currency_supported: bool,
) -> String {
    let source = institution.map_or_else(|| name.to_owned(), |bank| format!("{name} at {bank}"));
    let currency = if currency_supported {
        String::new()
    } else {
        format!(" Its currency, {currency_raw}, isn't one coffer can use.")
    };
    format!(
        "{source} comes from a bank connection but isn't set up as an account, so its money \
         isn't counted.{currency}"
    )
}

/// Why the feed of the account called `name` may not be keeping it current.
fn feed_sentence(name: &str, reason: FeedProblemReason) -> String {
    match reason {
        FeedProblemReason::ProviderError => {
            format!("The bank reported an error for {name}, so its balance may be out of date.")
        }
        FeedProblemReason::CurrencyUnsupported => format!(
            "The bank reports {name} in a currency coffer doesn't know, so its balance may be out \
             of date."
        ),
        FeedProblemReason::CurrencyDiffers => format!(
            "The bank reports {name} in a different currency from the account's, so its balance \
             may be out of date."
        ),
    }
}

/// Why the connection called `label` isn't syncing.
fn connection_sentence(label: &str, status: FeedStatus) -> String {
    match status {
        FeedStatus::Error => {
            format!(
                "The connection {label} failed to sync, so balances from it may be out of date."
            )
        }
        FeedStatus::ReauthRequired => format!(
            "The connection {label} needs signing in to again, so balances from it may be out of \
             date."
        ),
    }
}

/// Bills and one-offs in a category, dated earlier this month, that its spending doesn't
/// cover. One amount can sum several of them.
fn bill_sentence(category: Option<&str>, amount: Money) -> String {
    format!(
        "Bills or one-offs in {} dated earlier this month come to {} more than that category's \
         spending so far, so that much still counts as needed. If they were paid under another \
         category, there is more to invest than this shows. If one was paid from an account \
         that isn't the household's, even in this category, settle that bill for the month.",
        category.unwrap_or("a category"),
        magnitude(amount),
    )
}

/// A goal with spending linked on both the household's accounts and money that isn't its own.
/// `goal` starts the sentence.
fn both_sides_sentence(goal: &str) -> String {
    format!(
        "{goal} has spending linked to it on both the household's own accounts and an account \
         that isn't the household's. If one paid for the other, the same money lowers what it still needs \
         twice, so there may be less to invest than this shows. Link the payment or the \
         contribution, never both."
    )
}

/// An item hinted at an account that doesn't count, and what became of it.
fn hint_sentence(
    names: &Names<'_>,
    item: LineSource,
    account: AccountId,
    effect: HintEffect,
) -> String {
    let item = names.item(item).unwrap_or("A planned item");
    let account = names.account(account);
    match effect {
        HintEffect::StillNeeded => format!(
            "{item} is expected to be paid from {account}, which doesn't count as cash here. It \
             still counts as needed, which may overstate what counted cash has to cover."
        ),
        HintEffect::StillIncome => format!(
            "{item} is expected to arrive in {account}, which doesn't count as cash here. It \
             still counts as income, which may overstate what is free to invest."
        ),
        HintEffect::LeftOut => format!(
            "{item} is expected to arrive in {account}, which isn't the household's money, so it \
             stays out of the number. That is already right: don't remove the hint just to clear \
             this. If it isn't expected, mark the item inactive."
        ),
    }
}

/// A floor of some months of recurring outflows that came to 0.
fn zero_floor_sentence(floor_months: u8, currency: Currency) -> String {
    format!(
        "The safety floor is {} of recurring outflows, and that comes to 0 in {}, since none are \
         planned. Nothing is held back in that currency, so what could be invested may be \
         overstated.",
        months(floor_months),
        currency.as_str(),
    )
}

/// A goal and a one-off that may describe the same expense. `goal` starts the sentence.
fn overlap_sentence(goal: &str, one_off: &str, category: Option<&str>) -> String {
    format!(
        "{goal} and {one_off} are both in {} and close in date, so they may be the same expense. \
         Each counts on its own, so if they are the same, less is needed than this shows.",
        category.unwrap_or("the same category"),
    )
}

/// Money out that the balances may not show yet. An amount of 0 is left out, so a
/// sentence never says that 0.00 is outstanding.
fn newer_spending_sentence(total: Money, pending: Money) -> String {
    let which = if pending.minor_units == 0 {
        format!("{} posted after a balance's date", magnitude(total))
    } else if total.minor_units == 0 {
        format!("{} not posted yet", magnitude(pending))
    } else {
        format!(
            "{} posted after a balance's date, and {} not posted yet",
            magnitude(total),
            magnitude(pending),
        )
    };
    format!(
        "Some money out may not show in the balances yet: {which}. Where a balance doesn't \
         show it, the cash available is overstated by up to {}.",
        if pending.minor_units == 0 || total.minor_units == 0 {
            "this amount"
        } else {
            "these amounts"
        },
    )
}

/// What might change the number. `as_of` is the first day of the forecast.
pub(super) fn caution_sentence(caution: &Caution, as_of: Date, names: &Names<'_>) -> String {
    match caution {
        Caution::BalanceStale {
            account,
            on,
            age_days,
        } => format!(
            "The balance of {} is {} old, from {}, so the cash available may be out of date.",
            names.account(*account),
            days(*age_days),
            day(*on),
        ),
        Caution::UnboundProviderAccount {
            name,
            institution,
            currency_raw,
            currency_supported,
            ..
        } => unbound_sentence(
            name,
            institution.as_deref(),
            currency_raw,
            *currency_supported,
        ),
        Caution::AccountFeedProblem {
            account, reason, ..
        } => feed_sentence(names.account(*account), *reason),
        Caution::ConnectionNeedsAttention { label, status, .. } => {
            connection_sentence(label, *status)
        }
        Caution::AsOfNotToday { today } => format!(
            "This forecast starts on {start}, not today, {}. Anything due before {} is left out, \
             and so is income expected on or before {start} and any what-if item dated before it.",
            day(*today),
            as_of.strftime("%B %Y"),
            start = day(as_of),
        ),
        Caution::BillNotSeen { category, amount } => {
            bill_sentence(names.category(*category), *amount)
        }
        Caution::ClosedLiabilityOwing { account, balance } => format!(
            "{} is closed but still owes {}, which counts against the cash available.",
            names.account_first(*account),
            magnitude(*balance),
        ),
        Caution::MonthsFloorIsZero {
            currency,
            months: floor_months,
        } => zero_floor_sentence(*floor_months, *currency),
        Caution::LiabilityBalancePositive { account, balance } => format!(
            "{} is a card or loan, but shows {} in hand rather than owed. It may have been \
             entered with the wrong sign, and it counts as it stands.",
            names.account_first(*account),
            magnitude(*balance),
        ),
        Caution::GoalOneOffOverlap {
            goal,
            one_off,
            category,
        } => overlap_sentence(
            &names.goal(*goal),
            &names.one_off(*one_off),
            names.category(*category),
        ),
        Caution::GoalOverdue { goal, due_on } => format!(
            "{} was due on {} and still has money to find.",
            names.goal(*goal),
            day(*due_on),
        ),
        Caution::GoalLinkedBothSides { goal } => both_sides_sentence(&names.goal(*goal)),
        Caution::ItemHintNotCounted {
            item,
            account,
            effect,
        } => hint_sentence(names, *item, *account, *effect),
        Caution::UncategorizedSpending { count, total, .. } => format!(
            "This month has {} with no category, {} in all. Categorizing that spending could lower \
             what is still needed.",
            transactions(*count),
            magnitude(*total),
        ),
        Caution::PossibleDuplicates { count } => format!(
            "This month has {} that may be duplicated. That could inflate a category's spending, \
             which could make the need look smaller than it is or hide an unpaid bill.",
            transactions(*count),
        ),
        Caution::SpendingNewerThanBalance { total, pending, .. } => {
            newer_spending_sentence(*total, *pending)
        }
        Caution::CashDipsBelowFloor {
            month,
            projected_cash,
            ..
        } => format!(
            "In {}, cash is projected to fall to {}, below the safety floor. Income arriving \
             later can't pay for that month.",
            month_name(*month),
            money(*projected_cash),
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use coffer_domain::forecast::{
        Blocker, Caution, FeedProblemReason, FeedStatus, HintEffect, LineSource,
    };
    use coffer_domain::ids::{
        AccountId, CategoryId, ConnectionId, PlannedItemId, ProviderAccountId,
    };
    use coffer_domain::money::{Currency, Money};
    use coffer_domain::plan::CalendarMonth;
    use jiff::civil::{Date, date};

    use super::{Names, blocker_sentence, caution_sentence, newer_spending_sentence};
    use crate::http::pages::word_checks::{assert_worded, sampled_kinds, schema_kinds};

    fn usd(minor_units: i64) -> Money {
        Money {
            minor_units,
            currency: Currency::parse("USD").unwrap(),
        }
    }

    const ON: Date = date(2026, 11, 7);

    /// One of every caution, and of every reason inside one, so that each is read.
    fn cautions() -> Vec<Caution> {
        let account = AccountId::new();
        let provider_account = ProviderAccountId::new();
        let connection = ConnectionId::new();
        let category = CategoryId::new();
        let currency = Currency::parse("USD").unwrap();
        let goal = LineSource::PlannedItem {
            id: PlannedItemId::new(),
        };
        let one_off = LineSource::Hypothetical { index: 0 };
        let feed = |reason| Caution::AccountFeedProblem {
            account,
            provider_account,
            reason,
        };
        let connection_alert = |status| Caution::ConnectionNeedsAttention {
            connection,
            label: "Family bank".to_owned(),
            status,
        };
        vec![
            Caution::BalanceStale {
                account,
                on: ON,
                age_days: 8,
            },
            Caution::UnboundProviderAccount {
                provider_account,
                connection,
                name: "Everyday".to_owned(),
                institution: None,
                currency_raw: "XTS".to_owned(),
                currency_supported: false,
            },
            feed(FeedProblemReason::ProviderError),
            feed(FeedProblemReason::CurrencyUnsupported),
            feed(FeedProblemReason::CurrencyDiffers),
            connection_alert(FeedStatus::Error),
            connection_alert(FeedStatus::ReauthRequired),
            Caution::AsOfNotToday { today: ON },
            Caution::BillNotSeen {
                category,
                amount: usd(12_000),
            },
            Caution::ClosedLiabilityOwing {
                account,
                balance: usd(-30_000),
            },
            Caution::MonthsFloorIsZero {
                currency,
                months: 1,
            },
            Caution::LiabilityBalancePositive {
                account,
                balance: usd(5_000),
            },
            Caution::GoalOneOffOverlap {
                goal,
                one_off,
                category,
            },
            Caution::GoalOverdue { goal, due_on: ON },
            Caution::GoalLinkedBothSides { goal },
            Caution::ItemHintNotCounted {
                item: goal,
                account,
                effect: HintEffect::StillNeeded,
            },
            Caution::ItemHintNotCounted {
                item: goal,
                account,
                effect: HintEffect::StillIncome,
            },
            Caution::ItemHintNotCounted {
                item: goal,
                account,
                effect: HintEffect::LeftOut,
            },
            Caution::UncategorizedSpending {
                currency,
                count: 1,
                total: usd(1_000),
            },
            Caution::PossibleDuplicates { count: 2 },
            Caution::SpendingNewerThanBalance {
                currency,
                total: usd(2_500),
                pending: usd(700),
            },
            Caution::CashDipsBelowFloor {
                month: CalendarMonth::of(ON).unwrap(),
                currency,
                projected_cash: usd(-100),
            },
        ]
    }

    fn blockers() -> Vec<Blocker> {
        vec![
            Blocker::MixedCurrencies {
                currencies: vec![
                    Currency::parse("EUR").unwrap(),
                    Currency::parse("USD").unwrap(),
                ],
            },
            Blocker::FloorNotSet,
            Blocker::NoCountedAccounts,
            Blocker::BalanceMissing {
                account: AccountId::new(),
                name: "Rainy day".to_owned(),
            },
        ]
    }

    /// A new caution fails the build at `caution_sentence`'s `match`, and then fails here
    /// until it has a sample, whose sentence is then read.
    #[test]
    fn every_caution_kind_has_a_sentence() {
        let kinds = schema_kinds::<Caution>();
        let samples = cautions();
        assert_eq!(sampled_kinds(&samples), kinds);
        let names = Names::of_accounts(&[]);
        for caution in &samples {
            assert_worded(&caution_sentence(caution, ON, &names), &kinds);
        }
    }

    #[test]
    fn every_blocker_kind_has_a_sentence() {
        let kinds = schema_kinds::<Blocker>();
        let samples = blockers();
        assert_eq!(sampled_kinds(&samples), kinds);
        for blocker in &samples {
            assert_worded(&blocker_sentence(blocker), &kinds);
        }
    }

    /// Named things, for reading what a sentence says about them.
    struct Named {
        account: AccountId,
        category: CategoryId,
        goal: LineSource,
        one_off: LineSource,
        names: Names<'static>,
    }

    fn named() -> Named {
        let (account, category) = (AccountId::new(), CategoryId::new());
        let (goal_id, one_off_id) = (PlannedItemId::new(), PlannedItemId::new());
        let mut names = Names::of_accounts(&[]);
        names.accounts.insert(account, "Visa");
        names.categories.insert(category, "Home".to_owned());
        names.items.insert(goal_id, "Roof".to_owned());
        names.items.insert(one_off_id, "Boiler".to_owned());
        Named {
            account,
            category,
            goal: LineSource::PlannedItem { id: goal_id },
            one_off: LineSource::PlannedItem { id: one_off_id },
            names,
        }
    }

    fn said(caution: &Caution, names: &Names<'_>) -> String {
        caution_sentence(caution, date(2026, 11, 15), names)
    }

    #[test]
    fn newer_spending_names_only_the_amounts_that_are_not_zero() {
        let both = newer_spending_sentence(usd(4_500), usd(700));
        assert!(
            both.contains("45.00 USD posted after a balance's date, and 7.00 USD not posted yet")
        );
        assert!(both.contains("these amounts"));

        let posted = newer_spending_sentence(usd(4_500), usd(0));
        assert!(posted.contains("45.00 USD posted after a balance's date."));
        assert!(!posted.contains("not posted"));
        assert!(!posted.contains("0.00"));

        let pending = newer_spending_sentence(usd(0), usd(700));
        assert!(pending.contains("7.00 USD not posted yet."));
        assert!(!pending.contains("posted after"));
        assert!(!pending.contains("0.00 USD"));
    }

    /// A payment on an account that isn't the household's can't be recategorized into paying.
    #[test]
    fn an_unseen_bill_says_to_settle_one_paid_from_a_held_account() {
        let Named {
            category, names, ..
        } = named();
        let sentence = said(
            &Caution::BillNotSeen {
                category,
                amount: usd(1),
            },
            &names,
        );
        assert!(sentence.contains("settle that bill for the month"));
    }

    /// Removing the hint would clear the caution and add the money to the number.
    #[test]
    fn income_left_out_says_not_to_remove_the_hint() {
        let Named {
            goal,
            account,
            names,
            ..
        } = named();
        let sentence = said(
            &Caution::ItemHintNotCounted {
                item: goal,
                account,
                effect: HintEffect::LeftOut,
            },
            &names,
        );
        assert!(sentence.contains("don't remove the hint"));
        assert!(sentence.contains("mark the item inactive"));
    }

    #[test]
    fn an_overdue_goal_says_which_and_when() {
        let Named { goal, names, .. } = named();
        let sentence = said(&Caution::GoalOverdue { goal, due_on: ON }, &names);
        assert!(sentence.contains("The goal Roof was due on 7 November 2026"));
    }

    #[test]
    fn an_overlap_names_the_goal_the_one_off_and_the_category() {
        let Named {
            goal,
            one_off,
            category,
            names,
            ..
        } = named();
        let sentence = said(
            &Caution::GoalOneOffOverlap {
                goal,
                one_off,
                category,
            },
            &names,
        );
        assert!(sentence.contains("The goal Roof and the one-off Boiler are both in Home"));
        assert!(sentence.contains("may be the same expense"));
    }

    #[test]
    fn a_liability_in_hand_says_how_much_and_why_it_looks_wrong() {
        let Named {
            account: id, names, ..
        } = named();
        let sentence = said(
            &Caution::LiabilityBalancePositive {
                account: id,
                balance: usd(500_000),
            },
            &names,
        );
        assert!(sentence.contains("Visa"));
        assert!(sentence.contains("5,000.00 USD in hand"));
        assert!(sentence.contains("wrong sign"));
    }

    #[test]
    fn uncategorized_spending_says_how_many_and_how_much() {
        let sentence = said(
            &Caution::UncategorizedSpending {
                currency: Currency::parse("USD").unwrap(),
                count: 3,
                total: usd(123_456),
            },
            &Names::of_accounts(&[]),
        );
        assert!(sentence.contains("3 transactions with no category"));
        assert!(sentence.contains("1,234.56 USD in all"));
    }

    #[test]
    fn a_dip_below_the_floor_says_when_and_how_low() {
        let sentence = said(
            &Caution::CashDipsBelowFloor {
                month: CalendarMonth::of(ON).unwrap(),
                currency: Currency::parse("USD").unwrap(),
                projected_cash: usd(-123_456),
            },
            &Names::of_accounts(&[]),
        );
        assert!(sentence.contains("In November 2026"));
        assert!(sentence.contains("fall to -1,234.56 USD"));
        assert!(sentence.contains("below the safety floor"));
    }

    #[test]
    fn a_window_not_starting_today_gives_both_days() {
        let sentence = said(
            &Caution::AsOfNotToday { today: ON },
            &Names::of_accounts(&[]),
        );
        assert!(sentence.contains("starts on 15 November 2026"));
        assert!(sentence.contains("not today, 7 November 2026"));
        assert!(sentence.contains("Anything due before November 2026"));
        assert!(sentence.contains("income expected on or before 15 November 2026"));
        assert!(sentence.contains("any what-if item dated before it"));
    }

    /// Several items can sum into the one amount, so it can't be worded as one bill's.
    #[test]
    fn an_unseen_bill_reads_right_for_several_items() {
        let Named {
            category, names, ..
        } = named();
        let sentence = said(
            &Caution::BillNotSeen {
                category,
                amount: usd(12_000),
            },
            &names,
        );
        assert!(sentence.contains("Bills or one-offs in Home"));
        assert!(sentence.contains("120.00 USD more than that category's spending"));
        assert!(!sentence.contains("of it"), "no single bill is implied");
    }

    /// With no names at all, each sentence still reads as one: nothing like "The goal A
    /// planned item".
    #[test]
    fn missing_names_read_well() {
        let Named {
            account,
            category,
            goal,
            one_off,
            ..
        } = named();
        let names = Names::of_accounts(&[]);
        for (caution, expected) in [
            (
                Caution::GoalOverdue { goal, due_on: ON },
                "A goal was due on",
            ),
            (
                Caution::GoalOneOffOverlap {
                    goal,
                    one_off,
                    category,
                },
                "A goal and a one-off are both in the same category",
            ),
            (
                Caution::GoalLinkedBothSides { goal },
                "A goal has spending linked to it",
            ),
            (
                Caution::ItemHintNotCounted {
                    item: goal,
                    account,
                    effect: HintEffect::StillNeeded,
                },
                "A planned item is expected to be paid from an account,",
            ),
            (
                Caution::ItemHintNotCounted {
                    item: goal,
                    account,
                    effect: HintEffect::StillIncome,
                },
                "A planned item is expected to arrive in an account, which doesn't count",
            ),
            (
                Caution::ItemHintNotCounted {
                    item: goal,
                    account,
                    effect: HintEffect::LeftOut,
                },
                "A planned item is expected to arrive in an account, which isn't the household's",
            ),
            (
                Caution::BillNotSeen {
                    category,
                    amount: usd(1),
                },
                "Bills or one-offs in a category",
            ),
            (
                Caution::BalanceStale {
                    account,
                    on: ON,
                    age_days: 1,
                },
                "The balance of an account is 1 day old",
            ),
            (
                Caution::ClosedLiabilityOwing {
                    account,
                    balance: usd(-1),
                },
                "An account is closed",
            ),
        ] {
            assert!(said(&caution, &names).contains(expected), "{expected}");
        }
    }

    /// Reasons inside one kind say different things, so one can't be left as another's.
    #[test]
    fn each_sample_says_something_different() {
        let names = Names::of_accounts(&[]);
        let samples = cautions();
        let said: BTreeSet<String> = samples
            .iter()
            .map(|caution| caution_sentence(caution, ON, &names))
            .collect();
        assert_eq!(said.len(), samples.len());
    }
}
