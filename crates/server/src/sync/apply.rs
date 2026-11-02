//! The apply step: one provider account's fetched records, matched onto
//! canonical transactions inside the caller's transaction (spec §6.2,
//! §8.4).
//!
//! The order matters. Records that vanished are only computed first; every
//! incoming record is then matched (so a posted record can still attach to
//! the pending one it replaces); only then are the vanished records
//! removed. Every change to a canonical transaction is audited as
//! `system:sync`; a record that changes nothing writes no audit entry.
//!
//! A transaction fed by several connections shows the imported fields of the
//! one that owns them (spec §8.4). A record of another connection is
//! kept up to date as a source and changes nothing else, so two providers
//! that describe one purchase differently never rewrite it in turn.

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashSet};

use coffer_domain::audit::{AuditActor, AuditChanges, AuditEntry};
use coffer_domain::ids::{ConnectionId, ProviderAccountId, SourceRecordId, TransactionId};
use coffer_domain::matching::{
    Candidate, DATE_TOLERANCE_DAYS, Incoming, MatchDecision, decide, description_key,
    pending_counterparts, posted_counterparts,
};
use coffer_domain::transactions::{Description, Memo, Payee, TransactionFlag, TransactionOrigin};
use coffer_store::{
    AccountRecord, Attribution, CandidateRow, ConnectionStatus, ImportedFields, NewSource,
    NewTransaction, ProviderAccountRecord, SourceRecord, SourceUpdate, StoreTx, TransactionRecord,
};
use jiff::civil::Date;
use jiff::{Span, Timestamp};

use super::SyncError;
use crate::canonical::{apply_imported_fields, drop_candidate, drop_stale_candidates};

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

/// Whether `id` can identify a provider record or account: 1 to
/// [`MAX_PROVIDER_ID_CHARS`] characters without control characters. The one
/// rule for every id a provider sends, so none that fails it can reach
/// storage and fail there for good.
pub(crate) fn valid_provider_id(id: &str) -> bool {
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
    /// `false` when the provider gave no date: `posted_on` is then the
    /// day of the sync, and a known pending record keeps its stored day.
    pub date_reported: bool,
    pub transacted_on: Option<Date>,
    /// Negative is money out of the account.
    pub amount_minor: i64,
    /// The amount exactly as the provider wrote it, in its own sign:
    /// `amount_minor` alone follows coffer's.
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
    /// Records left out before this step because they could not be used,
    /// by provider id when known: counted as skipped, and a known one
    /// still counts as reported.
    pub unusable: Vec<Option<String>>,
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
    /// provider id, an amount that is not a decimal text, or a record the
    /// caller could not use).
    pub skipped: u32,
}

/// An incoming record with its texts sanitized.
struct Record {
    provider_id: String,
    fields: ImportedFields,
    /// See [`NormalizedTransaction::date_reported`].
    date_reported: bool,
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
            date_reported: normalized.date_reported,
            amount_raw: normalized.amount_raw,
            replaces: normalized.replaces_provider_id,
        })
    }

    fn from_held(source: SourceRecord) -> Self {
        Self {
            provider_id: source.provider_txn_id,
            fields: source.fields,
            date_reported: true,
            amount_raw: source.amount_raw,
            replaces: None,
        }
    }

    /// A pending record re-reported without a date keeps the day its known
    /// pending form was stored with, so it does not move every sync.
    fn keep_known_date(&mut self, known: &SourceRecord) {
        if !self.date_reported && self.fields.pending && known.fields.pending {
            self.fields.posted_on = known.fields.posted_on;
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
    // Every well-identified record counts as reported, even one skipped
    // for another defect, so a malformed re-report never makes a known
    // record vanish.
    let present: HashSet<String> = batch
        .transactions
        .iter()
        .map(|normalized| &normalized.provider_id)
        .chain(batch.unusable.iter().flatten())
        .filter(|provider_id| valid_provider_id(provider_id))
        .cloned()
        .collect();
    let total = batch.transactions.len() + batch.unusable.len();
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

/// The connection owning the imported fields of a transaction with these
/// live sources (spec §8.4): that of the earliest posted record, or without
/// one, of the earliest record. Earliest is by first sighting, the record id
/// settling a tie, so the answer depends only on what is stored.
fn owner_of(live: &[&SourceRecord]) -> Option<ConnectionId> {
    live.iter()
        .min_by_key(|source| (source.fields.pending, source.first_seen_at, source.id))
        .map(|source| source.connection)
}

/// Who the sync's audit entries are attributed to.
pub(crate) fn sync_actor() -> AuditActor {
    AuditActor::System {
        reason: SYNC_REASON.to_owned(),
    }
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
    /// Those of `sources` that can own imported fields: not about to vanish,
    /// and of a connection that is not removed and so can still report.
    async fn live_sources<'s>(
        &mut self,
        sources: &'s [SourceRecord],
    ) -> Result<Vec<&'s SourceRecord>, SyncError> {
        let mut checked = HashSet::from([self.connection]);
        let mut removed = HashSet::new();
        for source in sources {
            if checked.insert(source.connection)
                && self.tx.get_connection(source.connection).await?.status
                    == ConnectionStatus::Removed
            {
                removed.insert(source.connection);
            }
        }
        Ok(sources
            .iter()
            .filter(|source| {
                !self.vanished.contains(&source.id) && !removed.contains(&source.connection)
            })
            .collect())
    }

    /// Whether this apply's connection owns the imported fields of a
    /// transaction with `sources`.
    async fn owns_imported_fields(&mut self, sources: &[SourceRecord]) -> Result<bool, SyncError> {
        let live = self.live_sources(sources).await?;
        Ok(owner_of(&live) == Some(self.connection))
    }

    /// Forgets the stale candidate pairs of `transaction`, which just
    /// gained a posted record (see [`crate::canonical`]). Records
    /// about to vanish do not count.
    async fn drop_stale_candidates(&mut self, transaction: TransactionId) -> Result<(), SyncError> {
        Ok(drop_stale_candidates(
            self.tx,
            self.now,
            &sync_actor(),
            transaction,
            &self.vanished,
        )
        .await?)
    }

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
                    actor: sync_actor(),
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
    async fn hold(&mut self, mut record: Record) -> Result<(), SyncError> {
        let existing = self
            .tx
            .find_source(self.connection, &record.provider_id)
            .await?;
        if let Some(existing) = existing {
            if existing.provider_account != self.provider_account {
                return Err(SyncError::Invalid(RECORD_MOVED));
            }
            record.keep_known_date(&existing);
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
        mut record: Record,
    ) -> Result<(), SyncError> {
        // Same connection and provider id: the same transaction. Only a
        // record whose reported fields changed changes it: among several
        // records of one connection (as after a merge) it shows whichever
        // one it last applied, and records re-reported unchanged never
        // rewrite it. Another connection's record changes nothing at all.
        if let Some(existing) = self
            .tx
            .find_source(self.connection, &record.provider_id)
            .await?
        {
            if existing.provider_account != self.provider_account {
                return Err(SyncError::Invalid(RECORD_MOVED));
            }
            record.keep_known_date(&existing);
            self.tx
                .update_source(existing.id, record.update(self.now))
                .await?;
            return match existing.transaction {
                Some(_) if existing.fields == record.fields => Ok(()),
                Some(transaction) => {
                    let current = self.tx.get_transaction(transaction).await?;
                    if self.owns_imported_fields(&current.sources).await? {
                        self.update_imported_fields(&current, &record.fields)
                            .await?;
                    }
                    if existing.fields.pending && !record.fields.pending {
                        self.drop_stale_candidates(transaction).await?;
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
            // `others` are matched within this account, so the store's
            // same-account check cannot refuse them; if it ever did, the
            // error rolls the whole sync back.
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
        let sources = self.tx.sources_of(transaction).await?;
        if self.owns_imported_fields(&sources).await? {
            changes = self
                .imported_changes(&current, &record.fields, changes)
                .await?;
        }
        if current.flags.contains(&TransactionFlag::SourceRemoved) {
            self.tx
                .set_flag(transaction, TransactionFlag::SourceRemoved, false)
                .await?;
            changes = changes.change("source_removed", true, false);
        }
        self.audit("transaction.attach_source", transaction, changes)
            .await?;
        self.report.attached += 1;
        self.drop_stale_candidates(transaction).await
    }

    /// Applies `fields` to `current`'s imported fields as an audited update
    /// of its own, if they change anything.
    async fn update_imported_fields(
        &mut self,
        current: &TransactionRecord,
        fields: &ImportedFields,
    ) -> Result<(), SyncError> {
        let changes = self
            .imported_changes(current, fields, AuditChanges::new())
            .await?;
        if !changes.is_empty() {
            self.audit("transaction.imported_update", current.id, changes)
                .await?;
            self.report.updated += 1;
        }
        Ok(())
    }

    /// Hands the imported fields of `transaction` to the next owner, if
    /// losing `gone` changed who owns them. `remaining` are its sources now.
    async fn follow_new_owner(
        &mut self,
        transaction: TransactionId,
        gone: &SourceRecord,
        remaining: &[SourceRecord],
    ) -> Result<(), SyncError> {
        let live = self.live_sources(remaining).await?;
        let Some(owner) = owner_of(&live) else {
            return Ok(());
        };
        let mut before = live.clone();
        before.push(gone);
        if owner_of(&before) == Some(owner) {
            return Ok(());
        }
        // Its latest record, a posted one first: what it would have applied.
        let latest = live
            .iter()
            .filter(|source| source.connection == owner)
            .min_by_key(|source| {
                (
                    source.fields.pending,
                    Reverse(source.first_seen_at),
                    Reverse(source.id),
                )
            });
        if let Some(latest) = latest {
            let current = self.tx.get_transaction(transaction).await?;
            self.update_imported_fields(&current, &latest.fields)
                .await?;
        }
        Ok(())
    }

    /// Applies `fields` to `current`'s imported fields, appending what
    /// changed to `changes`. A pending record never overwrites a posted
    /// transaction.
    async fn imported_changes(
        &mut self,
        current: &TransactionRecord,
        fields: &ImportedFields,
        changes: AuditChanges,
    ) -> Result<AuditChanges, SyncError> {
        if !current.pending && fields.pending {
            return Ok(changes);
        }
        Ok(apply_imported_fields(
            self.tx,
            current,
            fields,
            changes,
            &sync_attribution(self.now),
        )
        .await?)
    }

    /// Step 3 for one vanished record: deletes it; a transaction left
    /// without records is deleted when it holds no user state, otherwise
    /// kept and flagged `SourceRemoved`.
    async fn remove(&mut self, source: SourceRecord) -> Result<(), SyncError> {
        self.tx.delete_source(source.id).await?;
        let Some(transaction) = source.transaction else {
            return Ok(());
        };
        let remaining = self.tx.sources_of(transaction).await?;
        if !remaining.is_empty() {
            let changes = AuditChanges::new().unset("source", source.id);
            self.audit("transaction.detach_source", transaction, changes)
                .await?;
            return self
                .follow_new_owner(transaction, &source, &remaining)
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
                drop_candidate(self.tx, self.now, &sync_actor(), other, transaction).await?;
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
