//! Changes to canonical transactions shared by the sync and the ledger:
//! applying a record's imported fields, and forgetting duplicate
//! candidates (spec §6.2, §8.4).
//!
//! A candidate pair is stale once both sides hold a posted record of the
//! same connection: one connection reports each purchase once, so they are
//! two purchases. Stale pairs are forgotten whenever a transaction gains
//! such a record (when the sync attaches one, when a pending record is
//! re-reported posted, and after a merge), and a flag with no candidate
//! left clears.

use std::collections::HashSet;

use coffer_domain::audit::{AuditActor, AuditChanges, AuditEntry};
use coffer_domain::ids::{ConnectionId, SourceRecordId, TransactionId};
use coffer_domain::transactions::{Memo, Payee, SplitInput, TransactionFlag};
use coffer_store::{
    Attribution, ImportedFields, SourceRecord, StoreError, StoreTx, TransactionRecord,
};
use jiff::Timestamp;

/// The connections of which `sources` hold a posted record, leaving out
/// `ignored` ones.
pub(crate) fn posted_connections(
    sources: &[SourceRecord],
    ignored: &[&HashSet<SourceRecordId>],
) -> HashSet<ConnectionId> {
    sources
        .iter()
        .filter(|source| {
            !source.fields.pending && !ignored.iter().any(|set| set.contains(&source.id))
        })
        .map(|source| source.connection)
        .collect()
}

async fn record_audit(
    tx: &mut StoreTx,
    now: Timestamp,
    actor: &AuditActor,
    action: &str,
    transaction: TransactionId,
    changes: AuditChanges,
) -> Result<(), StoreError> {
    tx.append_audit(
        now,
        &AuditEntry {
            actor: actor.clone(),
            action: action.to_owned(),
            entity_type: "transaction".to_owned(),
            entity_id: transaction.to_string(),
            changes,
        },
    )
    .await?;
    Ok(())
}

/// Forgets `candidate` as one `transaction` may duplicate (a pair already
/// gone, as when the candidate was deleted, is fine) and clears its
/// `PossibleDuplicate` flag once no candidate is left. Audited as
/// `transaction.candidate_removed` by `actor`.
///
/// # Errors
///
/// Storage failures, including a `transaction` that does not exist.
pub(crate) async fn drop_candidate(
    tx: &mut StoreTx,
    now: Timestamp,
    actor: &AuditActor,
    transaction: TransactionId,
    candidate: TransactionId,
) -> Result<(), StoreError> {
    tx.remove_duplicate_candidate(transaction, candidate)
        .await?;
    let current = tx.get_transaction(transaction).await?;
    let mut changes = AuditChanges::new().unset("duplicate_candidate", candidate);
    if current.duplicate_candidates.is_empty()
        && current.flags.contains(&TransactionFlag::PossibleDuplicate)
    {
        tx.set_flag(transaction, TransactionFlag::PossibleDuplicate, false)
            .await?;
        changes = changes.change("possible_duplicate", true, false);
    }
    record_audit(
        tx,
        now,
        actor,
        "transaction.candidate_removed",
        transaction,
        changes,
    )
    .await
}

/// Forgets every candidate pair involving `transaction` (either side)
/// whose two transactions each hold a posted record of the same
/// connection, leaving out `ignored` records (such as ones a sync is about
/// to remove). See the module documentation.
///
/// # Errors
///
/// Storage failures, including a `transaction` that does not exist.
pub(crate) async fn drop_stale_candidates(
    tx: &mut StoreTx,
    now: Timestamp,
    actor: &AuditActor,
    transaction: TransactionId,
    ignored: &[&HashSet<SourceRecordId>],
) -> Result<(), StoreError> {
    let current = tx.get_transaction(transaction).await?;
    let posted = posted_connections(&current.sources, ignored);
    if posted.is_empty() {
        return Ok(());
    }
    let apart = async |tx: &mut StoreTx, other: TransactionId| -> Result<bool, StoreError> {
        let other = tx.get_transaction(other).await?;
        Ok(!posted_connections(&other.sources, ignored).is_disjoint(&posted))
    };
    for candidate in &current.duplicate_candidates {
        if apart(tx, *candidate).await? {
            drop_candidate(tx, now, actor, transaction, *candidate).await?;
        }
    }
    for lister in tx.listed_as_candidate(transaction).await? {
        if apart(tx, lister).await? {
            drop_candidate(tx, now, actor, lister, transaction).await?;
        }
    }
    Ok(())
}

/// Replaces `current`'s imported fields with `fields`, appending what
/// changed to `changes`; a transaction whose fields already match writes
/// nothing. User fields never change, except that a single split follows a
/// new amount, recorded as categorized by `split_by`; several splits stay,
/// and the transaction is flagged `SplitMismatch` while they no longer sum
/// to it. Shared by the sync and by merge and unmerge.
///
/// # Errors
///
/// Storage failures.
pub(crate) async fn apply_imported_fields(
    tx: &mut StoreTx,
    current: &TransactionRecord,
    fields: &ImportedFields,
    mut changes: AuditChanges,
    split_by: &Attribution,
) -> Result<AuditChanges, StoreError> {
    let unchanged = current.posted_on == fields.posted_on
        && current.transacted_on == fields.transacted_on
        && current.pending == fields.pending
        && current.amount_minor == fields.amount_minor
        && current.raw_description == fields.description
        && current.imported_payee == fields.payee
        && current.memo == fields.memo;
    if unchanged {
        return Ok(changes);
    }
    if current.posted_on != fields.posted_on {
        changes = changes.change("posted_on", current.posted_on, fields.posted_on);
    }
    if current.transacted_on != fields.transacted_on {
        changes = changes.change("transacted_on", current.transacted_on, fields.transacted_on);
    }
    if current.pending != fields.pending {
        changes = changes.change("pending", current.pending, fields.pending);
    }
    if current.amount_minor != fields.amount_minor {
        changes = changes.change("amount_minor", current.amount_minor, fields.amount_minor);
    }
    if current.raw_description != fields.description {
        changes = changes.change(
            "description",
            current.raw_description.as_str(),
            fields.description.as_str(),
        );
    }
    if current.imported_payee != fields.payee {
        let new_payee = fields.payee.as_ref().map(Payee::as_str);
        changes = changes.change(
            "imported_payee",
            current.imported_payee.as_ref().map(Payee::as_str),
            new_payee,
        );
        if !current.payee_edited {
            changes = changes.change(
                "payee",
                current.payee.as_ref().map(Payee::as_str),
                new_payee,
            );
        }
    }
    if current.memo != fields.memo {
        changes = changes.change(
            "memo",
            current.memo.as_ref().map(Memo::as_str),
            fields.memo.as_ref().map(Memo::as_str),
        );
    }
    tx.update_imported_fields(current.id, fields, split_by.at)
        .await?;
    if current.amount_minor != fields.amount_minor {
        changes = follow_amount(tx, current, fields.amount_minor, changes, split_by).await?;
    }
    Ok(changes)
}

/// Keeps the splits of `current` consistent with its new amount.
async fn follow_amount(
    tx: &mut StoreTx,
    current: &TransactionRecord,
    amount_minor: i64,
    mut changes: AuditChanges,
    split_by: &Attribution,
) -> Result<AuditChanges, StoreError> {
    let mismatch = match current.splits.as_slice() {
        [] => false,
        // A split is never zero: a single split cannot follow a zero
        // amount, so it stays and the transaction is flagged.
        [only] if amount_minor != 0 => {
            tx.replace_splits(
                current.id,
                &[SplitInput {
                    category: only.category,
                    minor_units: amount_minor,
                }],
                split_by.clone(),
            )
            .await?;
            changes = changes.change("split_minor_units", only.minor_units, amount_minor);
            false
        }
        splits => {
            let sum: i128 = splits
                .iter()
                .map(|split| i128::from(split.minor_units))
                .sum();
            sum != i128::from(amount_minor)
        }
    };
    let flagged = current.flags.contains(&TransactionFlag::SplitMismatch);
    if mismatch != flagged {
        tx.set_flag(current.id, TransactionFlag::SplitMismatch, mismatch)
            .await?;
        changes = changes.change("split_mismatch", flagged, mismatch);
    }
    Ok(changes)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use coffer_domain::ids::{ConnectionId, ProviderAccountId, SourceRecordId};
    use coffer_domain::transactions::Description;
    use coffer_store::{ImportedFields, SourceRecord};
    use jiff::Timestamp;
    use jiff::civil::date;

    use super::posted_connections;

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
    fn only_posted_unignored_records_count_toward_a_connection() {
        let (one, two) = (ConnectionId::new(), ConnectionId::new());
        let posted = source(one, false, 1, -1);
        let pending = source(two, true, 1, -1);
        let ignored = source(two, false, 1, -1);
        let skip = HashSet::from([ignored.id]);
        assert_eq!(
            posted_connections(&[posted, pending, ignored], &[&skip]),
            HashSet::from([one])
        );
    }
}
