use std::collections::BTreeSet;

use coffer_domain::forecast::{
    Blocker, Caution, FeedProblemReason, FeedStatus, HintEffect, LineSource, RuleFiledTransfers,
};
use coffer_domain::ids::{
    AccountId, CategoryId, ConnectionId, PlannedItemId, ProviderAccountId, TransactionId,
    TransferLinkId,
};
use coffer_domain::money::{Currency, Money};
use coffer_domain::plan::CalendarMonth;
use jiff::civil::{Date, date};

use super::{
    Names, blocker_sentence, caution_sentence, newer_spending_sentence,
    rule_filed_transfers_sentence,
};
use crate::http::pages::word_checks::{assert_worded, sampled_kinds, schema_kinds};

fn usd(minor_units: i64) -> Money {
    Money {
        minor_units,
        currency: Currency::parse("USD").unwrap(),
    }
}

fn eur(minor_units: i64) -> Money {
    Money {
        minor_units,
        currency: Currency::parse("EUR").unwrap(),
    }
}

const ON: Date = date(2026, 11, 7);

/// One of every caution, and of every reason inside one, so that each is read.
fn cautions() -> Vec<Caution> {
    let item = LineSource::PlannedItem {
        id: PlannedItemId::new(),
    };
    let mut all = cautions_other_than_a_bills();
    all.push(Caution::BillHistoryMissing {
        starts_on: Some(ON),
        currency: Currency::parse("USD").unwrap(),
        occurrences: 8,
        amount: usd(600_000),
    });
    all.push(Caution::RefundBesidePaidBill {
        category: CategoryId::new(),
        transaction: TransactionId::new(),
        amount: usd(25_000),
    });
    all.push(Caution::LastMonthLooseEnds {
        currency: Currency::parse("USD").unwrap(),
        uncategorized_count: 2,
        uncategorized_total: usd(5_000),
        possible_duplicates: 1,
    });
    all.push(Caution::BillUnpaidLastMonth {
        item,
        on: ON,
        amount: usd(12_000),
    });
    all.push(Caution::BillPaymentUnclear {
        item,
        on: ON,
        amount: usd(12_000),
        transaction: TransactionId::new(),
        transaction_amount: usd(11_000),
    });
    all
}

/// What a provider's feed or connection can say.
fn feed_cautions() -> Vec<Caution> {
    let account = AccountId::new();
    let provider_account = ProviderAccountId::new();
    let connection = ConnectionId::new();
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
    ]
}

fn cautions_other_than_a_bills() -> Vec<Caution> {
    let account = AccountId::new();
    let category = CategoryId::new();
    let currency = Currency::parse("USD").unwrap();
    let goal = LineSource::PlannedItem {
        id: PlannedItemId::new(),
    };
    let one_off = LineSource::Hypothetical { index: 0 };
    let mut all = feed_cautions();
    all.extend([
        Caution::BalanceStale {
            account,
            on: ON,
            age_days: 8,
        },
        Caution::AsOfNotToday { today: ON },
        Caution::BillNotSeen {
            category,
            amount: usd(12_000),
            items: Vec::new(),
        },
        Caution::ClosedLiabilityOwing {
            account,
            balance: usd(-30_000),
        },
        Caution::EmergencyFundMonthsIsZero {
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
        Caution::TransferLegRemoved {
            link: TransferLinkId::new(),
        },
        Caution::TransferLinkRateMismatch {
            link: TransferLinkId::new(),
            outflow: usd(-125_000),
            inflow: eur(1_250),
            expected_inflow: eur(111_900),
            rate_on: ON,
            manual_rate: false,
        },
        Caution::ItemHintNotCounted {
            item: goal,
            account,
            effect: HintEffect::StillNeeded,
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
        Caution::CashDipsBelowEmergencyFund {
            month: CalendarMonth::of(ON).unwrap(),
            currency,
            projected_cash: usd(-100),
        },
    ]);
    all
}

fn blockers() -> Vec<Blocker> {
    vec![
        Blocker::MixedCurrencies {
            currencies: vec![
                Currency::parse("EUR").unwrap(),
                Currency::parse("USD").unwrap(),
            ],
        },
        Blocker::EmergencyFundNotSet {
            currency: Currency::parse("EUR").unwrap(),
        },
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
    assert!(both.contains("45.00 USD posted after a balance's date, and 7.00 USD not posted yet"));
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
            items: Vec::new(),
        },
        &names,
    );
    assert!(sentence.contains("settle that bill for the month"));
}

/// Removing the hint would clear the caution and add the money to the number, so the way to
/// count it is a counted account.
#[test]
fn income_left_out_says_to_hint_a_counted_account_if_it_will_arrive() {
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
    assert!(sentence.contains("hint the item at a counted account"));
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
fn rule_filed_transfers_say_how_many_and_how_much() {
    let sentence = rule_filed_transfers_sentence(&RuleFiledTransfers {
        currency: Currency::parse("USD").unwrap(),
        count: 2,
        amount: usd(45_000),
    });
    assert!(sentence.contains("Rules filed 2 transactions (450.00 USD)"));
    assert!(sentence.contains("a real expense may be hiding"));
}

#[test]
fn a_dip_below_the_emergency_fund_says_when_and_how_low() {
    let sentence = said(
        &Caution::CashDipsBelowEmergencyFund {
            month: CalendarMonth::of(ON).unwrap(),
            currency: Currency::parse("USD").unwrap(),
            projected_cash: usd(-123_456),
        },
        &Names::of_accounts(&[]),
    );
    assert!(sentence.contains("In November 2026"));
    assert!(sentence.contains("fall to -1,234.56 USD"));
    assert!(sentence.contains("below the emergency fund"));
}

#[test]
fn a_missing_emergency_fund_names_its_currency() {
    let sentence = blocker_sentence(&Blocker::EmergencyFundNotSet {
        currency: Currency::parse("EUR").unwrap(),
    });
    assert!(sentence.contains("No emergency fund is set for EUR."));
}

#[test]
fn an_emergency_fund_of_months_at_zero_names_its_currency() {
    let sentence = said(
        &Caution::EmergencyFundMonthsIsZero {
            currency: Currency::parse("EUR").unwrap(),
            months: 3,
        },
        &Names::of_accounts(&[]),
    );
    assert!(sentence.contains("The emergency fund in EUR is 3 months of recurring outflows"));
    assert!(sentence.contains("none are planned in EUR"));
}

#[test]
fn a_window_not_starting_today_gives_both_days() {
    let sentence = said(
        &Caution::AsOfNotToday { today: ON },
        &Names::of_accounts(&[]),
    );
    assert!(sentence.contains("starts on 15 November 2026"));
    assert!(sentence.contains("not today, 7 November 2026"));
    assert!(sentence.contains("Anything due before October 2026"));
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
            items: Vec::new(),
        },
        &names,
    );
    assert!(sentence.contains("Bills or one-offs in Home"));
    assert!(sentence.contains("come to 120.00 USD that no payment"));
    assert!(!sentence.contains("of it"), "no single bill is implied");
}

#[test]
fn a_bill_from_last_month_is_named_with_its_day_and_amount() {
    let id = PlannedItemId::new();
    let item = LineSource::PlannedItem { id };
    let mut names = Names::of_accounts(&[]);
    names.items.insert(id, "Rent".to_owned());
    let unpaid = said(
        &Caution::BillUnpaidLastMonth {
            item,
            on: ON,
            amount: usd(12_000),
        },
        &names,
    );
    assert!(unpaid.starts_with("Rent was due on 7 November 2026"));
    assert!(unpaid.contains("about 120.00 USD"));
    assert!(unpaid.contains(&format!(
        "PUT /api/v1/plan-items/{id}/settlements/2026-11 with {{\"kind\": \"paid\"}}"
    )));
    assert!(unpaid.contains("file it"));

    let unclear = said(
        &Caution::BillPaymentUnclear {
            item,
            on: ON,
            amount: usd(12_000),
            transaction: TransactionId::new(),
            transaction_amount: usd(11_000),
        },
        &names,
    );
    assert!(unclear.contains("A transaction of 110.00 USD"));
    assert!(unclear.contains("the bill still counts as needed"));
}

#[test]
fn a_bill_not_seen_names_its_items() {
    let id = PlannedItemId::new();
    let category = CategoryId::new();
    let mut names = Names::of_accounts(&[]);
    names.items.insert(id, "Rent".to_owned());
    let sentence = said(
        &Caution::BillNotSeen {
            category,
            amount: usd(12_000),
            items: vec![LineSource::PlannedItem { id }],
        },
        &names,
    );
    assert!(sentence.starts_with("Rent in a category dated earlier this month"));
}

#[test]
fn missing_history_says_when_it_starts_and_how_much() {
    let sentence = said(
        &Caution::BillHistoryMissing {
            starts_on: Some(ON),
            currency: Currency::parse("USD").unwrap(),
            occurrences: 8,
            amount: usd(600_000),
        },
        &Names::of_accounts(&[]),
    );
    assert!(sentence.starts_with("Transactions start on 7 November 2026"));
    assert!(sentence.contains("8 bills or one-offs due before then, 6,000.00 USD in all"));

    let without = said(
        &Caution::BillHistoryMissing {
            starts_on: None,
            currency: Currency::parse("USD").unwrap(),
            occurrences: 1,
            amount: usd(1_000),
        },
        &Names::of_accounts(&[]),
    );
    assert!(without.contains("no transactions yet"));
    assert!(without.contains("1 bill or one-off due last month"));
    assert!(!without.contains("before then"), "nothing to refer to");
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
            Caution::TransferLegRemoved {
                link: TransferLinkId::new(),
            },
            "A bank took back one side of a linked transfer",
        ),
        (
            Caution::TransferLinkRateMismatch {
                link: TransferLinkId::new(),
                outflow: usd(-125_000),
                inflow: eur(1_250),
                expected_inflow: eur(111_900),
                rate_on: ON,
                manual_rate: false,
            },
            "A linked transfer took 1,250.00 USD out and brought 12.50 EUR in, but 1,250.00 USD would be about 1,119.00 EUR",
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
                effect: HintEffect::LeftOut,
            },
            "A planned item is expected to arrive in an account, which doesn't count",
        ),
        (
            Caution::BillNotSeen {
                category,
                amount: usd(1),
                items: Vec::new(),
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
