//! What a token with grants doesn't get of a transaction it can read (spec §8.2,
//! §8.9 R3, §8.12): a duplicate candidate or the other leg of a transfer link in an account it
//! can't read, and a goal link it may not see. A goal's id on a split would confirm a hidden item exists, tell when
//! it was made and which transactions share it, so the split reads as linked to
//! nothing.
//!
//! The link itself is not the token's to lose by not knowing of it, so a patch keeps
//! it: see [`keep_unseen_links`].

use std::collections::{HashMap, HashSet};

use coffer_domain::ids::{AccountId, PlannedItemId, TransactionId};
use coffer_domain::policy::{AccountAccess, AccountLevel, plan_item_visible};
use coffer_domain::transactions::{CategoryKind, SplitInput};
use coffer_store::StoreTx;

use super::transactions::TransactionView;
use super::{Ledger, LedgerError};

impl Ledger {
    /// Takes out of `views` what `access` may not see, for reads.
    pub(super) async fn hide_unseen(
        &self,
        access: &AccountAccess,
        views: &mut [TransactionView],
    ) -> Result<(), LedgerError> {
        if access.sees_every_account(AccountLevel::Transactions) {
            return Ok(());
        }
        let accounts = self
            .store
            .accounts_of_transactions(&candidates_of(views))
            .await?;
        keep_visible_candidates(access, views, &accounts);
        let hints = self.store.hints_of_planned_items(&goals_of(views)).await?;
        keep_visible_goals(access, views, &hints);
        Ok(())
    }
}

/// Takes out of `views` what `access` may not see, inside `tx`, so a write fails
/// before its commit if they cannot be read.
pub(super) async fn hide_unseen_in_tx(
    tx: &mut StoreTx,
    access: &AccountAccess,
    views: &mut [TransactionView],
) -> Result<(), LedgerError> {
    if access.sees_every_account(AccountLevel::Transactions) {
        return Ok(());
    }
    let accounts = tx.accounts_of_transactions(&candidates_of(views)).await?;
    keep_visible_candidates(access, views, &accounts);
    let hints = tx.hints_of_planned_items(&goals_of(views)).await?;
    keep_visible_goals(access, views, &hints);
    Ok(())
}

/// The distinct candidates and other legs of `views`.
///
/// For a candidate this is defence in depth: the server pairs candidates and merges
/// only within one account, so a transaction the token can read should not list one
/// it cannot. Should such a pair ever exist, naming it would reveal a hidden
/// transaction. `possible_duplicate` stays: it says only that a member should look.
///
/// A transfer link is between two accounts by what it is, so here it is the rule.
fn candidates_of(views: &[TransactionView]) -> Vec<TransactionId> {
    let distinct: HashSet<TransactionId> = views
        .iter()
        .flat_map(|view| {
            view.duplicate_candidates
                .iter()
                .copied()
                .chain(view.transfer_link.map(|link| link.other_transaction))
        })
        .collect();
    distinct.into_iter().collect()
}

fn keep_visible_candidates(
    access: &AccountAccess,
    views: &mut [TransactionView],
    accounts: &HashMap<TransactionId, AccountId>,
) {
    let readable = |transaction: &TransactionId| {
        accounts
            .get(transaction)
            .is_some_and(|account| access.level_for(*account) >= Some(AccountLevel::Transactions))
    };
    for view in views {
        view.duplicate_candidates.retain(&readable);
        view.transfer_link = view
            .transfer_link
            .filter(|link| readable(&link.other_transaction));
    }
}

/// The distinct goals the splits of `views` link to.
fn goals_of(views: &[TransactionView]) -> Vec<PlannedItemId> {
    let distinct: HashSet<PlannedItemId> = views
        .iter()
        .flat_map(|view| view.splits.iter().filter_map(|split| split.goal))
        .collect();
    distinct.into_iter().collect()
}

fn keep_visible_goals(
    access: &AccountAccess,
    views: &mut [TransactionView],
    hints: &HashMap<PlannedItemId, Option<AccountId>>,
) {
    for split in views.iter_mut().flat_map(|view| view.splits.iter_mut()) {
        split.goal = split.goal.filter(|goal| goal_visible(access, hints, *goal));
    }
}

/// Whether `access` may see `goal`. One not in `hints` is hidden, which fails closed.
fn goal_visible(
    access: &AccountAccess,
    hints: &HashMap<PlannedItemId, Option<AccountId>>,
    goal: PlannedItemId,
) -> bool {
    hints
        .get(&goal)
        .is_some_and(|hint| plan_item_visible(access, *hint))
}

/// `splits` as `access` reads them: without the links it may not see.
pub(super) async fn splits_as_shown(
    tx: &mut StoreTx,
    access: &AccountAccess,
    splits: &[SplitInput],
) -> Result<Vec<SplitInput>, LedgerError> {
    let mut shown = splits.to_vec();
    if access.sees_every_account(AccountLevel::Transactions) {
        return Ok(shown);
    }
    let goals: Vec<PlannedItemId> = splits.iter().filter_map(|split| split.goal).collect();
    let hints = tx.hints_of_planned_items(&goals).await?;
    for split in &mut shown {
        split.goal = split
            .goal
            .filter(|goal| goal_visible(access, &hints, *goal));
    }
    Ok(shown)
}

/// `sent`, a patch's splits, with the links `access` can't see back on them, since a
/// split sent without a `goal` is all a token not shown one can send. Refusing would
/// say a link is there, and dropping it would move a goal's figures unseen.
///
/// `before` is what the transaction holds and `shown` the same as the token reads it,
/// which `sent` differs from. A link goes back on a split sent with no goal of the
/// same category and amount, or failing that of the same amount in another `expense`
/// category, the only kind a link may be on. With neither left it is gone, as
/// money divided differently no longer says what counted towards the goal; the audit
/// log records that.
pub(super) async fn keep_unseen_links(
    tx: &mut StoreTx,
    before: &[SplitInput],
    shown: &[SplitInput],
    mut sent: Vec<SplitInput>,
) -> Result<Vec<SplitInput>, LedgerError> {
    let mut unplaced = Vec::new();
    for link in before
        .iter()
        .zip(shown)
        .filter_map(|(held, seen)| (held.goal != seen.goal).then_some(held))
    {
        match sent.iter_mut().find(|split| {
            split.goal.is_none()
                && split.category == link.category
                && split.minor_units == link.minor_units
        }) {
            Some(split) => split.goal = link.goal,
            None => unplaced.push(link),
        }
    }
    for link in unplaced {
        for split in &mut sent {
            if split.goal.is_none()
                && split.minor_units == link.minor_units
                && tx.get_category(split.category).await?.kind == CategoryKind::Expense
            {
                split.goal = link.goal;
                break;
            }
        }
    }
    Ok(sent)
}
