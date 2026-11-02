//! Resolving possible duplicates (spec §6.2, §8.4): merging two imported
//! transactions of one account, un-merging a source record into a
//! transaction of its own, and dismissing a `PossibleDuplicate` flag. Each
//! needs `Write` on every account involved and is audited, attributed to
//! the member and the agent token they act through.
//!
//! Merge never loses data:
//!
//! - The target keeps its own user state (payee, notes, review, assignee,
//!   splits). The duplicate's user state is never combined with it: a merge
//!   of a duplicate holding any is refused unless the caller discards it,
//!   and the discarded values are recorded in the audit `before`.
//! - The target keeps its imported fields, except that a pending target
//!   merged with a posted duplicate takes the duplicate's (as when the
//!   sync attaches a posted record to its pending form); a single split
//!   follows a new amount.
//! - The duplicate's candidacies move to the target: its own candidates
//!   become the target's, and transactions listing it list the target
//!   instead.
//!
//! Stale candidate pairs are forgotten after a merge; see
//! [`crate::canonical`].

use coffer_domain::audit::AuditChanges;
use coffer_domain::identity::Actor;
use coffer_domain::ids::{SourceRecordId, TransactionId};
use coffer_domain::policy::{AccountLevel, authorize_account};
use coffer_domain::transactions::{Memo, Payee, ReviewStatus, TransactionFlag, TransactionOrigin};
use coffer_store::{
    ImportedFields, NewTransaction, SourceRecord, StoreError, StoreTx, TransactionRecord,
};

use super::transactions::{
    TransactionView, attribution, current_splits, hide_candidates_in_tx, splits_value,
};
use super::{Ledger, LedgerError, audit, complete_created};
use crate::canonical::{
    apply_imported_fields, drop_candidate, drop_stale_candidates, posted_connections,
};
use crate::http::idempotency::IdempotencySlot;
use crate::http::problem::ProblemCode;

const MERGE_SELF: &str = "a transaction cannot be merged with itself";
const MERGE_ACROSS_ACCOUNTS: &str = "only transactions of the same account can be merged";
const MERGE_NOT_IMPORTED: &str = "only imported transactions can be merged";
const MERGE_REPORTED_APART: &str =
    "both transactions hold a posted record of the same connection: they are distinct purchases";
const MERGE_CONFLICT: &str = "the duplicate has notes, splits, a review, an assignee or an edited \
                              payee: set discard_duplicate_state to discard them";
const SOURCE_NOT_OWN: &str = "the source is not one of the transaction's records";
const LAST_SOURCE: &str = "a transaction keeps at least one source record";

/// `ids`, comma-separated; `None` when there are none.
fn listed(ids: &[TransactionId]) -> Option<String> {
    (!ids.is_empty()).then(|| {
        ids.iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",")
    })
}

/// The imported fields `record` shows.
fn imported_fields(record: &TransactionRecord) -> ImportedFields {
    ImportedFields {
        posted_on: record.posted_on,
        transacted_on: record.transacted_on,
        pending: record.pending,
        amount_minor: record.amount_minor,
        description: record.raw_description.clone(),
        payee: record.imported_payee.clone(),
        memo: record.memo.clone(),
    }
}

/// The record whose fields a transaction holding `sources` shows: a posted
/// one before any pending one (a pending record never overwrites a posted
/// transaction), then the one seen last.
fn representative(sources: &[&SourceRecord]) -> Option<ImportedFields> {
    sources
        .iter()
        .max_by_key(|source| {
            (
                !source.fields.pending,
                source.last_seen_at,
                source.fields.posted_on,
                source.id,
            )
        })
        .map(|source| source.fields.clone())
}

/// The duplicate's audit record as it is merged away: what identified it,
/// and every user value and candidacy that goes with it.
fn merged_away(
    duplicate: &TransactionRecord,
    target: TransactionId,
) -> Result<AuditChanges, LedgerError> {
    let mut changes = AuditChanges::new()
        .set("merged_into", target)
        .unset("account", duplicate.account)
        .unset("origin", duplicate.origin.as_str())
        .unset("posted_on", duplicate.posted_on)
        .unset("amount_minor", duplicate.amount_minor)
        .unset("description", duplicate.raw_description.as_str());
    if let Some(notes) = &duplicate.notes {
        changes = changes.unset("notes", notes.as_str());
    }
    if duplicate.payee_edited {
        changes = changes.unset("payee", duplicate.payee.as_ref().map(Payee::as_str));
    }
    if duplicate.review == ReviewStatus::Reviewed {
        changes = changes.unset("review", duplicate.review.as_str());
    }
    if let Some(assignee) = duplicate.assignee {
        changes = changes.unset("assignee", assignee);
    }
    if !duplicate.splits.is_empty() {
        changes = changes.unset("splits", splits_value(&current_splits(duplicate))?);
    }
    if let Some(candidates) = listed(&duplicate.duplicate_candidates) {
        changes = changes.unset("duplicate_candidates", candidates);
    }
    Ok(changes)
}

/// Deletes `duplicate`, merged into `target`, moving its candidacies to
/// the target: its own candidates become the target's and transactions
/// listing it list the target instead. Sets or clears the target's
/// `PossibleDuplicate` flag to match its candidates, appending both to
/// `changes`. Returns them with the transactions that listed the
/// duplicate, each with whether it already listed the target.
async fn delete_into(
    tx: &mut StoreTx,
    target: &TransactionRecord,
    duplicate: &TransactionRecord,
    mut changes: AuditChanges,
) -> Result<(AuditChanges, Vec<(TransactionId, bool)>), StoreError> {
    let mut listers = Vec::new();
    for lister in tx.listed_as_candidate(duplicate.id).await? {
        if lister != target.id {
            let listed_target = tx
                .get_transaction(lister)
                .await?
                .duplicate_candidates
                .contains(&target.id);
            listers.push((lister, listed_target));
        }
    }
    let inherited: Vec<TransactionId> = duplicate
        .duplicate_candidates
        .iter()
        .copied()
        .filter(|candidate| *candidate != target.id)
        .collect();
    // Deleting it forgets every pair it was in.
    tx.delete_transaction(duplicate.id).await?;
    tx.add_duplicate_candidates(target.id, &inherited).await?;
    for (lister, _) in &listers {
        tx.add_duplicate_candidates(*lister, &[target.id]).await?;
    }
    let merged = tx.get_transaction(target.id).await?;
    if merged.duplicate_candidates != target.duplicate_candidates {
        changes = changes.change(
            "duplicate_candidates",
            listed(&target.duplicate_candidates),
            listed(&merged.duplicate_candidates),
        );
    }
    let was_flagged = target.flags.contains(&TransactionFlag::PossibleDuplicate);
    let flagged = !merged.duplicate_candidates.is_empty();
    if flagged != was_flagged {
        tx.set_flag(target.id, TransactionFlag::PossibleDuplicate, flagged)
            .await?;
        changes = changes.change("possible_duplicate", was_flagged, flagged);
    }
    Ok((changes, listers))
}

impl Ledger {
    /// Merges `duplicate` into `target`: the duplicate's source records
    /// move to the target and the duplicate is deleted (see the module
    /// documentation for what the target keeps). The target's
    /// `PossibleDuplicate` flag clears when it has no candidate left.
    ///
    /// # Errors
    ///
    /// [`LedgerError::NotFound`] when either transaction is unknown or in a
    /// hidden account; [`LedgerError::Denied`] without write access to
    /// either account; [`LedgerError::Invalid`] for the same transaction
    /// twice, transactions of different accounts, a manual one, or two
    /// that each hold a posted record of the same connection (distinct
    /// purchases);
    /// [`LedgerError::Conflict`] (`merge_conflict`) when the duplicate has
    /// user state and `discard_duplicate_state` is false; storage failures.
    pub async fn merge(
        &self,
        actor: &Actor,
        target: TransactionId,
        duplicate: TransactionId,
        discard_duplicate_state: bool,
    ) -> Result<TransactionView, LedgerError> {
        let access = self.access.resolve(actor).await?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let target = tx.get_transaction(target).await?;
        authorize_account(actor, &access, target.account, AccountLevel::Write)?;
        if target.id == duplicate {
            return Err(LedgerError::Invalid(MERGE_SELF));
        }
        let duplicate = tx.get_transaction(duplicate).await?;
        authorize_account(actor, &access, duplicate.account, AccountLevel::Write)?;
        if target.account != duplicate.account {
            return Err(LedgerError::Invalid(MERGE_ACROSS_ACCOUNTS));
        }
        if target.origin != TransactionOrigin::Imported
            || duplicate.origin != TransactionOrigin::Imported
        {
            return Err(LedgerError::Invalid(MERGE_NOT_IMPORTED));
        }
        if !posted_connections(&target.sources, &[])
            .is_disjoint(&posted_connections(&duplicate.sources, &[]))
        {
            return Err(LedgerError::Invalid(MERGE_REPORTED_APART));
        }
        if !duplicate.user_state().is_empty() && !discard_duplicate_state {
            return Err(LedgerError::Conflict(
                ProblemCode::MergeConflict,
                MERGE_CONFLICT,
            ));
        }
        let discarded = merged_away(&duplicate, target.id)?;

        for source in &duplicate.sources {
            tx.attach_source(source.id, target.id).await?;
        }
        let moved: Vec<String> = duplicate.sources.iter().map(|s| s.id.to_string()).collect();
        let mut changes = AuditChanges::new()
            .set("merged", duplicate.id)
            .set("sources", moved.join(","));
        if target.pending && !duplicate.pending {
            changes = apply_imported_fields(
                &mut tx,
                &target,
                &imported_fields(&duplicate),
                changes,
                &attribution(actor, now),
            )
            .await?;
        }

        let (changes, listers) = delete_into(&mut tx, &target, &duplicate, changes).await?;

        audit(
            &mut tx,
            now,
            actor,
            "transaction.merged",
            "transaction",
            duplicate.id,
            discarded,
        )
        .await?;
        audit(
            &mut tx,
            now,
            actor,
            "transaction.merge",
            "transaction",
            target.id,
            changes,
        )
        .await?;
        // A lister that already listed the target only lost the duplicate.
        for (lister, listed_target) in listers {
            let (action, changes) = if listed_target {
                (
                    "transaction.candidate_removed",
                    AuditChanges::new().unset("duplicate_candidate", duplicate.id),
                )
            } else {
                (
                    "transaction.candidate_replaced",
                    AuditChanges::new().change("duplicate_candidate", duplicate.id, target.id),
                )
            };
            audit(&mut tx, now, actor, action, "transaction", lister, changes).await?;
        }
        drop_stale_candidates(&mut tx, now, &actor.into(), target.id, &[]).await?;
        let record = tx.get_transaction(target.id).await?;
        let mut views = [TransactionView::new(&record)];
        hide_candidates_in_tx(&mut tx, &access, &mut views).await?;
        tx.commit().await?;
        let [view] = views;
        Ok(view)
    }

    /// Moves `source` out of `transaction` into a new imported transaction
    /// of the same account, with the record's fields and no user state,
    /// flags or candidates. The transaction keeps its user state and shows
    /// the fields of its remaining records (a posted one before a pending
    /// one, then the one seen last); a single split follows a new amount.
    /// Later syncs keep them apart: a record stays on the transaction that
    /// holds it. With `slot`, the idempotency key is completed with the
    /// response in the same transaction.
    ///
    /// # Errors
    ///
    /// [`LedgerError::NotFound`] for an unknown transaction or one in a
    /// hidden account; [`LedgerError::Denied`] without write access to its
    /// account; [`LedgerError::Invalid`] when `source` is not one of its
    /// records; [`LedgerError::Conflict`] when it is its only record;
    /// storage failures.
    pub async fn unmerge(
        &self,
        actor: &Actor,
        transaction: TransactionId,
        source: SourceRecordId,
        slot: Option<IdempotencySlot>,
    ) -> Result<TransactionView, LedgerError> {
        let access = self.access.resolve(actor).await?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let current = tx.get_transaction(transaction).await?;
        authorize_account(actor, &access, current.account, AccountLevel::Write)?;
        let Some(moving) = current.sources.iter().find(|s| s.id == source) else {
            return Err(LedgerError::Invalid(SOURCE_NOT_OWN));
        };
        let remaining: Vec<&SourceRecord> =
            current.sources.iter().filter(|s| s.id != source).collect();
        let Some(kept_fields) = representative(&remaining) else {
            return Err(LedgerError::Conflict(ProblemCode::Conflict, LAST_SOURCE));
        };

        let fields = &moving.fields;
        let created = tx
            .insert_transaction(NewTransaction {
                origin: TransactionOrigin::Imported,
                account: current.account,
                posted_on: fields.posted_on,
                transacted_on: fields.transacted_on,
                pending: fields.pending,
                amount_minor: fields.amount_minor,
                currency: current.currency,
                raw_description: fields.description.clone(),
                imported_payee: fields.payee.clone(),
                memo: fields.memo.clone(),
                notes: None,
                created_at: now,
            })
            .await?;
        tx.attach_source(source, created.id).await?;
        let changes = AuditChanges::new()
            .unset("source", source)
            .set("unmerged_into", created.id);
        let changes = apply_imported_fields(
            &mut tx,
            &current,
            &kept_fields,
            changes,
            &attribution(actor, now),
        )
        .await?;

        let created_changes = AuditChanges::new()
            .set("unmerged_from", current.id)
            .set("account", created.account)
            .set("origin", created.origin.as_str())
            .set("posted_on", created.posted_on)
            .set("transacted_on", created.transacted_on)
            .set("pending", created.pending)
            .set("amount_minor", created.amount_minor)
            .set("currency", created.currency.as_str())
            .set("description", created.raw_description.as_str())
            .set("payee", created.payee.as_ref().map(Payee::as_str))
            .set("memo", created.memo.as_ref().map(Memo::as_str))
            .set("source", source);
        audit(
            &mut tx,
            now,
            actor,
            "transaction.unmerge",
            "transaction",
            current.id,
            changes,
        )
        .await?;
        audit(
            &mut tx,
            now,
            actor,
            "transaction.unmerged",
            "transaction",
            created.id,
            created_changes,
        )
        .await?;
        let record = tx.get_transaction(created.id).await?;
        let view = TransactionView::new(&record);
        complete_created(&mut tx, slot, &view).await?;
        tx.commit().await?;
        Ok(view)
    }

    /// Clears a transaction's `PossibleDuplicate` flag and forgets its
    /// candidates; a candidate that lists it back forgets it too, its own
    /// flag clearing when no candidate is left. A transaction neither flagged
    /// nor listing candidates is returned as it is, and nothing is recorded.
    ///
    /// # Errors
    ///
    /// [`LedgerError::NotFound`] for an unknown transaction or one in a
    /// hidden account; [`LedgerError::Denied`] without write access to its
    /// account; storage failures.
    pub async fn dismiss_duplicate(
        &self,
        actor: &Actor,
        transaction: TransactionId,
    ) -> Result<TransactionView, LedgerError> {
        let access = self.access.resolve(actor).await?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let current = tx.get_transaction(transaction).await?;
        authorize_account(actor, &access, current.account, AccountLevel::Write)?;
        let flagged = current.flags.contains(&TransactionFlag::PossibleDuplicate);
        let mut changes = AuditChanges::new();
        if let Some(candidates) = listed(&current.duplicate_candidates) {
            tx.clear_duplicate_candidates(current.id).await?;
            changes = changes.unset("duplicate_candidates", candidates);
        }
        if flagged {
            tx.set_flag(current.id, TransactionFlag::PossibleDuplicate, false)
                .await?;
            changes = changes.change("possible_duplicate", true, false);
        }
        if changes.is_empty() {
            return Ok(TransactionView::new(&current));
        }
        // A dismissed candidate that lists this transaction back is not its
        // duplicate either.
        let mut mutual = Vec::new();
        for candidate in &current.duplicate_candidates {
            if tx
                .get_transaction(*candidate)
                .await?
                .duplicate_candidates
                .contains(&current.id)
            {
                mutual.push(*candidate);
            }
        }
        audit(
            &mut tx,
            now,
            actor,
            "transaction.dismiss_duplicate",
            "transaction",
            current.id,
            changes,
        )
        .await?;
        for candidate in mutual {
            drop_candidate(&mut tx, now, &actor.into(), candidate, current.id).await?;
        }
        let record = tx.get_transaction(current.id).await?;
        tx.commit().await?;
        Ok(TransactionView::new(&record))
    }
}

#[cfg(test)]
mod tests {
    use coffer_domain::ids::{ConnectionId, ProviderAccountId, SourceRecordId, TransactionId};
    use coffer_domain::transactions::Description;
    use coffer_store::{ImportedFields, SourceRecord};
    use jiff::Timestamp;
    use jiff::civil::date;

    use super::{listed, representative};

    fn source(connection: ConnectionId, pending: bool, seen: i64, amount: i64) -> SourceRecord {
        SourceRecord {
            id: SourceRecordId::new(),
            connection,
            provider_account: ProviderAccountId::new(),
            provider_txn_id: "p".into(),
            transaction: None,
            fields: ImportedFields {
                posted_on: date(2026, 10, 10),
                transacted_on: None,
                pending,
                amount_minor: amount,
                description: Description::parse("SHOP").unwrap(),
                payee: None,
                memo: None,
            },
            amount_raw: amount.to_string(),
            fingerprint: String::new(),
            fingerprint_version: 1,
            first_seen_at: Timestamp::from_second(seen).unwrap(),
            last_seen_at: Timestamp::from_second(seen).unwrap(),
        }
    }

    #[test]
    fn representative_prefers_posted_then_latest_seen() {
        let connection = ConnectionId::new();
        let old_posted = source(connection, false, 1, -1);
        let new_pending = source(connection, true, 9, -2);
        let new_posted = source(connection, false, 5, -3);
        assert_eq!(
            representative(&[&old_posted, &new_pending, &new_posted]).map(|f| f.amount_minor),
            Some(-3)
        );
        assert_eq!(
            representative(&[&new_pending]).map(|f| f.amount_minor),
            Some(-2)
        );
        assert_eq!(representative(&[]), None);
    }

    #[test]
    fn listed_ids_are_comma_separated_or_none() {
        let (a, b) = (TransactionId::new(), TransactionId::new());
        assert_eq!(listed(&[]), None);
        assert_eq!(listed(&[a, b]), Some(format!("{a},{b}")));
    }
}
