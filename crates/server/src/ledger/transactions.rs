//! Transactions: creating and deleting manual ones, and the fields members
//! and agents set on any — splits, payee, notes, review and assignee — with
//! who categorized and who reviewed.

use std::collections::{HashMap, HashSet};

use coffer_domain::audit::{AuditChanges, AuditValue};
use coffer_domain::identity::{Actor, Via};
use coffer_domain::ids::{
    AccountId, AgentTokenId, CategoryId, ConnectionId, MemberId, ProviderAccountId, SourceRecordId,
    SplitId, TransactionId,
};
use coffer_domain::money::Money;
use coffer_domain::policy::{AccountAccess, AccountLevel, authorize_account};
use coffer_domain::transactions::{
    Description, Memo, Notes, Payee, ReviewStatus, SplitError, SplitInput, TransactionFlag,
    TransactionOrigin, validate_splits,
};
use coffer_store::{
    AssigneeFilter, Attribution, NewTransaction, PageRequest, PayeeChange, ReviewChange,
    SourceRecord, StoreError, StoreTx, TransactionFilter, TransactionRecord, UserFieldChanges,
};
use jiff::Timestamp;
use jiff::civil::Date;
use serde::{Deserialize, Deserializer, Serialize};
use utoipa::ToSchema;

use super::{Ledger, LedgerError, audit, complete_created};
use crate::http::idempotency::IdempotencySlot;
use crate::http::problem::ProblemCode;

const PAYEE_INVALID: &str = "payee must be 1 to 200 characters without control characters";
const DESCRIPTION_INVALID: &str =
    "description must be 1 to 500 characters without control characters";
const MEMO_INVALID: &str = "memo must be 1 to 500 characters without control characters";
const NOTES_INVALID: &str =
    "notes must be at most 2000 characters without control characters other than newlines";
const ASSIGNEE_INVALID: &str = "the assignee must be an active member";
const UNKNOWN_CATEGORY: &str = "a split names a category that does not exist";
const ARCHIVED_CATEGORY: &str = "a split names an archived category";
const ACCOUNT_CLOSED: &str = "the account is closed";
const NOT_MANUAL: &str = "only manual transactions can be deleted";
const ACCOUNT_LINKED: &str =
    "the account is linked to a provider: its transactions come from syncs only";
const BATCH_SIZE_INVALID: &str = "a batch has 1 to 200 operations";
const BATCH_REPEATED: &str = "each operation of a batch names a different transaction";

/// The most operations one batch may hold.
pub const MAX_BATCH_OPERATIONS: usize = 200;

/// A field of a patch: absent (left as it is), `null` (cleared) or a value
/// (set).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Patch<T> {
    /// The field was not sent.
    #[default]
    Absent,
    /// The field was sent as `null`.
    Clear,
    /// The field was sent with a value.
    Set(T),
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Patch<T> {
    /// Called only for a field that is present (absent fields take
    /// [`Patch::Absent`] through `#[serde(default)]`).
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Option::<T>::deserialize(deserializer)?.map_or(Self::Clear, Self::Set))
    }
}

/// Changes to a transaction's user fields. An absent field stays as it is.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TransactionPatch {
    /// 1 to 200 characters, without control characters; from then on a
    /// sync no longer changes it. `null` restores the payee the source
    /// gave.
    #[serde(default)]
    #[schema(value_type = Option<String>)]
    pub payee: Patch<String>,
    /// At most 2000 characters; newlines are the only control characters
    /// allowed. `null` or `""` clears them.
    #[serde(default)]
    #[schema(value_type = Option<String>)]
    pub notes: Patch<String>,
    /// `reviewed` records who reviewed it (unless it already was);
    /// `unreviewed` forgets the reviewer.
    pub review: Option<ReviewStatus>,
    /// An active member; `null` unassigns.
    #[serde(default)]
    #[schema(value_type = Option<MemberId>)]
    pub assignee: Patch<MemberId>,
    /// Replaces every split and records who categorized it: `[]`
    /// uncategorizes, otherwise 1 to 50 non-zero splits on categories that
    /// are not archived, summing exactly to the amount.
    pub splits: Option<Vec<SplitInput>>,
}

/// Who made a change: a member, possibly through one of their agent
/// tokens, or the system with its reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct AttributionView {
    #[schema(required)]
    pub member: Option<MemberId>,
    /// The agent token the member acted through; `null` when signed in.
    #[schema(required)]
    pub agent: Option<AgentTokenId>,
    /// The system's reason, such as `system:sync`; `null` for a member.
    #[schema(required)]
    pub system: Option<String>,
    pub at: Timestamp,
}

impl AttributionView {
    fn new(attribution: &Attribution) -> Self {
        Self {
            member: attribution.member,
            agent: attribution.agent,
            system: attribution.system.clone(),
            at: attribution.at,
        }
    }
}

/// One categorized part of a transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct SplitView {
    pub id: SplitId,
    pub category: CategoryId,
    pub amount: Money,
}

/// A provider record a transaction was imported from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct SourceView {
    pub id: SourceRecordId,
    pub connection: ConnectionId,
    pub provider_account: ProviderAccountId,
    pub pending: bool,
    /// The day the provider says it posted (ISO 8601).
    pub posted_on: Date,
    /// The amount exactly as the provider wrote it, in its own sign.
    pub amount_raw: String,
    pub first_seen_at: Timestamp,
}

impl SourceView {
    fn new(source: &SourceRecord) -> Self {
        Self {
            id: source.id,
            connection: source.connection,
            provider_account: source.provider_account,
            pending: source.fields.pending,
            posted_on: source.fields.posted_on,
            amount_raw: source.amount_raw.clone(),
            first_seen_at: source.first_seen_at,
        }
    }
}

/// A transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct TransactionView {
    pub id: TransactionId,
    pub account: AccountId,
    /// Negative is money out of the account.
    pub amount: Money,
    /// The calendar day it posted (ISO 8601).
    pub posted_on: Date,
    /// The day it was made, when the source says (ISO 8601).
    #[schema(required)]
    pub transacted_on: Option<Date>,
    pub pending: bool,
    /// The description as its source gave it.
    pub description: String,
    #[schema(required)]
    pub payee: Option<String>,
    #[schema(required)]
    pub memo: Option<String>,
    #[schema(required)]
    pub notes: Option<String>,
    pub review: ReviewStatus,
    #[schema(required)]
    pub assignee: Option<MemberId>,
    /// In order; empty when uncategorized.
    pub splits: Vec<SplitView>,
    pub flags: Vec<TransactionFlag>,
    pub origin: TransactionOrigin,
    /// The provider records it was imported from, oldest posting day
    /// first; empty for a manual transaction.
    pub sources: Vec<SourceView>,
    /// The transactions it may duplicate while flagged
    /// `possible_duplicate`.
    pub duplicate_candidates: Vec<TransactionId>,
    /// Who marked it reviewed; `null` while unreviewed.
    #[schema(required)]
    pub reviewed_by: Option<AttributionView>,
    /// Who last changed its splits; `null` if nobody has.
    #[schema(required)]
    pub categorized_by: Option<AttributionView>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl TransactionView {
    pub(super) fn new(record: &TransactionRecord) -> Self {
        let money = |minor_units| Money {
            minor_units,
            currency: record.currency,
        };
        Self {
            id: record.id,
            account: record.account,
            amount: money(record.amount_minor),
            posted_on: record.posted_on,
            transacted_on: record.transacted_on,
            pending: record.pending,
            description: record.raw_description.as_str().to_owned(),
            payee: record.payee.as_ref().map(|p| p.as_str().to_owned()),
            memo: record.memo.as_ref().map(|m| m.as_str().to_owned()),
            notes: record.notes.as_ref().map(|n| n.as_str().to_owned()),
            review: record.review,
            assignee: record.assignee,
            splits: record
                .splits
                .iter()
                .map(|split| SplitView {
                    id: split.id,
                    category: split.category,
                    amount: money(split.minor_units),
                })
                .collect(),
            flags: record.flags.clone(),
            origin: record.origin,
            sources: record.sources.iter().map(SourceView::new).collect(),
            duplicate_candidates: record.duplicate_candidates.clone(),
            reviewed_by: record.reviewed.as_ref().map(AttributionView::new),
            categorized_by: record.categorized.as_ref().map(AttributionView::new),
            created_at: record.created_at,
            updated_at: record.updated_at,
        }
    }
}

/// One page of transactions, newest posting day first.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TransactionPage {
    pub items: Vec<TransactionView>,
    /// Pass as `after` for the next page; `null` on the last page.
    #[schema(required)]
    pub next: Option<String>,
}

/// Which transactions to list, and which page. Every criterion set must
/// hold; an empty list or `None` does not filter.
#[derive(Debug, Clone)]
pub struct TransactionQueryInput {
    /// Only these accounts; each must be visible to the actor.
    pub accounts: Vec<AccountId>,
    /// Posted on or after this day.
    pub from: Option<Date>,
    /// Posted before this day.
    pub to: Option<Date>,
    pub review: Option<ReviewStatus>,
    pub assignee: Option<AssigneeFilter>,
    /// With a split in at least one of these categories.
    pub categories: Vec<CategoryId>,
    /// Only uncategorized transactions; `false` does not filter.
    pub uncategorized: bool,
    pub pending: Option<bool>,
    pub flag: Option<TransactionFlag>,
    /// A signed amount of at least this many minor units.
    pub min_minor: Option<i64>,
    /// A signed amount of at most this many minor units.
    pub max_minor: Option<i64>,
    /// Text contained in the description, payee or notes.
    pub text: Option<String>,
    pub page: PageRequest,
}

/// One change of a batch: a patch to one transaction.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct BatchOperation {
    /// The transaction to change.
    pub id: TransactionId,
    pub patch: TransactionPatch,
}

/// A manual transaction to create.
#[derive(Debug, Clone)]
pub struct NewManualTransactionInput {
    pub account: AccountId,
    pub posted_on: Date,
    /// In the account currency's minor units; negative is money out.
    pub minor_units: i64,
    pub description: String,
    pub payee: Option<String>,
    pub memo: Option<String>,
    pub notes: Option<String>,
}

fn split_message(err: SplitError) -> &'static str {
    match err {
        SplitError::TooMany => "a transaction has at most 50 splits",
        SplitError::ZeroSplit => "a split amount cannot be zero",
        SplitError::SumMismatch => "the splits must sum exactly to the transaction amount",
    }
}

fn payee(input: &str) -> Result<Payee, LedgerError> {
    Payee::parse(input).map_err(|_| LedgerError::Invalid(PAYEE_INVALID))
}

/// Notes as stored: empty notes are none.
fn notes(input: &str) -> Result<Option<Notes>, LedgerError> {
    let notes = Notes::parse(input).map_err(|_| LedgerError::Invalid(NOTES_INVALID))?;
    Ok((!notes.is_empty()).then_some(notes))
}

pub(super) fn attribution(actor: &Actor, at: Timestamp) -> Attribution {
    Attribution {
        member: Some(actor.member),
        agent: match actor.via {
            Via::Session => None,
            Via::Agent { token, .. } => Some(token),
        },
        system: None,
        at,
    }
}

/// The splits as an audited value: their JSON array.
pub(super) fn splits_value(splits: &[SplitInput]) -> Result<AuditValue, LedgerError> {
    serde_json::to_string(splits)
        .map(AuditValue::Text)
        .map_err(|_| LedgerError::Internal("split serialization failed"))
}

pub(super) fn current_splits(record: &TransactionRecord) -> Vec<SplitInput> {
    record
        .splits
        .iter()
        .map(|split| SplitInput {
            category: split.category,
            minor_units: split.minor_units,
        })
        .collect()
}

/// Refuses an assignee that does not exist or is disabled.
async fn check_assignee(tx: &mut StoreTx, member: MemberId) -> Result<(), LedgerError> {
    match tx.get_member(member).await {
        Ok(found) if found.disabled_at.is_none() => Ok(()),
        Ok(_) | Err(StoreError::NotFound) => Err(LedgerError::Invalid(ASSIGNEE_INVALID)),
        Err(other) => Err(other.into()),
    }
}

/// Refuses a split on a category that does not exist, is archived, or is
/// in an archived group.
async fn check_categories(tx: &mut StoreTx, splits: &[SplitInput]) -> Result<(), LedgerError> {
    for split in splits {
        let category = match tx.get_category(split.category).await {
            Ok(category) => category,
            Err(StoreError::NotFound) => return Err(LedgerError::Invalid(UNKNOWN_CATEGORY)),
            Err(other) => return Err(other.into()),
        };
        let group = tx.get_category_group(category.group).await?;
        if category.archived_at.is_some() || group.archived_at.is_some() {
            return Err(LedgerError::Invalid(ARCHIVED_CATEGORY));
        }
    }
    Ok(())
}

/// The payee, notes and review changes that differ from `current`, and
/// their audit record.
fn text_and_review_changes(
    current: &TransactionRecord,
    new_payee: Patch<Payee>,
    new_notes: Patch<Notes>,
    review: Option<ReviewStatus>,
    actor: &Actor,
    now: Timestamp,
) -> (UserFieldChanges, AuditChanges) {
    let mut changes = UserFieldChanges::new(now);
    let mut audited = AuditChanges::new();
    let shown_payee = current.payee.as_ref().map(Payee::as_str);
    match new_payee {
        Patch::Set(chosen)
            if !(current.payee_edited && current.payee.as_ref() == Some(&chosen)) =>
        {
            audited = audited.change("payee", shown_payee, chosen.as_str());
            if !current.payee_edited {
                audited = audited.change("payee_edited", false, true);
            }
            changes.payee = Some(PayeeChange::Set(chosen));
        }
        Patch::Clear if current.payee_edited => {
            audited = audited
                .change(
                    "payee",
                    shown_payee,
                    current.imported_payee.as_ref().map(Payee::as_str),
                )
                .change("payee_edited", true, false);
            changes.payee = Some(PayeeChange::RestoreImported);
        }
        _ => {}
    }
    let target_notes = match new_notes {
        Patch::Absent => None,
        Patch::Clear => Some(None),
        Patch::Set(notes) => Some(Some(notes)),
    };
    if let Some(notes) = target_notes.filter(|notes| *notes != current.notes) {
        audited = audited.change(
            "notes",
            current.notes.as_ref().map(Notes::as_str),
            notes.as_ref().map(Notes::as_str),
        );
        changes.notes = Some(notes);
    }
    if let Some(review) = review.filter(|review| *review != current.review) {
        audited = audited.change("review", current.review.as_str(), review.as_str());
        changes.review = Some(match review {
            ReviewStatus::Reviewed => ReviewChange::Reviewed(attribution(actor, now)),
            ReviewStatus::Unreviewed => ReviewChange::Unreviewed,
        });
    }
    (changes, audited)
}

/// Applies `patch` to transaction `id` inside `tx`, writing only the
/// fields that change and auditing them; a patch that changes nothing
/// writes nothing. Returns the transaction as it now is.
async fn apply_patch(
    tx: &mut StoreTx,
    actor: &Actor,
    access: &AccountAccess,
    now: Timestamp,
    id: TransactionId,
    patch: TransactionPatch,
) -> Result<TransactionRecord, LedgerError> {
    let current = tx.get_transaction(id).await?;
    authorize_account(actor, access, current.account, AccountLevel::Write)?;
    let new_payee = match &patch.payee {
        Patch::Absent => Patch::Absent,
        Patch::Clear => Patch::Clear,
        Patch::Set(value) => Patch::Set(payee(value)?),
    };
    // Empty notes clear them.
    let new_notes = match &patch.notes {
        Patch::Absent => Patch::Absent,
        Patch::Clear => Patch::Clear,
        Patch::Set(value) => notes(value)?.map_or(Patch::Clear, Patch::Set),
    };

    let (mut changes, mut audited) =
        text_and_review_changes(&current, new_payee, new_notes, patch.review, actor, now);
    let new_assignee = match patch.assignee {
        Patch::Absent => None,
        Patch::Clear => Some(None),
        Patch::Set(member) => {
            check_assignee(tx, member).await?;
            Some(Some(member))
        }
    };
    if let Some(assignee) = new_assignee.filter(|assignee| *assignee != current.assignee) {
        audited = audited.change("assignee", current.assignee, assignee);
        changes.assignee = Some(assignee);
    }
    let mut new_splits = None;
    if let Some(splits) = patch.splits {
        validate_splits(current.amount_minor, &splits)
            .map_err(|err| LedgerError::Invalid(split_message(err)))?;
        let before = current_splits(&current);
        if splits != before {
            check_categories(tx, &splits).await?;
            audited = audited.change("splits", splits_value(&before)?, splits_value(&splits)?);
            if current.flags.contains(&TransactionFlag::SplitMismatch) {
                audited = audited.change("split_mismatch", true, false);
            }
            new_splits = Some(splits);
        }
    }

    if audited.is_empty() {
        return Ok(current);
    }
    let user_fields_change = changes.payee.is_some()
        || changes.notes.is_some()
        || changes.review.is_some()
        || changes.assignee.is_some();
    if user_fields_change {
        tx.update_user_fields(id, changes).await?;
    }
    if let Some(splits) = new_splits {
        tx.replace_splits(id, &splits, attribution(actor, now))
            .await?;
        // Valid splits sum to the amount again.
        if current.flags.contains(&TransactionFlag::SplitMismatch) {
            tx.set_flag(id, TransactionFlag::SplitMismatch, false)
                .await?;
        }
    }
    let record = tx.get_transaction(id).await?;
    audit(
        tx,
        now,
        actor,
        "transaction.update",
        "transaction",
        id,
        audited,
    )
    .await?;
    Ok(record)
}

impl Ledger {
    /// Creates a manual transaction in an open account, in the account's
    /// currency. With `slot`, the idempotency key is completed with the
    /// response in the same transaction.
    ///
    /// # Errors
    ///
    /// [`LedgerError::NotFound`] for an unknown or hidden account;
    /// [`LedgerError::Denied`] without write access to it;
    /// [`LedgerError::Invalid`] for an invalid text;
    /// [`LedgerError::Conflict`] when the account is closed or linked to a
    /// provider; storage failures.
    pub async fn create_manual_transaction(
        &self,
        actor: &Actor,
        input: NewManualTransactionInput,
        slot: Option<IdempotencySlot>,
    ) -> Result<TransactionView, LedgerError> {
        let access = self.access.resolve(actor).await?;
        authorize_account(actor, &access, input.account, AccountLevel::Write)?;
        let description = Description::parse(&input.description)
            .map_err(|_| LedgerError::Invalid(DESCRIPTION_INVALID))?;
        let chosen_payee = input.payee.as_deref().map(payee).transpose()?;
        let memo = input
            .memo
            .as_deref()
            .map(|memo| Memo::parse(memo).map_err(|_| LedgerError::Invalid(MEMO_INVALID)))
            .transpose()?;
        let notes = input.notes.as_deref().map(notes).transpose()?.flatten();
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let account = tx.get_account(input.account).await?;
        if account.closed_at.is_some() {
            return Err(LedgerError::Conflict(ProblemCode::Conflict, ACCOUNT_CLOSED));
        }
        if tx.account_is_linked(account.id).await? {
            return Err(LedgerError::Conflict(ProblemCode::Conflict, ACCOUNT_LINKED));
        }
        let mut record = tx
            .insert_transaction(NewTransaction {
                origin: TransactionOrigin::Manual,
                account: account.id,
                posted_on: input.posted_on,
                transacted_on: None,
                pending: false,
                amount_minor: input.minor_units,
                currency: account.currency,
                raw_description: description,
                imported_payee: None,
                memo,
                notes,
                created_at: now,
            })
            .await?;
        // A manual transaction has no imported payee: the one given is the
        // member's own.
        if let Some(chosen) = chosen_payee {
            record = tx
                .update_user_fields(
                    record.id,
                    UserFieldChanges {
                        payee: Some(PayeeChange::Set(chosen)),
                        ..UserFieldChanges::new(now)
                    },
                )
                .await?;
        }
        let changes = AuditChanges::new()
            .set("account", record.account)
            .set("origin", record.origin.as_str())
            .set("posted_on", record.posted_on)
            .set("amount_minor", record.amount_minor)
            .set("currency", record.currency.as_str())
            .set("description", record.raw_description.as_str())
            .set("payee", record.payee.as_ref().map(Payee::as_str))
            .set("memo", record.memo.as_ref().map(Memo::as_str))
            .set("notes", record.notes.as_ref().map(Notes::as_str));
        audit(
            &mut tx,
            now,
            actor,
            "transaction.create",
            "transaction",
            record.id,
            changes,
        )
        .await?;
        let view = TransactionView::new(&record);
        complete_created(&mut tx, slot, &view).await?;
        tx.commit().await?;
        Ok(view)
    }

    /// Changes a transaction's user fields. Only the fields that change are
    /// written and audited; a patch that changes nothing writes nothing.
    ///
    /// # Errors
    ///
    /// [`LedgerError::NotFound`] for an unknown transaction or one in a
    /// hidden account; [`LedgerError::Denied`] without write access to its
    /// account; [`LedgerError::Invalid`] for an invalid value (text, an
    /// assignee who is not an active member, splits that do not fit, or an
    /// unknown or archived category); storage failures.
    pub async fn update_transaction(
        &self,
        actor: &Actor,
        id: TransactionId,
        patch: TransactionPatch,
    ) -> Result<TransactionView, LedgerError> {
        let access = self.access.resolve(actor).await?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let record = apply_patch(&mut tx, actor, &access, now, id, patch).await?;
        let mut views = [TransactionView::new(&record)];
        hide_candidates_in_tx(&mut tx, &access, &mut views).await?;
        tx.commit().await?;
        let [view] = views;
        Ok(view)
    }

    /// Deletes a manual transaction. Imported ones are never deleted by
    /// hand: a sync removes them when their source does.
    ///
    /// # Errors
    ///
    /// [`LedgerError::NotFound`] for an unknown transaction or one in a
    /// hidden account; [`LedgerError::Denied`] without write access to its
    /// account; [`LedgerError::Conflict`] for an imported transaction;
    /// storage failures.
    pub async fn delete_transaction(
        &self,
        actor: &Actor,
        id: TransactionId,
    ) -> Result<(), LedgerError> {
        let access = self.access.resolve(actor).await?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let current = tx.get_transaction(id).await?;
        authorize_account(actor, &access, current.account, AccountLevel::Write)?;
        if current.origin != TransactionOrigin::Manual {
            return Err(LedgerError::Conflict(ProblemCode::Conflict, NOT_MANUAL));
        }
        tx.delete_transaction(id).await?;
        let changes = AuditChanges::new()
            .unset("account", current.account)
            .unset("origin", current.origin.as_str())
            .unset("posted_on", current.posted_on)
            .unset("amount_minor", current.amount_minor)
            .unset("description", current.raw_description.as_str());
        audit(
            &mut tx,
            now,
            actor,
            "transaction.delete",
            "transaction",
            id,
            changes,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// One transaction with its splits.
    ///
    /// # Errors
    ///
    /// [`LedgerError::NotFound`] for an unknown transaction or one in a
    /// hidden account; [`LedgerError::Denied`] without transaction access
    /// to its account; storage failures.
    pub async fn get_transaction(
        &self,
        actor: &Actor,
        id: TransactionId,
    ) -> Result<TransactionView, LedgerError> {
        let access = self.access.resolve(actor).await?;
        let record = self.store.get_transaction(id).await?;
        authorize_account(actor, &access, record.account, AccountLevel::Transactions)?;
        let mut views = [TransactionView::new(&record)];
        self.hide_candidates(&access, &mut views).await?;
        let [view] = views;
        Ok(view)
    }

    /// Whether `actor` may use transaction `id` at `level`, decided before
    /// a create that derives from it reserves its idempotency key.
    ///
    /// # Errors
    ///
    /// [`LedgerError::NotFound`] for an unknown transaction or one in a
    /// hidden account; [`LedgerError::Denied`] when its account is visible
    /// below `level`; storage failures.
    pub async fn authorize_transaction_access(
        &self,
        actor: &Actor,
        id: TransactionId,
        level: AccountLevel,
    ) -> Result<(), LedgerError> {
        let access = self.access.resolve(actor).await?;
        let record = self.store.get_transaction(id).await?;
        authorize_account(actor, &access, record.account, level)?;
        Ok(())
    }

    /// The transactions in accounts the actor may see at
    /// [`AccountLevel::Transactions`] that match `query`, newest posting day
    /// first (ties newest first).
    ///
    /// # Errors
    ///
    /// [`LedgerError::NotFound`] when a requested account does not exist or
    /// is hidden; [`LedgerError::Denied`] when one is visible below
    /// transaction access; an invalid cursor; storage failures.
    pub async fn list_transactions(
        &self,
        actor: &Actor,
        query: TransactionQueryInput,
    ) -> Result<TransactionPage, LedgerError> {
        let access = self.access.resolve(actor).await?;
        for account in &query.accounts {
            authorize_account(actor, &access, *account, AccountLevel::Transactions)?;
            self.store.get_account(*account).await?;
        }
        let filter = TransactionFilter {
            scope: access.scope(AccountLevel::Transactions),
            accounts: query.accounts,
            from: query.from,
            to: query.to,
            review: query.review,
            assignee: query.assignee,
            categories: query.categories,
            uncategorized: query.uncategorized,
            pending: query.pending,
            flag: query.flag,
            min_minor: query.min_minor,
            max_minor: query.max_minor,
            text: query.text,
        };
        let page = self.store.list_transactions(&filter, &query.page).await?;
        let mut items: Vec<TransactionView> = page.items.iter().map(TransactionView::new).collect();
        self.hide_candidates(&access, &mut items).await?;
        Ok(TransactionPage {
            items,
            next: page.next,
        })
    }

    /// Applies every operation, in order, in one transaction: all of them
    /// or none. Each is authorized and audited like
    /// [`Ledger::update_transaction`]. Returns the transactions as they
    /// now are, in operation order.
    ///
    /// # Errors
    ///
    /// [`LedgerError::Invalid`] for an empty batch, one of more than
    /// [`MAX_BATCH_OPERATIONS`], or one naming a transaction twice;
    /// [`LedgerError::Operation`] with the index of the first operation
    /// that fails and its error; storage failures.
    pub async fn apply_batch(
        &self,
        actor: &Actor,
        operations: Vec<BatchOperation>,
    ) -> Result<Vec<TransactionView>, LedgerError> {
        if !(1..=MAX_BATCH_OPERATIONS).contains(&operations.len()) {
            return Err(LedgerError::Invalid(BATCH_SIZE_INVALID));
        }
        let mut seen = HashSet::new();
        if !operations.iter().all(|operation| seen.insert(operation.id)) {
            return Err(LedgerError::Invalid(BATCH_REPEATED));
        }
        let access = self.access.resolve(actor).await?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let mut views = Vec::new();
        for (index, operation) in operations.into_iter().enumerate() {
            let record = apply_patch(&mut tx, actor, &access, now, operation.id, operation.patch)
                .await
                .map_err(|error| LedgerError::operation(index, error))?;
            views.push(TransactionView::new(&record));
        }
        hide_candidates_in_tx(&mut tx, &access, &mut views).await?;
        tx.commit().await?;
        Ok(views)
    }

    /// Removes the duplicate candidates `access` may not see, for reads; see
    /// [`candidates_to_check`].
    async fn hide_candidates(
        &self,
        access: &AccountAccess,
        views: &mut [TransactionView],
    ) -> Result<(), LedgerError> {
        if let Some(candidates) = candidates_to_check(access, views) {
            let accounts = self.store.accounts_of_transactions(&candidates).await?;
            keep_visible_candidates(access, views, &accounts);
        }
        Ok(())
    }
}

/// Removes the duplicate candidates `access` may not see, inside `tx`, so a
/// write fails before its commit if they cannot be read; see
/// [`candidates_to_check`].
pub(super) async fn hide_candidates_in_tx(
    tx: &mut StoreTx,
    access: &AccountAccess,
    views: &mut [TransactionView],
) -> Result<(), LedgerError> {
    if let Some(candidates) = candidates_to_check(access, views) {
        let accounts = tx.accounts_of_transactions(&candidates).await?;
        keep_visible_candidates(access, views, &accounts);
    }
    Ok(())
}

/// The distinct candidates of `views` whose accounts decide what `access`
/// sees, or `None` when it reads every account.
///
/// Defence in depth: the server pairs candidates and merges only within one
/// account, so a transaction the token can read should not list one it
/// cannot. Should such a pair ever exist, naming it would reveal a hidden
/// transaction. `possible_duplicate` stays: it says only that a member
/// should look.
fn candidates_to_check(
    access: &AccountAccess,
    views: &[TransactionView],
) -> Option<Vec<TransactionId>> {
    if access.sees_every_account(AccountLevel::Transactions) {
        return None;
    }
    let distinct: HashSet<TransactionId> = views
        .iter()
        .flat_map(|view| view.duplicate_candidates.iter().copied())
        .collect();
    Some(distinct.into_iter().collect())
}

fn keep_visible_candidates(
    access: &AccountAccess,
    views: &mut [TransactionView],
    accounts: &HashMap<TransactionId, AccountId>,
) {
    for view in views {
        view.duplicate_candidates.retain(|candidate| {
            accounts.get(candidate).is_some_and(|account| {
                access.level_for(*account) >= Some(AccountLevel::Transactions)
            })
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{BATCH_SIZE_INVALID, MAX_BATCH_OPERATIONS};

    #[test]
    fn batch_size_message_states_the_limit() {
        assert_eq!(
            BATCH_SIZE_INVALID,
            format!("a batch has 1 to {MAX_BATCH_OPERATIONS} operations")
        );
    }
}
