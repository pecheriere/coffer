//! Views and names for the cards' unit tests, built as the services would answer them.

use coffer_domain::ids::{AccountId, CategoryId, MemberId, SplitId, TransactionId};
use coffer_domain::money::{Currency, Money};
use coffer_domain::transactions::{ReviewStatus, TransactionOrigin};
use jiff::Timestamp;
use jiff::civil::date;

use super::Names;
use crate::ledger::transactions::{AttributionView, SplitView, TransactionView};

pub(super) fn usd(minor_units: i64) -> Money {
    Money {
        minor_units,
        currency: Currency::parse("USD").unwrap(),
    }
}

/// A reviewed transaction entered by hand in `account`, of `minor_units` dollars' cents, in no
/// category, link or duplicate.
pub(super) fn transaction(account: AccountId, minor_units: i64) -> TransactionView {
    TransactionView {
        id: TransactionId::new(),
        account,
        amount: usd(minor_units),
        posted_on: date(2026, 10, 2),
        transacted_on: None,
        pending: false,
        description: "Corner shop purchase".to_owned(),
        payee: Some("Corner shop".to_owned()),
        memo: None,
        notes: None,
        review: ReviewStatus::Reviewed,
        assignee: None,
        splits: Vec::new(),
        flags: Vec::new(),
        origin: TransactionOrigin::Manual,
        sources: Vec::new(),
        duplicate_candidates: Vec::new(),
        transfer_link: None,
        reviewed_by: None,
        categorized_by: None,
        assigned_by: None,
        created_at: Timestamp::UNIX_EPOCH,
        updated_at: Timestamp::UNIX_EPOCH,
    }
}

/// `transaction` split in two parts.
pub(super) fn split_in_two(mut transaction: TransactionView) -> TransactionView {
    let half = transaction.amount.minor_units / 2;
    transaction.splits = [half, transaction.amount.minor_units - half]
        .into_iter()
        .map(|minor_units| SplitView {
            id: SplitId::new(),
            category: CategoryId::new(),
            amount: usd(minor_units),
            goal: None,
        })
        .collect();
    transaction
}

/// Who made something: a member, signed in.
pub(super) fn by_member() -> AttributionView {
    AttributionView {
        member: Some(MemberId::new()),
        agent: None,
        system: None,
        rule: None,
        at: Timestamp::UNIX_EPOCH,
    }
}

/// Names for an actor who reads the transactions of `readable` and changes those of `writable`,
/// of accounts with no name the page knows.
pub(super) fn names(readable: &[AccountId], writable: &[AccountId]) -> Names {
    Names {
        accounts: Vec::new(),
        readable: readable.iter().copied().collect(),
        writable: writable.iter().copied().collect(),
        categories: Vec::new(),
        goals: Vec::new(),
        members: Vec::new(),
    }
}
