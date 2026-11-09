//! The forecast's blockers and cautions as sentences a person understands. Every
//! `match` here is exhaustive, so a new kind fails the build until it has words, and
//! the tests below fail until it has a sample to check them with.

use std::borrow::Cow;
use std::collections::BTreeMap;

use coffer_domain::forecast::{
    AccountLine, Blocker, Caution, FeedProblemReason, FeedStatus, HintEffect, LineSource,
    RuleFiledTransfers, SettledBudget, SettledLine, bills_looked_at_from,
};
use coffer_domain::ids::{AccountId, CategoryId, PlannedItemId};
use coffer_domain::money::{Currency, Money};
use coffer_domain::plan::{CalendarMonth, SettlementKind};
use jiff::civil::Date;

use crate::http::pages::amounts::{day, magnitude, money};

/// What the forecast's ids are called. An id with no name, which only a change between
/// two reads can cause, falls back to a plain description.
pub(in crate::http::pages) struct Names<'a> {
    accounts: BTreeMap<AccountId, Cow<'a, str>>,
    pub(super) categories: BTreeMap<CategoryId, String>,
    pub(super) items: BTreeMap<PlannedItemId, String>,
}

impl<'a> Names<'a> {
    pub(in crate::http::pages) fn of_accounts(accounts: &'a [AccountLine]) -> Self {
        Self {
            accounts: accounts
                .iter()
                .map(|line| (line.account, Cow::Borrowed(line.name.as_str())))
                .collect(),
            categories: BTreeMap::new(),
            items: BTreeMap::new(),
        }
    }

    /// Whether a sentence about `id` would have no name for it.
    pub(super) fn lacks_account(&self, id: AccountId) -> bool {
        !self.accounts.contains_key(&id)
    }

    /// Learns an account's name from a lookup.
    pub(super) fn add_account(&mut self, id: AccountId, name: String) {
        self.accounts.insert(id, Cow::Owned(name));
    }

    /// The account's name, to go inside a sentence.
    fn account(&self, id: AccountId) -> &str {
        self.accounts.get(&id).map_or("an account", Cow::as_ref)
    }

    /// The account's name, to start a sentence.
    fn account_first(&self, id: AccountId) -> &str {
        self.accounts.get(&id).map_or("An account", Cow::as_ref)
    }

    fn category(&self, id: CategoryId) -> Option<&str> {
        self.categories.get(&id).map(String::as_str)
    }

    /// What a planned item is called. An imagined item has no stored name, and this page
    /// never shows one.
    pub(super) fn item(&self, source: LineSource) -> Option<&str> {
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

/// A budget lowered beside a bill settled as paid, which the number takes at its word.
pub(super) fn settled_budget_sentence(line: &SettledBudget) -> String {
    format!(
        "{}: {} of spending lowered this budget for {}. A bill settled as paid shares its \
         category, so if some of that was the bill's, the budget needs up to {} more.",
        line.name,
        money(line.amount),
        month_name(line.month),
        money(line.amount),
    )
}

/// What follows the missing emergency fund's sentence for someone who can't set one: an admin
/// gets a link to the Household page instead.
pub(in crate::http::pages) const ADMIN_SETS_EMERGENCY_FUND: &str =
    "An admin can set one on the Household page.";

/// A currency in use without an emergency fund: nothing is held back in it.
fn no_emergency_fund_sentence(currency: Currency) -> String {
    format!(
        "No emergency fund is set for {}, so everything left counts as free.",
        currency.as_str()
    )
}

/// Why there is no number.
pub(in crate::http::pages) fn blocker_sentence(blocker: &Blocker) -> String {
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
        Blocker::NoCountedAccounts => {
            "No account counts as cash yet, so there is nothing to measure the plan against."
                .to_owned()
        }
        Blocker::BalanceMissing { name, .. } => {
            format!("{name} has no balance yet, so it could hold anything.")
        }
    }
}

/// The account a caution speaks of, for the kinds that name one. A kind not listed has no
/// account to name; one added here later only costs its name if it is forgotten, as the
/// sentence then says "an account".
pub(super) fn caution_account(caution: &Caution) -> Option<AccountId> {
    match caution {
        Caution::BalanceStale { account, .. }
        | Caution::AccountFeedProblem { account, .. }
        | Caution::ClosedLiabilityOwing { account, .. }
        | Caution::LiabilityBalancePositive { account, .. }
        | Caution::ItemHintNotCounted { account, .. } => Some(*account),
        _ => None,
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

/// Bills and one-offs in a category, dated earlier this month, that no transaction paid. One
/// amount can sum several of them.
fn bill_sentence(
    names: &Names<'_>,
    items: &[LineSource],
    category: CategoryId,
    amount: Money,
) -> String {
    let found: Vec<&str> = items.iter().filter_map(|item| names.item(*item)).collect();
    let who = if found.is_empty() {
        "Bills or one-offs".to_owned()
    } else {
        listed(&found)
    };
    format!(
        "{who} in {} dated earlier this month come to {} that no payment of about that amount has \
         been seen for, so that much still counts as needed. If they were paid under another \
         category or at another amount, there is more to invest than this shows. If one was paid \
         some other way, such as from an account that isn't the household's, settle that bill for \
         the month.",
        names.category(category).unwrap_or("a category"),
        magnitude(amount),
    )
}

/// How to settle `item`'s month, whoever reads this: it covers all of that month's occurrences.
fn how_to_settle(item: LineSource, on: Date) -> String {
    let LineSource::PlannedItem { id } = item else {
        return String::new();
    };
    let month = on.strftime("%Y-%m");
    format!(
        " Settling covers all of {}. A member does it with the buttons on this page, and an agent \
         with write access sends PUT /api/v1/plan-items/{id}/settlements/{month} with \
         {{\"kind\": \"paid\"}} or {{\"kind\": \"skipped\"}}.",
        on.strftime("%B %Y"),
    )
}

const TRANSFER_LEG_REMOVED: &str = "A bank took back one side of a linked transfer, so its money \
                                    counts as filed again. Check the link, and remove it if it \
                                    no longer holds.";

/// A goal past its due date with money still to find. `goal` starts the sentence.
fn overdue_sentence(goal: &str, due_on: Date) -> String {
    format!(
        "{goal} was due on {} and still has money to find.",
        day(due_on)
    )
}

/// The first month whose closing cash is below the currency's emergency fund.
fn emergency_fund_dip_sentence(month: CalendarMonth, projected_cash: Money) -> String {
    format!(
        "In {}, cash is projected to fall to {}, below the emergency fund. Income arriving \
         later can't pay for that month.",
        month_name(month),
        money(projected_cash),
    )
}

/// A linked transfer between currencies whose two sides don't agree at the day's rate.
fn rate_mismatch_sentence(outflow: Money, inflow: Money, expected: Money) -> String {
    format!(
        "A linked transfer took {} out and brought {} in, but {} would be about {} at that \
         day's rate. Check the link, and remove it if the two sides aren't one transfer: the \
         money is counted as moved either way.",
        magnitude(outflow),
        money(inflow),
        magnitude(outflow),
        money(expected),
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
        HintEffect::LeftOut => format!(
            "{item} is expected to arrive in {account}, which doesn't count as cash here, so it \
             stays out of the number: the money may not reach counted cash. If it will, hint the \
             item at a counted account instead. If it isn't expected, mark the item inactive."
        ),
    }
}

/// An emergency fund of some months of recurring must pay and essentials outflows that came
/// to 0.
fn zero_emergency_fund_sentence(fund_months: u8, currency: Currency) -> String {
    format!(
        "The emergency fund in {1} is {0} of recurring must pay and essentials outflows, and \
         that comes to 0, since none are planned in {1}. Nothing is held back in that currency, so what could be invested \
         may be overstated.",
        months(fund_months),
        currency.as_str(),
    )
}

/// A goal and a one-off that may describe the same expense.
fn overlap_sentence(
    names: &Names<'_>,
    goal: LineSource,
    one_off: LineSource,
    category: CategoryId,
) -> String {
    let (goal, one_off) = (names.goal(goal), names.one_off(one_off));
    let category = names.category(category);
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

/// A bill from last month that nothing paid.
fn unpaid_last_month_sentence(
    names: &Names<'_>,
    item: LineSource,
    on: Date,
    amount: Money,
) -> String {
    format!(
        "{} was due on {} and no payment of about {} has been seen, so it still counts as \
         needed. If a transaction for it has no category or is marked a possible duplicate, file \
         it and this goes.{}",
        names.item(item).unwrap_or("A bill"),
        day(on),
        magnitude(amount),
        how_to_settle(item, on),
    )
}

/// A transaction that may be a bill's payment or a budget's spending.
fn unclear_payment_sentence(
    names: &Names<'_>,
    item: LineSource,
    on: Date,
    amount: Money,
    spent: Money,
) -> String {
    format!(
        "{} was due on {} for {}. A transaction of {} in its category may have paid it, but it \
         could also be spending on a budget there, so the bill still counts as needed.{}",
        names.item(item).unwrap_or("A bill"),
        day(on),
        magnitude(amount),
        magnitude(spent),
        how_to_settle(item, on),
    )
}

/// Last month's bills from before coffer's first transaction: nothing says whether they were paid.
fn history_sentence(starts_on: Option<Date>, occurrences: u32, amount: Money) -> String {
    let bills = if occurrences == 1 {
        "1 bill or one-off".to_owned()
    } else {
        format!("{occurrences} bills or one-offs")
    };
    let why = starts_on.map_or_else(
        || {
            format!(
                "Coffer has no transactions yet, so it can't tell whether {bills} due last month"
            )
        },
        |first| {
            format!(
                "Transactions start on {}, so coffer can't tell whether {bills} due before then",
                day(first),
            )
        },
    );
    format!(
        "{why}, {} in all, were paid. They aren't counted as needed. If one is still to be paid, \
         there is less to invest than this shows.",
        magnitude(amount),
    )
}

/// A refund that says nothing of the bill paid beside it.
fn refund_sentence(category: Option<&str>, amount: Money) -> String {
    format!(
        "A refund of {} in {} doesn't look like a bill's payment coming back, so the bill there \
         still counts as paid. If it was part of that payment, there is less to invest than this \
         shows.",
        magnitude(amount),
        category.unwrap_or("a category"),
    )
}

/// Last month's money that can't pay a bill until it is filed.
fn last_month_loose_sentence(count: u32, total: Money, duplicates: u32) -> String {
    let uncategorized = (count > 0).then(|| {
        format!(
            "{} with no category, {} in all",
            transactions(count),
            magnitude(total)
        )
    });
    let flagged =
        (duplicates > 0).then(|| format!("{} that may be duplicated", transactions(duplicates)));
    let found: Vec<String> = uncategorized.into_iter().chain(flagged).collect();
    format!(
        "Last month has {}. One may be the payment of a bill from last month, which counts as \
         unpaid until the transaction is filed or the duplicate cleared.",
        found.join(" and "),
    )
}

/// A window that doesn't start today.
fn not_today_sentence(today: Date, as_of: Date) -> String {
    format!(
        "This forecast starts on {start}, not today, {}. Anything due before {} is left out, and \
         so is income expected on or before {start} and any what-if item dated before it.",
        day(today),
        bills_looked_at_from(as_of).strftime("%B %Y"),
        start = day(as_of),
    )
}

/// A closed card still owing. `account` starts the sentence.
fn closed_owing_sentence(account: &str, balance: Money) -> String {
    format!(
        "{account} is closed but still owes {}, which counts against the cash available.",
        magnitude(balance),
    )
}

/// A card or loan showing money in hand. `account` starts the sentence.
fn positive_liability_sentence(account: &str, balance: Money) -> String {
    format!(
        "{account} is a card or loan, but shows {} in hand rather than owed. It may have been \
         entered with the wrong sign, and it counts as it stands.",
        magnitude(balance),
    )
}

fn stale_sentence(account: &str, age_days: u32, on: Date) -> String {
    format!(
        "The balance of {account} is {} old, from {}, so the cash available may be out of date.",
        days(age_days),
        day(on),
    )
}

fn uncategorized_sentence(count: u32, total: Money) -> String {
    format!(
        "This month has {} with no category, {} in all. Categorizing that spending could lower \
         what is still needed.",
        transactions(count),
        magnitude(total),
    )
}

/// What rules filed as `transfer` with no link, which changes no number: a note, not a
/// caution.
pub(super) fn rule_filed_transfers_sentence(filed: &RuleFiledTransfers) -> String {
    format!(
        "Rules filed {} ({}) as transfers with no link this month. A transfer is not counted \
         as spending, so if a rule is too broad, a real expense may be hiding there.",
        transactions(filed.count),
        magnitude(filed.amount),
    )
}

fn duplicates_sentence(count: u32) -> String {
    format!(
        "This month has {} that may be duplicated. That could inflate a category's spending, \
         which could make the need look smaller than it is. A possible duplicate doesn't count as \
         paying a bill until it is cleared.",
        transactions(count),
    )
}

/// What might change the number. `as_of` is the first day of the forecast.
pub(in crate::http::pages) fn caution_sentence(
    caution: &Caution,
    as_of: Date,
    names: &Names<'_>,
) -> String {
    match caution {
        Caution::BalanceStale {
            account,
            on,
            age_days,
        } => stale_sentence(names.account(*account), *age_days, *on),
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
        Caution::AsOfNotToday { today } => not_today_sentence(*today, as_of),
        Caution::BillNotSeen {
            category,
            amount,
            items,
        } => bill_sentence(names, items, *category, *amount),
        Caution::BillHistoryMissing {
            starts_on,
            occurrences,
            amount,
            ..
        } => history_sentence(*starts_on, *occurrences, *amount),
        Caution::RefundBesidePaidBill {
            category, amount, ..
        } => refund_sentence(names.category(*category), *amount),
        Caution::LastMonthLooseEnds {
            uncategorized_count: count,
            uncategorized_total: total,
            possible_duplicates: duplicates,
            ..
        } => last_month_loose_sentence(*count, *total, *duplicates),
        Caution::BillUnpaidLastMonth { item, on, amount } => {
            unpaid_last_month_sentence(names, *item, *on, *amount)
        }
        Caution::BillPaymentUnclear {
            item,
            on,
            amount,
            transaction_amount,
            ..
        } => unclear_payment_sentence(names, *item, *on, *amount, *transaction_amount),
        Caution::ClosedLiabilityOwing { account, balance } => {
            closed_owing_sentence(names.account_first(*account), *balance)
        }
        Caution::EmergencyFundNotSet { currency } => no_emergency_fund_sentence(*currency),
        Caution::EmergencyFundMonthsIsZero {
            currency,
            months: fund_months,
        } => zero_emergency_fund_sentence(*fund_months, *currency),
        Caution::LiabilityBalancePositive { account, balance } => {
            positive_liability_sentence(names.account_first(*account), *balance)
        }
        Caution::GoalOneOffOverlap {
            goal,
            one_off,
            category,
        } => overlap_sentence(names, *goal, *one_off, *category),
        Caution::GoalOverdue { goal, due_on } => overdue_sentence(&names.goal(*goal), *due_on),
        Caution::GoalLinkedBothSides { goal } => both_sides_sentence(&names.goal(*goal)),
        Caution::TransferLegRemoved { .. } => TRANSFER_LEG_REMOVED.to_owned(),
        Caution::TransferLinkRateMismatch {
            outflow,
            inflow,
            expected_inflow,
            ..
        } => rate_mismatch_sentence(*outflow, *inflow, *expected_inflow),
        Caution::ItemHintNotCounted {
            item,
            account,
            effect,
        } => hint_sentence(names, *item, *account, *effect),
        Caution::UncategorizedSpending { count, total, .. } => {
            uncategorized_sentence(*count, *total)
        }
        Caution::PossibleDuplicates { count } => duplicates_sentence(*count),
        Caution::SpendingNewerThanBalance { total, pending, .. } => {
            newer_spending_sentence(*total, *pending)
        }
        Caution::CashDipsBelowEmergencyFund {
            month,
            projected_cash,
            ..
        } => emergency_fund_dip_sentence(*month, *projected_cash),
    }
}

#[cfg(test)]
mod tests;
