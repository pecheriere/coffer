//! Transactions: creating and deleting manual ones, and the fields members
//! and agents set on any — splits, payee, notes, review and assignee — with
//! who categorized and who reviewed.

use coffer_domain::audit::{AuditChanges, AuditValue};
use coffer_domain::identity::{Actor, Via};
use coffer_domain::ids::{AccountId, AgentTokenId, CategoryId, MemberId, SplitId, TransactionId};
use coffer_domain::money::Money;
use coffer_domain::policy::{AccountLevel, authorize_account};
use coffer_domain::transactions::{
    Description, Memo, Notes, Payee, ReviewStatus, SplitError, SplitInput, TransactionFlag,
    TransactionOrigin, validate_splits,
};
use coffer_store::{
    Attribution, NewTransaction, PayeeChange, ReviewChange, StoreError, StoreTx, TransactionRecord,
    UserFieldChanges,
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
    fn new(record: &TransactionRecord) -> Self {
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
            reviewed_by: record.reviewed.as_ref().map(AttributionView::new),
            categorized_by: record.categorized.as_ref().map(AttributionView::new),
            created_at: record.created_at,
            updated_at: record.updated_at,
        }
    }
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

fn attribution(actor: &Actor, at: Timestamp) -> Attribution {
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
fn splits_value(splits: &[SplitInput]) -> Result<AuditValue, LedgerError> {
    serde_json::to_string(splits)
        .map(AuditValue::Text)
        .map_err(|_| LedgerError::Internal("split serialization failed"))
}

fn current_splits(record: &TransactionRecord) -> Vec<SplitInput> {
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
    /// [`LedgerError::Conflict`] when the account is closed; storage
    /// failures.
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
        let current = tx.get_transaction(id).await?;
        authorize_account(actor, &access, current.account, AccountLevel::Write)?;
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
                check_assignee(&mut tx, member).await?;
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
                check_categories(&mut tx, &splits).await?;
                audited = audited.change("splits", splits_value(&before)?, splits_value(&splits)?);
                new_splits = Some(splits);
            }
        }

        if audited.is_empty() {
            return Ok(TransactionView::new(&current));
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
        }
        let record = tx.get_transaction(id).await?;
        audit(
            &mut tx,
            now,
            actor,
            "transaction.update",
            "transaction",
            id,
            audited,
        )
        .await?;
        tx.commit().await?;
        Ok(TransactionView::new(&record))
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
        Ok(TransactionView::new(&record))
    }
}
