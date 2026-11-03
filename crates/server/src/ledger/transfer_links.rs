//! Transfer links (spec §8.12): saying that two transactions on two accounts are one movement
//! of money. Never made by itself: [`Ledger::transfer_candidates`] suggests, and a member or
//! agent confirms. Each link needs `Write` on both accounts, and with one side hidden it
//! answers as missing, and is left out of whatever else mentions it. What it does to a report
//! or the forecast is read there, from the store.

use std::cmp::Reverse;

use coffer_domain::audit::AuditChanges;
use coffer_domain::identity::Actor;
use coffer_domain::ids::{AccountId, TransactionId, TransferLinkId};
use coffer_domain::money::Money;
use coffer_domain::policy::{AccountAccess, AccountLevel, authorize_account};
use coffer_domain::transactions::{SplitInput, TransactionFlag};
use coffer_store::{
    NewTransferLink, PageRequest, StoreTx, TransactionFilter, TransactionRecord,
    TransferLinkRecord, TransferLinkSide,
};
use jiff::civil::Date;
use jiff::{Span, Timestamp};
use serde::Serialize;
use utoipa::ToSchema;

use super::transactions::{AttributionView, TransactionView, attribution, current_splits};
use super::unseen::splits_as_shown;
use super::{Ledger, LedgerError, audit, complete_created};
use crate::http::idempotency::IdempotencySlot;
use crate::http::problem::ProblemCode;

/// How many days either side of a transaction a candidate may be posted: a bank transfer can
/// take a few working days, and a weekend.
const CANDIDATE_WINDOW_DAYS: i64 = 5;
/// The transactions of the same amount in the window that are looked through. A household
/// doesn't make more of them.
const CANDIDATES_READ: u32 = 200;

const SAME_ACCOUNT: &str = "a transfer link joins transactions on two different accounts";
const OUTFLOW_SIGN: &str = "`outflow` must be money out of its account";
const INFLOW_SIGN: &str = "`inflow` must be money into its account";
const SPLIT_LEG: &str = "a transaction with more than one split can't be one leg of a transfer link: part of it may \
     be spending";
const GOAL_LEG: &str =
    "money moved between two counted accounts can't count towards a goal: it is still cash";
const ALREADY_LINKED: &str = "a transaction is already one leg of a transfer link";
const SPLIT_LINKED: &str = "a leg of a transfer link keeps one split at most: remove the link \
                            first";
const GOAL_LINKED: &str = "this leg of a transfer link can't count towards a goal: the money is \
                           still cash";
const DELETE_LINKED: &str = "the transaction is one leg of a transfer link: remove the link first";
/// What a token that doesn't see a link is told, in no words of one (spec §8.2).
const NOT_WITH_THIS_ACCESS: &str = "this change can't be made to this transaction with this access";
/// The same for a merge, whose conflict otherwise tells the caller to discard.
pub(super) const MERGE_NOT_WITH_THIS_ACCESS: &str =
    "the duplicate holds what this access can't discard";

/// One leg of a link, as far as it matters to the link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct TransferLegView {
    pub transaction: TransactionId,
    pub account: AccountId,
    /// Negative is money out of the account.
    pub amount: Money,
    /// The calendar day it posted (ISO 8601).
    pub posted_on: Date,
    pub pending: bool,
    /// `source_removed` says its provider took it back, which ends nothing: a person decides
    /// whether the link still stands.
    pub flags: Vec<TransactionFlag>,
}

impl TransferLegView {
    fn new(record: &TransactionRecord) -> Self {
        Self {
            transaction: record.id,
            account: record.account,
            amount: Money {
                minor_units: record.amount_minor,
                currency: record.currency,
            },
            posted_on: record.posted_on,
            pending: record.pending,
            flags: record.flags.clone(),
        }
    }
}

/// Two transactions that are one movement of money.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct TransferLinkView {
    pub id: TransferLinkId,
    pub outflow: TransferLegView,
    pub inflow: TransferLegView,
    /// What arrived less what left, negative when less arrived, such as for a fee; `null` for
    /// legs in two currencies, since a link doesn't read an exchange rate.
    #[schema(required)]
    pub difference: Option<Money>,
    /// Both accounts are in counted cash (open, switched into the forecast, of a spendable
    /// class) and neither leg was taken back by its provider, so what moved is one movement in
    /// the forecast, whatever it is filed as. `false` is any other link, which records the
    /// movement and changes no number. It follows the accounts.
    pub one_movement: bool,
    /// The same as reports read it, for what happened: an account closed since its leg posted
    /// still counts here, so closing one doesn't rewrite its past months.
    pub one_movement_in_reports: bool,
    pub created_by: AttributionView,
}

impl TransferLinkView {
    fn new(
        link: &TransferLinkRecord,
        outflow: &TransactionRecord,
        inflow: &TransactionRecord,
    ) -> Self {
        Self {
            id: link.id,
            outflow: TransferLegView::new(outflow),
            inflow: TransferLegView::new(inflow),
            // Opposite signs can't overflow, but a provider may have revised one since.
            difference: (outflow.currency == inflow.currency)
                .then(|| outflow.amount_minor.checked_add(inflow.amount_minor))
                .flatten()
                .map(|minor_units| Money {
                    minor_units,
                    currency: outflow.currency,
                }),
            one_movement: link.one_movement,
            one_movement_in_reports: link.one_movement_in_reports,
            created_by: AttributionView::new(&link.created),
        }
    }
}

/// Transactions that might be the other leg, nearest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct TransferCandidates {
    pub items: Vec<TransactionView>,
}

/// Refuses a link that would be one movement, on legs where one has a goal. A token not shown
/// that goal is refused in words that don't say there is one (spec §8.9 R3).
async fn refuse_goal_on_movement(
    tx: &mut StoreTx,
    access: &AccountAccess,
    outflow: &TransactionRecord,
    inflow: &TransactionRecord,
) -> Result<(), LedgerError> {
    let has_goal = |splits: &[SplitInput]| splits.iter().any(|split| split.goal.is_some());
    let (out_splits, in_splits) = (current_splits(outflow), current_splits(inflow));
    if !(has_goal(&out_splits) || has_goal(&in_splits))
        || !tx.would_be_one_movement(outflow.id, inflow.id).await?
    {
        return Ok(());
    }
    let shown = has_goal(&splits_as_shown(tx, access, &out_splits).await?)
        || has_goal(&splits_as_shown(tx, access, &in_splits).await?);
    Err(LedgerError::Invalid(if shown {
        GOAL_LEG
    } else {
        NOT_WITH_THIS_ACCESS
    }))
}

/// `shown` if `access` can read the other leg of `side`, and otherwise words that don't tell
/// the token that a link is there.
async fn link_words(
    tx: &mut StoreTx,
    access: &AccountAccess,
    side: &TransferLinkSide,
    shown: &'static str,
) -> Result<&'static str, LedgerError> {
    let other = tx.get_transaction(side.other).await?;
    Ok(
        if access.level_for(other.account) >= Some(AccountLevel::Transactions) {
            shown
        } else {
            NOT_WITH_THIS_ACCESS
        },
    )
}

/// Refuses `splits` a leg can't hold: more than one, or, on a link that is one movement, a goal.
/// Any other link's money left counted cash, so its goal stays allowed (spec §8.11 B10).
pub(super) async fn check_splits_on_leg(
    tx: &mut StoreTx,
    access: &AccountAccess,
    side: &TransferLinkSide,
    splits: &[SplitInput],
) -> Result<(), LedgerError> {
    let refused = if splits.len() > 1 {
        SPLIT_LINKED
    } else if side.one_movement_in_reports && splits.iter().any(|split| split.goal.is_some()) {
        GOAL_LINKED
    } else {
        return Ok(());
    };
    Err(LedgerError::Invalid(
        link_words(tx, access, side, refused).await?,
    ))
}

/// Refuses to delete a leg: a person ends the link first.
pub(super) async fn refuse_deleting_leg(
    tx: &mut StoreTx,
    access: &AccountAccess,
    side: &TransferLinkSide,
) -> LedgerError {
    match link_words(tx, access, side, DELETE_LINKED).await {
        Ok(words) => LedgerError::Conflict(ProblemCode::Conflict, words),
        Err(error) => error,
    }
}

/// Audits the end of `link`, adding `changes` to what identifies it.
async fn audit_removal(
    tx: &mut StoreTx,
    now: Timestamp,
    actor: &Actor,
    link: &TransferLinkRecord,
    changes: AuditChanges,
) -> Result<(), LedgerError> {
    audit(
        tx,
        now,
        actor,
        "transfer_link.remove",
        "transfer_link",
        link.id,
        changes
            .unset("outflow", link.outflow)
            .unset("inflow", link.inflow),
    )
    .await
}

/// Ends the link on `duplicate`, whose merge into `target` was told to discard it. It needs the
/// same `Write` on both accounts as removing it would, and a token without that gets the merge's
/// conflict, whatever else it is told to discard.
pub(super) async fn discard_in_merge(
    tx: &mut StoreTx,
    now: Timestamp,
    actor: &Actor,
    access: &AccountAccess,
    side: &TransferLinkSide,
    target: TransactionId,
) -> Result<(), LedgerError> {
    let other = tx.get_transaction(side.other).await?;
    if authorize_account(actor, access, other.account, AccountLevel::Write).is_err() {
        return Err(LedgerError::Conflict(
            ProblemCode::MergeConflict,
            MERGE_NOT_WITH_THIS_ACCESS,
        ));
    }
    let link = tx.get_transfer_link(side.id).await?;
    tx.delete_transfer_link(link.id).await?;
    audit_removal(
        tx,
        now,
        actor,
        &link,
        AuditChanges::new().set("merged_into", target),
    )
    .await
}

/// A link with a side `access` can't read doesn't exist for it, whichever side that is, so a
/// refusal about the other side can't say otherwise.
fn hidden_is_missing(access: &AccountAccess, accounts: [AccountId; 2]) -> Result<(), LedgerError> {
    if accounts
        .iter()
        .all(|account| access.level_for(*account) >= Some(AccountLevel::Transactions))
    {
        Ok(())
    } else {
        Err(LedgerError::NotFound)
    }
}

/// Whether `candidate` could be linked with `record`, an unlinked transaction of at most one
/// split, its account and currency apart. See the filter it comes from for the rest.
fn could_be_other_leg(record: &TransactionRecord, candidate: &TransactionRecord) -> bool {
    candidate.account != record.account
        && candidate.currency == record.currency
        && candidate.transfer_link.is_none()
        && candidate.splits.len() <= 1
}

impl Ledger {
    /// Links two transactions. With `slot`, the idempotency key is completed with the response
    /// in the same transaction.
    ///
    /// # Errors
    ///
    /// [`LedgerError::NotFound`] for an unknown transaction or one in a hidden account;
    /// [`LedgerError::Denied`] without write access to either account;
    /// [`LedgerError::Invalid`] for legs that can't be joined: on one account, not out and in,
    /// split in more than one, or, between two counted accounts, linked to a goal;
    /// [`LedgerError::Conflict`] when a leg is already in a link; storage failures.
    pub async fn create_transfer_link(
        &self,
        actor: &Actor,
        outflow: TransactionId,
        inflow: TransactionId,
        slot: Option<IdempotencySlot>,
    ) -> Result<TransferLinkView, LedgerError> {
        let access = self.access.resolve(actor).await?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let outflow = tx.get_transaction(outflow).await?;
        authorize_account(actor, &access, outflow.account, AccountLevel::Write)?;
        let inflow = tx.get_transaction(inflow).await?;
        authorize_account(actor, &access, inflow.account, AccountLevel::Write)?;
        if outflow.account == inflow.account {
            return Err(LedgerError::Invalid(SAME_ACCOUNT));
        }
        if outflow.amount_minor >= 0 {
            return Err(LedgerError::Invalid(OUTFLOW_SIGN));
        }
        if inflow.amount_minor <= 0 {
            return Err(LedgerError::Invalid(INFLOW_SIGN));
        }
        for leg in [&outflow, &inflow] {
            if leg.splits.len() > 1 {
                return Err(LedgerError::Invalid(SPLIT_LEG));
            }
            if let Some(side) = &leg.transfer_link {
                let words = link_words(&mut tx, &access, side, ALREADY_LINKED).await?;
                return Err(LedgerError::Conflict(ProblemCode::Conflict, words));
            }
        }
        refuse_goal_on_movement(&mut tx, &access, &outflow, &inflow).await?;
        let link = tx
            .insert_transfer_link(NewTransferLink {
                outflow: outflow.id,
                inflow: inflow.id,
                by: attribution(actor, now),
            })
            .await?;
        let view = TransferLinkView::new(&link, &outflow, &inflow);
        audit(
            &mut tx,
            now,
            actor,
            "transfer_link.create",
            "transfer_link",
            link.id,
            AuditChanges::new()
                .set("outflow", outflow.id)
                .set("outflow_account", outflow.account)
                .set("outflow_amount_minor", outflow.amount_minor)
                .set("inflow", inflow.id)
                .set("inflow_account", inflow.account)
                .set("inflow_amount_minor", inflow.amount_minor)
                .set("one_movement", link.one_movement),
        )
        .await?;
        complete_created(&mut tx, slot, &view).await?;
        tx.commit().await?;
        Ok(view)
    }

    /// Ends a link. Its transactions stay, each with its category.
    ///
    /// # Errors
    ///
    /// [`LedgerError::NotFound`] for an unknown link or one with a side in a hidden account;
    /// [`LedgerError::Denied`] without write access to either account; storage failures.
    pub async fn remove_transfer_link(
        &self,
        actor: &Actor,
        id: TransferLinkId,
    ) -> Result<(), LedgerError> {
        let access = self.access.resolve(actor).await?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let link = tx.get_transfer_link(id).await?;
        let outflow = tx.get_transaction(link.outflow).await?;
        let inflow = tx.get_transaction(link.inflow).await?;
        let accounts = [outflow.account, inflow.account];
        hidden_is_missing(&access, accounts)?;
        for account in accounts {
            authorize_account(actor, &access, account, AccountLevel::Write)?;
        }
        tx.delete_transfer_link(link.id).await?;
        audit_removal(&mut tx, now, actor, &link, AuditChanges::new()).await?;
        tx.commit().await?;
        Ok(())
    }

    /// One link.
    ///
    /// # Errors
    ///
    /// [`LedgerError::NotFound`] for an unknown link or one with a side in a hidden account;
    /// [`LedgerError::Denied`] without transaction access to either account; storage failures.
    pub async fn get_transfer_link(
        &self,
        actor: &Actor,
        id: TransferLinkId,
    ) -> Result<TransferLinkView, LedgerError> {
        let access = self.access.resolve(actor).await?;
        let link = self.store.get_transfer_link(id).await?;
        let outflow = self.store.get_transaction(link.outflow).await?;
        let inflow = self.store.get_transaction(link.inflow).await?;
        let accounts = [outflow.account, inflow.account];
        hidden_is_missing(&access, accounts)?;
        for account in accounts {
            authorize_account(actor, &access, account, AccountLevel::Transactions)?;
        }
        Ok(TransferLinkView::new(&link, &outflow, &inflow))
    }

    /// What might be the other leg of `transaction`: the same amount the other way, in the same
    /// currency, on another account the actor can read, posted within
    /// five days, in no link and split at most once, nearest first. Nothing
    /// for a transaction that can't be linked itself. It suggests and changes nothing.
    ///
    /// # Errors
    ///
    /// [`LedgerError::NotFound`] for an unknown transaction or one in a hidden account;
    /// [`LedgerError::Denied`] without transaction access to its account; storage failures.
    pub async fn transfer_candidates(
        &self,
        actor: &Actor,
        transaction: TransactionId,
    ) -> Result<TransferCandidates, LedgerError> {
        let access = self.access.resolve(actor).await?;
        let record = self.store.get_transaction(transaction).await?;
        authorize_account(actor, &access, record.account, AccountLevel::Transactions)?;
        let opposite = record.amount_minor.checked_neg();
        let (Some(opposite), None, true) = (
            opposite.filter(|amount| *amount != 0),
            &record.transfer_link,
            record.splits.len() <= 1,
        ) else {
            return Ok(TransferCandidates { items: Vec::new() });
        };
        let window = Span::new().days(CANDIDATE_WINDOW_DAYS);
        // At the calendar's edge, a bound that doesn't exist bounds nothing.
        let from = record.posted_on.checked_sub(window).ok();
        let to = record
            .posted_on
            .checked_add(window)
            .and_then(Date::tomorrow)
            .ok();
        let filter = TransactionFilter {
            scope: access.scope(AccountLevel::Transactions),
            accounts: Vec::new(),
            from,
            to,
            review: None,
            assignee: None,
            categories: Vec::new(),
            uncategorized: false,
            unmatched_transfer: false,
            pending: None,
            flag: None,
            min_minor: Some(opposite),
            max_minor: Some(opposite),
            text: None,
        };
        let page = PageRequest {
            after: None,
            limit: CANDIDATES_READ,
        };
        let mut found = self.store.list_transactions(&filter, &page).await?.items;
        found.retain(|candidate| could_be_other_leg(&record, candidate));
        // Nearest first, then the later of two as far apart, then the newer.
        found.sort_by_key(|candidate| {
            let apart = (candidate.posted_on - record.posted_on).get_days().abs();
            (apart, Reverse(candidate.posted_on), Reverse(candidate.id))
        });
        let mut items: Vec<TransactionView> = found.iter().map(TransactionView::new).collect();
        self.hide_unseen(&access, &mut items).await?;
        Ok(TransferCandidates { items })
    }
}
