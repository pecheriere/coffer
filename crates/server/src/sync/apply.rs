//! The apply step: one provider account's fetched records, matched onto
//! canonical transactions inside the caller's transaction (spec §6.2,
//! §8.4).
//!
//! The order matters. Records that vanished are only computed first; every
//! incoming record is then matched (so a posted record can still attach to
//! the pending one it replaces); only then are the vanished records
//! removed. Every change to a canonical transaction is audited as
//! `system:sync`; a record that changes nothing writes no audit entry.

use std::collections::{BTreeMap, HashSet};

use coffer_domain::audit::{AuditActor, AuditChanges, AuditEntry};
use coffer_domain::ids::{ConnectionId, ProviderAccountId, SourceRecordId, TransactionId};
use coffer_domain::matching::{
    Candidate, DATE_TOLERANCE_DAYS, Incoming, MatchDecision, decide, description_key,
    pending_counterparts, posted_counterparts,
};
use coffer_domain::transactions::{
    Description, Memo, Payee, SplitInput, TransactionFlag, TransactionOrigin,
};
use coffer_store::{
    AccountRecord, Attribution, CandidateRow, ImportedFields, NewSource, NewTransaction,
    ProviderAccountRecord, SourceRecord, SourceUpdate, StoreTx, TransactionRecord,
};
use jiff::civil::Date;
use jiff::{Span, Timestamp};

use super::SyncError;

/// The reason every sync change is attributed to.
pub const SYNC_REASON: &str = "system:sync";

const CONNECTION_MISMATCH: &str = "the provider account belongs to another connection";
const BINDING_MISMATCH: &str = "the account is not the provider account's binding";
const RECORD_MOVED: &str = "a record moved to another provider account";
const DATE_OUT_OF_RANGE: &str = "a posting date is out of range";

/// The longest provider record id, in characters.
pub const MAX_PROVIDER_ID_CHARS: usize = 255;

/// The longest raw amount, in characters.
pub const MAX_AMOUNT_RAW_CHARS: usize = 64;

/// Whether `id` can identify a provider record: 1 to
/// [`MAX_PROVIDER_ID_CHARS`] characters without control characters.
fn valid_provider_id(id: &str) -> bool {
    (1..=MAX_PROVIDER_ID_CHARS).contains(&id.chars().count()) && !id.chars().any(char::is_control)
}

/// Whether `raw` is a decimal text of at most [`MAX_AMOUNT_RAW_CHARS`]
/// characters: an optional sign, digits, and optionally a point and more
/// digits.
fn valid_amount_raw(raw: &str) -> bool {
    if raw.len() > MAX_AMOUNT_RAW_CHARS {
        return false;
    }
    let unsigned = raw.strip_prefix(['-', '+']).unwrap_or(raw);
    let (whole, fraction) = unsigned.split_once('.').unwrap_or((unsigned, "0"));
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    digits(whole) && digits(fraction)
}

/// A transaction record as a provider reports it, its amount already in
/// the account currency's minor units. Texts are sanitized to the
/// transaction limits when applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedTransaction {
    /// The provider's id, unique within the connection.
    pub provider_id: String,
    pub posted_on: Date,
    pub transacted_on: Option<Date>,
    /// Negative is money out of the account.
    pub amount_minor: i64,
    /// The amount as the provider gave it.
    pub amount_raw: String,
    pub description: String,
    pub payee: Option<String>,
    pub memo: Option<String>,
    pub pending: bool,
    /// The provider id of an earlier record this one replaces (such as the
    /// pending form of a posted record), when the provider says.
    pub replaces_provider_id: Option<String>,
}

/// Which records a fetch covers, and so which absent ones vanished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coverage {
    /// Every record posted on or after `start`: a pending record of that
    /// period that is absent has vanished.
    Window { start: Date },
    /// Only changes: nothing vanishes unless named in `removed`.
    Delta,
}

/// One provider account's share of a fetch.
#[derive(Debug, Clone)]
pub struct AccountBatch {
    pub provider_account: ProviderAccountRecord,
    /// The account it is bound to; `None` holds its records.
    pub account: Option<AccountRecord>,
    pub transactions: Vec<NormalizedTransaction>,
    /// Provider ids of records the provider says are gone.
    pub removed: Vec<String>,
    pub coverage: Coverage,
}

/// What an apply did to canonical transactions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ApplyReport {
    /// Transactions created.
    pub created: u32,
    /// Of those, the ones flagged as possible duplicates.
    pub possible_duplicates: u32,
    /// Records attached to an existing transaction.
    pub attached: u32,
    /// Transactions whose imported fields changed.
    pub updated: u32,
    /// Transactions deleted when their last record vanished.
    pub deleted: u32,
    /// Transactions kept and flagged when their last record vanished.
    pub source_removed: u32,
    /// Records held for an unbound provider account.
    pub held: u32,
    /// Malformed records skipped (an empty, over-long or control-character
    /// provider id, or an amount that is not a decimal text).
    pub skipped: u32,
}

/// An incoming record with its texts sanitized.
struct Record {
    provider_id: String,
    fields: ImportedFields,
    amount_raw: String,
    replaces: Option<String>,
}

impl Record {
    /// The record, its texts sanitized; `None` when its provider id or raw
    /// amount is malformed.
    fn new(normalized: NormalizedTransaction) -> Option<Self> {
        if !valid_provider_id(&normalized.provider_id) || !valid_amount_raw(&normalized.amount_raw)
        {
            return None;
        }
        Some(Self {
            fields: ImportedFields {
                posted_on: normalized.posted_on,
                transacted_on: normalized.transacted_on,
                pending: normalized.pending,
                amount_minor: normalized.amount_minor,
                description: Description::from_provider(&normalized.description),
                payee: normalized.payee.as_deref().and_then(Payee::from_provider),
                memo: normalized.memo.as_deref().and_then(Memo::from_provider),
            },
            provider_id: normalized.provider_id,
            amount_raw: normalized.amount_raw,
            replaces: normalized.replaces_provider_id,
        })
    }

    fn from_held(source: SourceRecord) -> Self {
        Self {
            provider_id: source.provider_txn_id,
            fields: source.fields,
            amount_raw: source.amount_raw,
            replaces: None,
        }
    }

    fn update(&self, now: Timestamp) -> SourceUpdate {
        SourceUpdate {
            fields: self.fields.clone(),
            amount_raw: self.amount_raw.clone(),
            seen_at: now,
        }
    }
}

/// The state of one apply.
struct Apply<'a> {
    tx: &'a mut StoreTx,
    now: Timestamp,
    connection: ConnectionId,
    provider_account: ProviderAccountId,
    /// Sources that vanished from this fetch; removed after matching.
    vanished: HashSet<SourceRecordId>,
    report: ApplyReport,
}

/// Applies one provider account's records (see the module documentation).
///
/// # Errors
///
/// [`SyncError::Invalid`] when the provider account is not of
/// `connection`, `batch.account` is not its binding, a known record moved
/// to another provider account, or a posting date is out of range; storage
/// failures. The caller rolls back its transaction on any error, so one
/// account's data is never partially committed.
pub async fn apply_account_batch(
    tx: &mut StoreTx,
    now: Timestamp,
    connection: ConnectionId,
    batch: AccountBatch,
) -> Result<ApplyReport, SyncError> {
    let provider_account = &batch.provider_account;
    if provider_account.connection != connection {
        return Err(SyncError::Invalid(CONNECTION_MISMATCH));
    }
    if batch.account.as_ref().map(|account| account.id) != provider_account.account {
        return Err(SyncError::Invalid(BINDING_MISMATCH));
    }
    let total = batch.transactions.len();
    let records: Vec<Record> = batch
        .transactions
        .into_iter()
        .filter_map(Record::new)
        .collect();
    let skipped = u32::try_from(total - records.len()).unwrap_or(u32::MAX);
    if skipped > 0 {
        tracing::warn!(skipped, "skipped malformed provider records");
    }
    let mut apply = Apply {
        tx,
        now,
        connection,
        provider_account: provider_account.id,
        vanished: HashSet::new(),
        report: ApplyReport {
            skipped,
            ..ApplyReport::default()
        },
    };

    // 1. The vanished set; nothing is deleted yet.
    let present: HashSet<&str> = records.iter().map(|r| r.provider_id.as_str()).collect();
    let mut vanished: BTreeMap<SourceRecordId, SourceRecord> = BTreeMap::new();
    for provider_id in &batch.removed {
        if present.contains(provider_id.as_str()) {
            continue;
        }
        if let Some(source) = apply.tx.find_source(connection, provider_id).await?
            && source.provider_account == provider_account.id
        {
            vanished.insert(source.id, source);
        }
    }
    if let Coverage::Window { start } = batch.coverage {
        for source in apply
            .tx
            .pending_sources_since(provider_account.id, start)
            .await?
        {
            if !present.contains(source.provider_txn_id.as_str()) {
                vanished.insert(source.id, source);
            }
        }
    }
    apply.vanished = vanished.keys().copied().collect();

    // 2. Match every incoming record.
    for record in records {
        match &batch.account {
            Some(account) => apply.match_record(account, record).await?,
            None => apply.hold(record).await?,
        }
    }

    // 3. Remove the leftovers.
    for source in vanished.into_values() {
        apply.remove(source).await?;
    }
    Ok(apply.report)
}

/// Runs the held records of a provider account just bound to `account`
/// through matching.
///
/// # Errors
///
/// [`SyncError::Invalid`] when `account` is not the provider account's
/// binding or a posting date is out of range; storage failures.
pub async fn apply_held(
    tx: &mut StoreTx,
    now: Timestamp,
    provider_account: ProviderAccountRecord,
    account: AccountRecord,
) -> Result<ApplyReport, SyncError> {
    if provider_account.account != Some(account.id) {
        return Err(SyncError::Invalid(BINDING_MISMATCH));
    }
    let held = tx.held_sources(provider_account.id).await?;
    let mut apply = Apply {
        tx,
        now,
        connection: provider_account.connection,
        provider_account: provider_account.id,
        vanished: HashSet::new(),
        report: ApplyReport::default(),
    };
    for source in held {
        let id = source.id;
        apply
            .place(&account, Record::from_held(source), Some(id))
            .await?;
    }
    Ok(apply.report)
}

/// Who changes splits on the sync's behalf.
fn sync_attribution(at: Timestamp) -> Attribution {
    Attribution {
        member: None,
        agent: None,
        system: Some(SYNC_REASON.to_owned()),
        at,
    }
}

impl Apply<'_> {
    async fn audit(
        &mut self,
        action: &str,
        transaction: TransactionId,
        changes: AuditChanges,
    ) -> Result<(), SyncError> {
        self.tx
            .append_audit(
                self.now,
                &AuditEntry {
                    actor: AuditActor::System {
                        reason: SYNC_REASON.to_owned(),
                    },
                    action: action.to_owned(),
                    entity_type: "transaction".to_owned(),
                    entity_id: transaction.to_string(),
                    changes,
                },
            )
            .await?;
        Ok(())
    }

    /// Keeps a record of an unbound provider account, without a
    /// transaction.
    async fn hold(&mut self, record: Record) -> Result<(), SyncError> {
        let existing = self
            .tx
            .find_source(self.connection, &record.provider_id)
            .await?;
        if let Some(existing) = existing {
            if existing.provider_account != self.provider_account {
                return Err(SyncError::Invalid(RECORD_MOVED));
            }
            self.tx
                .update_source(existing.id, record.update(self.now))
                .await?;
        } else {
            self.insert_source(&record, None).await?;
            self.report.held += 1;
        }
        Ok(())
    }

    async fn insert_source(
        &mut self,
        record: &Record,
        transaction: Option<TransactionId>,
    ) -> Result<SourceRecordId, SyncError> {
        let source = self
            .tx
            .insert_source(NewSource {
                connection: self.connection,
                provider_account: self.provider_account,
                provider_txn_id: record.provider_id.clone(),
                transaction,
                fields: record.fields.clone(),
                amount_raw: record.amount_raw.clone(),
                seen_at: self.now,
            })
            .await?;
        Ok(source.id)
    }

    /// Step 2 for one record of a bound provider account.
    async fn match_record(
        &mut self,
        account: &AccountRecord,
        record: Record,
    ) -> Result<(), SyncError> {
        // Same connection and provider id: the same transaction.
        if let Some(existing) = self
            .tx
            .find_source(self.connection, &record.provider_id)
            .await?
        {
            if existing.provider_account != self.provider_account {
                return Err(SyncError::Invalid(RECORD_MOVED));
            }
            self.tx
                .update_source(existing.id, record.update(self.now))
                .await?;
            return match existing.transaction {
                Some(transaction) => {
                    let current = self.tx.get_transaction(transaction).await?;
                    let changes = self
                        .imported_changes(&current, &record.fields, AuditChanges::new())
                        .await?;
                    if !changes.is_empty() {
                        self.audit("transaction.imported_update", transaction, changes)
                            .await?;
                        self.report.updated += 1;
                    }
                    Ok(())
                }
                None => self.place(account, record, Some(existing.id)).await,
            };
        }
        // A record naming the one it replaces joins that one's transaction.
        if let Some(replaced) = &record.replaces
            && let Some(source) = self.tx.find_source(self.connection, replaced).await?
            && source.provider_account == self.provider_account
            && let Some(transaction) = source.transaction
        {
            return self.attach(transaction, &record, None).await;
        }
        self.place(account, record, None).await
    }

    /// Matches `record` against the account's imported transactions:
    /// attaches it, or creates a transaction (flagged when ambiguous).
    /// `held` is its existing, held source record, if any.
    async fn place(
        &mut self,
        account: &AccountRecord,
        record: Record,
        held: Option<SourceRecordId>,
    ) -> Result<(), SyncError> {
        let tolerance = Span::new().days(DATE_TOLERANCE_DAYS);
        let posted_on = record.fields.posted_on;
        let from = posted_on
            .checked_sub(tolerance)
            .map_err(|_| SyncError::Invalid(DATE_OUT_OF_RANGE))?;
        let to = posted_on
            .checked_add(tolerance)
            .map_err(|_| SyncError::Invalid(DATE_OUT_OF_RANGE))?;
        let rows = self.tx.match_candidates(account.id, from, to).await?;
        let candidates: Vec<Candidate> = rows.iter().map(|row| self.candidate(row)).collect();
        let incoming = Incoming {
            posted_on,
            amount_minor: record.fields.amount_minor,
            description_key: description_key(record.fields.description.as_str()),
        };
        match decide(&incoming, &candidates) {
            MatchDecision::Attach(transaction) => self.attach(transaction, &record, held).await,
            MatchDecision::NewPossibleDuplicate(others) => {
                self.create(account, &record, held, &others).await
            }
            MatchDecision::New => {
                // A record beside its other form (pending beside posted, in
                // either order) is flagged, never a silent second
                // transaction.
                let pending = record.fields.pending;
                let others: Vec<Candidate> = rows
                    .iter()
                    .zip(&candidates)
                    .filter(|(row, candidate)| {
                        let mut live = row
                            .sources
                            .iter()
                            .filter(|source| !self.vanished.contains(&source.id))
                            .peekable();
                        candidate.live_source_from_same_connection
                            && live.peek().is_some()
                            && live.all(|source| source.fields.pending != pending)
                    })
                    .map(|(_, candidate)| candidate.clone())
                    .collect();
                let others = if pending {
                    posted_counterparts(&incoming, &others)
                } else {
                    pending_counterparts(&incoming, &others)
                };
                self.create(account, &record, held, &others).await
            }
        }
    }

    fn candidate(&self, row: &CandidateRow) -> Candidate {
        let live = |source: &&SourceRecord| !self.vanished.contains(&source.id);
        Candidate {
            transaction: row.transaction,
            posted_on: row.posted_on,
            amount_minor: row.amount_minor,
            description_key: description_key(row.description.as_str()),
            live_source_from_same_connection: row
                .sources
                .iter()
                .filter(live)
                .any(|source| source.connection == self.connection),
            only_vanished_pending: !row.sources.is_empty()
                && row
                    .sources
                    .iter()
                    .all(|source| source.fields.pending && self.vanished.contains(&source.id)),
        }
    }

    /// Creates an imported transaction for `record`, flagged as a possible
    /// duplicate of `others` when there are any.
    async fn create(
        &mut self,
        account: &AccountRecord,
        record: &Record,
        held: Option<SourceRecordId>,
        others: &[TransactionId],
    ) -> Result<(), SyncError> {
        let fields = &record.fields;
        let created = self
            .tx
            .insert_transaction(NewTransaction {
                origin: TransactionOrigin::Imported,
                account: account.id,
                posted_on: fields.posted_on,
                transacted_on: fields.transacted_on,
                pending: fields.pending,
                amount_minor: fields.amount_minor,
                currency: account.currency,
                raw_description: fields.description.clone(),
                imported_payee: fields.payee.clone(),
                memo: fields.memo.clone(),
                notes: None,
                created_at: self.now,
            })
            .await?;
        let source = match held {
            Some(source) => {
                self.tx.attach_source(source, created.id).await?;
                source
            }
            None => self.insert_source(record, Some(created.id)).await?,
        };
        let mut changes = AuditChanges::new()
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
            .set("source", source)
            .set("provider_account", self.provider_account);
        if !others.is_empty() {
            self.tx
                .set_flag(created.id, TransactionFlag::PossibleDuplicate, true)
                .await?;
            self.tx.add_duplicate_candidates(created.id, others).await?;
            let listed: Vec<String> = others.iter().map(ToString::to_string).collect();
            changes = changes
                .set("possible_duplicate", true)
                .set("duplicate_candidates", listed.join(","));
            self.report.possible_duplicates += 1;
        }
        self.audit("transaction.import", created.id, changes)
            .await?;
        self.report.created += 1;
        Ok(())
    }

    /// Attaches `record` to `transaction` (its held source when `held`),
    /// applying its imported fields and clearing `SourceRemoved`.
    async fn attach(
        &mut self,
        transaction: TransactionId,
        record: &Record,
        held: Option<SourceRecordId>,
    ) -> Result<(), SyncError> {
        let current = self.tx.get_transaction(transaction).await?;
        let source = match held {
            Some(source) => {
                self.tx.attach_source(source, transaction).await?;
                source
            }
            None => self.insert_source(record, Some(transaction)).await?,
        };
        let mut changes = AuditChanges::new()
            .set("source", source)
            .set("provider_account", self.provider_account);
        changes = self
            .imported_changes(&current, &record.fields, changes)
            .await?;
        if current.flags.contains(&TransactionFlag::SourceRemoved) {
            self.tx
                .set_flag(transaction, TransactionFlag::SourceRemoved, false)
                .await?;
            changes = changes.change("source_removed", true, false);
        }
        self.audit("transaction.attach_source", transaction, changes)
            .await?;
        self.report.attached += 1;
        Ok(())
    }

    /// Applies `fields` to `current`'s imported fields, appending what
    /// changed to `changes`. A pending record never overwrites a posted
    /// transaction. User fields never change, except that a single split
    /// follows a new amount; several splits stay, and the transaction is
    /// flagged `SplitMismatch` while they no longer sum to it.
    async fn imported_changes(
        &mut self,
        current: &TransactionRecord,
        fields: &ImportedFields,
        mut changes: AuditChanges,
    ) -> Result<AuditChanges, SyncError> {
        if !current.pending && fields.pending {
            return Ok(changes);
        }
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
        self.tx
            .update_imported_fields(current.id, fields, self.now)
            .await?;
        if current.amount_minor != fields.amount_minor {
            changes = self
                .follow_amount(current, fields.amount_minor, changes)
                .await?;
        }
        Ok(changes)
    }

    /// Keeps the splits of `current` consistent with its new amount.
    async fn follow_amount(
        &mut self,
        current: &TransactionRecord,
        amount_minor: i64,
        mut changes: AuditChanges,
    ) -> Result<AuditChanges, SyncError> {
        let mismatch = match current.splits.as_slice() {
            [] => false,
            // A split is never zero: a single split cannot follow a zero
            // amount, so it stays and the transaction is flagged.
            [only] if amount_minor != 0 => {
                self.tx
                    .replace_splits(
                        current.id,
                        &[SplitInput {
                            category: only.category,
                            minor_units: amount_minor,
                        }],
                        sync_attribution(self.now),
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
            self.tx
                .set_flag(current.id, TransactionFlag::SplitMismatch, mismatch)
                .await?;
            changes = changes.change("split_mismatch", flagged, mismatch);
        }
        Ok(changes)
    }

    /// Records that `removed`, a deleted transaction, is no longer a
    /// candidate of `transaction`, whose `PossibleDuplicate` flag clears
    /// once it has no candidate left.
    async fn candidate_removed(
        &mut self,
        transaction: TransactionId,
        removed: TransactionId,
    ) -> Result<(), SyncError> {
        let current = self.tx.get_transaction(transaction).await?;
        let mut changes = AuditChanges::new().unset("duplicate_candidate", removed);
        if current.duplicate_candidates.is_empty()
            && current.flags.contains(&TransactionFlag::PossibleDuplicate)
        {
            self.tx
                .set_flag(transaction, TransactionFlag::PossibleDuplicate, false)
                .await?;
            changes = changes.change("possible_duplicate", true, false);
        }
        self.audit("transaction.candidate_removed", transaction, changes)
            .await
    }

    /// Step 3 for one vanished record: deletes it; a transaction left
    /// without records is deleted when it holds no user state, otherwise
    /// kept and flagged `SourceRemoved`.
    async fn remove(&mut self, source: SourceRecord) -> Result<(), SyncError> {
        self.tx.delete_source(source.id).await?;
        let Some(transaction) = source.transaction else {
            return Ok(());
        };
        if !self.tx.sources_of(transaction).await?.is_empty() {
            let changes = AuditChanges::new().unset("source", source.id);
            return self
                .audit("transaction.detach_source", transaction, changes)
                .await;
        }
        let current = self.tx.get_transaction(transaction).await?;
        if current.user_state().is_empty() {
            let listing = self.tx.listed_as_candidate(transaction).await?;
            self.tx.delete_transaction(transaction).await?;
            let changes = AuditChanges::new()
                .unset("account", current.account)
                .unset("origin", current.origin.as_str())
                .unset("posted_on", current.posted_on)
                .unset("amount_minor", current.amount_minor)
                .unset("description", current.raw_description.as_str())
                .unset("source", source.id);
            self.audit("transaction.delete", transaction, changes)
                .await?;
            self.report.deleted += 1;
            for other in listing {
                self.candidate_removed(other, transaction).await?;
            }
        } else if !current.flags.contains(&TransactionFlag::SourceRemoved) {
            self.tx
                .set_flag(transaction, TransactionFlag::SourceRemoved, true)
                .await?;
            let changes = AuditChanges::new().unset("source", source.id).change(
                "source_removed",
                false,
                true,
            );
            self.audit("transaction.source_removed", transaction, changes)
                .await?;
            self.report.source_removed += 1;
        }
        Ok(())
    }
}
