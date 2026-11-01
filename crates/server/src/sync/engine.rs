//! The sync engine (spec §6.2): performs a queued run of one connection.
//!
//! A run opens the connection's credentials only for the provider call,
//! asks for a window computed from when each provider account was last
//! applied and the household's `today`, then applies each provider account
//! in its own store transaction, so a failing account leaves nothing of
//! itself behind, never blocks the others, and is fetched again from where
//! it stopped. The outcome is recorded on the connection, its
//! provider accounts and the run. Every provider text is sanitized before
//! it is stored, and nothing about the credentials is ever logged.

use std::sync::Arc;

use coffer_domain::accounts::BalanceSource;
use coffer_domain::audit::{AuditChanges, AuditEntry};
use coffer_domain::ids::{AccountId, ConnectionId, SyncRunId};
use coffer_domain::money::{Currency, DecimalAmount};
use coffer_domain::sync::{SyncRunStatus, SyncTrigger};
use coffer_domain::time::Clock;
use coffer_store::{
    AccountRecord, BalanceSnapshot, ConnectionRecord, ConnectionStatus, ConnectionStatusChange,
    ProviderAccountRecord, ProviderAccountUpsert, RawBalance, Store, StoreError, StoreTx,
    SyncRunRecord,
};
use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{Span, Timestamp};
use serde::Serialize;
use utoipa::ToSchema;

use super::SyncError;
use super::apply::{
    AccountBatch, ApplyReport, MAX_PROVIDER_ID_CHARS, NormalizedTransaction, apply_account_batch,
    sync_actor,
};
use super::provider::{
    FetchMode, FetchRequest, FetchResult, MAX_PROVIDER_MESSAGE_CHARS, ProviderAccountData,
    ProviderError, ProviderKind, ProviderRegistry, ProviderTransaction, sanitize_provider_message,
};
use crate::auth::AccessResolver;
use crate::config::Config;
use crate::ledger::transactions::AttributionView;
use crate::vault::{CredentialVault, SealedCredentials};

const RUN_FINISHED: &str = "the sync run has already finished";
pub(super) const CONNECTION_REMOVED: &str = "the connection is removed";
pub(super) const PROVIDER_NOT_OFFERED: &str =
    "the connection's provider is not offered by this server";
const ACCOUNT_ERROR: &str = "the provider reported an error for this account";
const UNSUPPORTED_CURRENCY: &str =
    "the provider reported a currency that is not a supported ISO 4217 code";
const CURRENCY_DIFFERS: &str = "the provider reported a currency other than the bound account's";
const BALANCE_UNREPRESENTABLE: &str =
    "the provider reported a balance with more decimals than the currency allows, or too large";
const ACCOUNT_NOT_STORED: &str = "the account's data could not be stored; the next sync fetches \
     it again from shortly before it was last stored";
const INTERNAL_FAILURE: &str = "the sync failed on the server";

/// A sync run, as the API shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct SyncRunView {
    pub id: SyncRunId,
    pub connection: ConnectionId,
    pub trigger: SyncTrigger,
    pub status: SyncRunStatus,
    /// Who asked for it; `null` for the schedule.
    #[schema(required)]
    pub requested_by: Option<AttributionView>,
    pub queued_at: Timestamp,
    #[schema(required)]
    pub started_at: Option<Timestamp>,
    #[schema(required)]
    pub finished_at: Option<Timestamp>,
    /// What happened: counts for a run that applied data, the sanitized
    /// error for one that failed. Never a credential.
    #[schema(required)]
    pub detail: Option<String>,
}

impl From<SyncRunRecord> for SyncRunView {
    fn from(run: SyncRunRecord) -> Self {
        Self {
            id: run.id,
            connection: run.connection,
            trigger: run.trigger,
            status: run.status,
            requested_by: run.requested_by.map(|(member, agent)| AttributionView {
                member: Some(member),
                agent,
                system: None,
                at: run.queued_at,
            }),
            queued_at: run.queued_at,
            started_at: run.started_at,
            finished_at: run.finished_at,
            detail: run.detail,
        }
    }
}

/// Performs sync runs and manages connections and their provider accounts
/// (see [`super::connections`]).
pub struct SyncService {
    pub(super) store: Store,
    pub(super) clock: Arc<dyn Clock>,
    pub(super) vault: Arc<CredentialVault>,
    pub(super) registry: Arc<ProviderRegistry>,
    /// The only source of account access.
    pub(super) access: Arc<AccessResolver>,
    pub(super) config: Config,
    /// Wakes the scheduler's worker when a run is queued.
    pub(super) queued: tokio::sync::Notify,
}

impl std::fmt::Debug for SyncService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncService")
            .field("registry", &self.registry)
            .finish_non_exhaustive()
    }
}

/// How one provider account fared.
enum AccountOutcome {
    Applied(ApplyReport),
    Failed,
    /// The connection was removed before the account could be applied:
    /// nothing of it was written.
    ConnectionRemoved,
}

/// Why a run could not use its connection's provider.
enum Failure {
    /// The connection is removed: the run fails, the connection stays as
    /// it is.
    Removed,
    /// The provider call could not be made or failed: recorded on the
    /// connection with this status and text.
    Connection {
        status: ConnectionStatus,
        text: String,
    },
}

/// The totals of a run's applied accounts.
#[derive(Default)]
struct Totals {
    accounts: u32,
    failed_accounts: u32,
    report: ApplyReport,
    /// Whether the connection was removed while the run applied its data.
    connection_removed: bool,
}

impl Totals {
    fn add(&mut self, outcome: &AccountOutcome) {
        if matches!(outcome, AccountOutcome::ConnectionRemoved) {
            self.connection_removed = true;
            return;
        }
        self.accounts += 1;
        match outcome {
            AccountOutcome::Applied(report) => {
                let total = &mut self.report;
                total.created += report.created;
                total.possible_duplicates += report.possible_duplicates;
                total.attached += report.attached;
                total.updated += report.updated;
                total.deleted += report.deleted;
                total.source_removed += report.source_removed;
                total.held += report.held;
                total.skipped += report.skipped;
            }
            AccountOutcome::Failed => self.failed_accounts += 1,
            AccountOutcome::ConnectionRemoved => {}
        }
    }

    fn detail(&self, connection_errors: bool) -> String {
        let report = &self.report;
        let mut detail = format!(
            "{} {}, {} with errors; {} created, {} attached, {} updated, {} deleted, \
             {} held, {} skipped",
            self.accounts,
            if self.accounts == 1 {
                "account"
            } else {
                "accounts"
            },
            self.failed_accounts,
            report.created,
            report.attached,
            report.updated,
            report.deleted,
            report.held,
            report.skipped,
        );
        if connection_errors {
            detail.push_str("; the provider reported errors");
        }
        detail
    }
}

impl SyncService {
    /// The service over `store`, opening credentials with `vault`,
    /// reaching providers through `registry` and resolving account access
    /// with `access`.
    #[must_use]
    pub fn new(
        store: Store,
        clock: Arc<dyn Clock>,
        vault: Arc<CredentialVault>,
        registry: Arc<ProviderRegistry>,
        access: Arc<AccessResolver>,
        config: Config,
    ) -> Self {
        Self {
            store,
            clock,
            vault,
            registry,
            access,
            config,
            queued: tokio::sync::Notify::new(),
        }
    }

    /// Performs the queued run `run` and returns it finished. A provider
    /// failure is an outcome, recorded on the run and the connection, not an
    /// error.
    ///
    /// # Errors
    ///
    /// [`SyncError::NotFound`] for an unknown run; [`SyncError::InProgress`]
    /// when it is already running; [`SyncError::Invalid`] when it has
    /// finished; storage failures, after which the run is marked failed when
    /// the store allows.
    pub async fn run(&self, run: SyncRunId) -> Result<SyncRunView, SyncError> {
        let started_at = self.clock.now();
        let mut tx = self.store.begin().await?;
        let started = match tx.start_sync_run(run, started_at).await {
            Ok(started) => started,
            Err(StoreError::Conflict(_)) => {
                drop(tx);
                let current = self.store.get_sync_run(run).await?;
                return Err(if current.status == SyncRunStatus::Running {
                    SyncError::InProgress
                } else {
                    SyncError::Invalid(RUN_FINISHED)
                });
            }
            Err(err) => return Err(err.into()),
        };
        tx.commit().await?;
        tracing::info!(
            run = %run,
            connection = %started.connection,
            trigger = started.trigger.as_str(),
            "sync run started"
        );
        match self.perform(&started, started_at).await {
            Ok(finished) => {
                tracing::info!(
                    run = %run,
                    connection = %finished.connection,
                    status = finished.status.as_str(),
                    "sync run finished"
                );
                Ok(SyncRunView::from(finished))
            }
            Err(err) => {
                tracing::error!(run = %run, connection = %started.connection, error = ?err, "sync run failed");
                self.abandon(run).await;
                Err(err)
            }
        }
    }

    /// Marks `run` failed after a storage error or a panic, whether it is
    /// still queued or running, if the store allows; a run that already
    /// finished is left as it is.
    pub(super) async fn abandon(&self, run: SyncRunId) {
        let finished = async {
            let mut tx = self.store.begin().await?;
            tx.fail_sync_run(run, INTERNAL_FAILURE, self.clock.now())
                .await?;
            tx.commit().await
        }
        .await;
        if let Err(err) = finished {
            tracing::error!(run = %run, error = ?err, "could not mark the sync run failed");
        }
    }

    async fn perform(
        &self,
        run: &SyncRunRecord,
        started_at: Timestamp,
    ) -> Result<SyncRunRecord, SyncError> {
        let connection = self.store.get_connection(run.connection).await?;
        let applied = self
            .store
            .reported_provider_accounts_applied(connection.id)
            .await?;
        let today = self.today();
        match self.fetch(&connection, &applied, today).await {
            Ok((result, mode)) => {
                self.apply(run, &connection, result, mode, today, started_at)
                    .await
            }
            Err(failure) => {
                self.record_failure(run, &connection, failure, started_at)
                    .await
            }
        }
    }

    /// The household's current day.
    fn today(&self) -> Date {
        self.clock
            .now()
            .to_zoned(self.config.timezone.clone())
            .date()
    }

    /// Calls the provider with the connection's credentials, opened only
    /// for the call. `applied` is when each provider account it still
    /// reports was last applied (see [`window_start`]).
    async fn fetch(
        &self,
        connection: &ConnectionRecord,
        applied: &[Option<Timestamp>],
        today: Date,
    ) -> Result<(FetchResult, FetchMode), Failure> {
        let sealed = match (&connection.credentials, connection.status) {
            (_, ConnectionStatus::Removed) | (None, _) => return Err(Failure::Removed),
            (Some(sealed), _) => SealedCredentials::from(sealed.clone()),
        };
        let provider = connection
            .provider
            .parse::<ProviderKind>()
            .ok()
            .and_then(|kind| self.registry.get(kind))
            .ok_or_else(|| Failure::Connection {
                status: ConnectionStatus::Error,
                text: PROVIDER_NOT_OFFERED.to_owned(),
            })?;
        let mode = provider.capabilities().mode;
        let (window_start, cursor) = match mode {
            FetchMode::Window {
                max_days,
                overlap_days,
            } => (
                Some(window_start(
                    applied,
                    connection.last_success_at,
                    today,
                    &self.config.timezone,
                    max_days,
                    overlap_days,
                )),
                None,
            ),
            FetchMode::Cursor => (None, connection.provider_state.as_deref()),
        };
        let fetched = {
            let credentials = self
                .vault
                .open(connection.id, &sealed)
                .map_err(|err| failure(&SyncError::Vault(err)))?;
            provider
                .fetch(FetchRequest {
                    credentials: &credentials,
                    window_start,
                    cursor,
                    today,
                    timezone: &self.config.timezone,
                })
                .await
        };
        fetched
            .map(|result| (result, mode))
            .map_err(|err| failure(&SyncError::Provider(err)))
    }

    async fn record_failure(
        &self,
        run: &SyncRunRecord,
        connection: &ConnectionRecord,
        failure: Failure,
        started_at: Timestamp,
    ) -> Result<SyncRunRecord, SyncError> {
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let detail = match failure {
            Failure::Removed => CONNECTION_REMOVED.to_owned(),
            Failure::Connection { status, text } => {
                let stored = tx.get_connection(connection.id).await?;
                if still_current(&stored, connection) {
                    tx.update_connection_status(
                        connection.id,
                        ConnectionStatusChange {
                            status,
                            last_error: Some(text.clone()),
                            attempted_at: Some(started_at),
                            succeeded_at: None,
                            at: now,
                        },
                    )
                    .await?;
                }
                text
            }
        };
        let finished = tx
            .finish_sync_run(run.id, SyncRunStatus::Failed, Some(detail), now)
            .await?;
        tx.commit().await?;
        Ok(finished)
    }

    async fn apply(
        &self,
        run: &SyncRunRecord,
        connection: &ConnectionRecord,
        result: FetchResult,
        mode: FetchMode,
        today: Date,
        started_at: Timestamp,
    ) -> Result<SyncRunRecord, SyncError> {
        let mut totals = Totals::default();
        for data in result.accounts {
            let outcome = self.apply_account(connection.id, data, today).await;
            totals.add(&outcome);
            if totals.connection_removed {
                // Removed while the run was fetching or applying: nothing
                // more is written for it.
                break;
            }
        }
        if totals.report.skipped > 0 {
            tracing::warn!(
                connection = %connection.id,
                skipped = totals.report.skipped,
                "skipped malformed provider records"
            );
        }
        if totals.failed_accounts > 0 {
            tracing::warn!(
                connection = %connection.id,
                failed = totals.failed_accounts,
                "provider accounts not applied"
            );
        }

        let connection_errors = joined_errors(&result.connection_errors);
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let stored = tx.get_connection(connection.id).await?;
        let removed = totals.connection_removed || stored.status == ConnectionStatus::Removed;
        let (status, detail) = outcome(&totals, removed, connection_errors.is_some());
        if !removed && still_current(&stored, connection) {
            tx.update_connection_status(
                connection.id,
                ConnectionStatusChange {
                    status: ConnectionStatus::Active,
                    last_error: connection_errors,
                    attempted_at: Some(started_at),
                    succeeded_at: Some(now),
                    at: now,
                },
            )
            .await?;
            if let (FetchMode::Cursor, Some(cursor)) = (mode, result.cursor) {
                tx.set_provider_state(connection.id, Some(cursor)).await?;
            }
        }
        let finished = tx
            .finish_sync_run(run.id, status, Some(detail), now)
            .await?;
        tx.commit().await?;
        Ok(finished)
    }

    /// Applies one provider account in its own transaction. On a failure
    /// nothing of it is committed, and the error is recorded on it in a
    /// second transaction.
    async fn apply_account(
        &self,
        connection: ConnectionId,
        data: ProviderAccountData,
        today: Date,
    ) -> AccountOutcome {
        if !valid_provider_account_id(&data.provider_account_id) {
            // Nothing can be recorded on an account without a usable id:
            // the run counts it among the accounts with errors.
            tracing::warn!(
                connection = %connection,
                "provider account with an invalid id skipped"
            );
            return AccountOutcome::Failed;
        }
        let now = self.clock.now();
        let upsert = ProviderAccountUpsert {
            connection,
            provider_account_id: data.provider_account_id.clone(),
            name: sanitize_provider_message(&data.name),
            institution: data
                .institution
                .as_deref()
                .map(sanitize_provider_message)
                .filter(|institution| !institution.is_empty()),
            currency_raw: sanitize_provider_message(&data.currency),
            kind_hint: data.kind_hint,
            balance: None,
            seen_at: now,
        };
        match self.try_apply_account(upsert.clone(), data, today).await {
            Ok(outcome) => outcome,
            Err(err) => {
                tracing::warn!(connection = %connection, error = ?err, "provider account not applied");
                if let Err(err) = self.record_account_failure(upsert).await {
                    tracing::error!(
                        connection = %connection,
                        error = ?err,
                        "could not record a provider account's failure"
                    );
                }
                AccountOutcome::Failed
            }
        }
    }

    async fn try_apply_account(
        &self,
        mut upsert: ProviderAccountUpsert,
        data: ProviderAccountData,
        today: Date,
    ) -> Result<AccountOutcome, SyncError> {
        let now = upsert.seen_at;
        let connection = upsert.connection;
        upsert.balance = data.balance.as_ref().map(|(on, amount)| RawBalance {
            on: *on,
            amount_raw: amount.as_str().to_owned(),
        });
        let mut tx = self.store.begin().await?;
        // Checked in the transaction that writes, so a removal that commits
        // first is always seen and a removal that comes later waits for it.
        if tx.get_connection(connection).await?.status == ConnectionStatus::Removed {
            return Ok(AccountOutcome::ConnectionRemoved);
        }
        let record = tx.upsert_provider_account(upsert).await?;

        let checked = check_account(&mut tx, &record, &data).await?;
        let (account, currency) = match checked {
            Ok(checked) => checked,
            Err(error) => {
                tx.set_provider_account_error(record.id, Some(error))
                    .await?;
                tx.commit().await?;
                return Ok(AccountOutcome::Failed);
            }
        };
        let Some(Converted {
            transactions,
            unusable,
            balance,
        }) = convert(
            data.transactions,
            data.unusable,
            data.balance,
            currency,
            today,
        )
        else {
            tx.set_provider_account_error(record.id, Some(BALANCE_UNREPRESENTABLE.to_owned()))
                .await?;
            tx.commit().await?;
            return Ok(AccountOutcome::Failed);
        };
        if record.error.is_some() {
            tx.set_provider_account_error(record.id, None).await?;
        }
        let provider_account = record.id;
        let bound = account.clone();
        let report = apply_account_batch(
            &mut tx,
            now,
            connection,
            AccountBatch {
                provider_account: record,
                account,
                transactions,
                removed: data.removed,
                coverage: data.coverage,
                unusable,
            },
        )
        .await?;
        if let (Some(account), Some((on, balance_minor))) = (bound, balance) {
            write_balance(&mut tx, now, account.id, on.min(today), balance_minor).await?;
        }
        tx.set_provider_account_applied(provider_account, now)
            .await?;
        tx.commit().await?;
        tracing::debug!(provider_account = %provider_account, "provider account applied");
        Ok(AccountOutcome::Applied(report))
    }

    /// Records on a provider account that its data could not be stored,
    /// creating it if it is new; its balance is left as it was.
    async fn record_account_failure(
        &self,
        upsert: ProviderAccountUpsert,
    ) -> Result<(), StoreError> {
        let mut tx = self.store.begin().await?;
        if tx.get_connection(upsert.connection).await?.status == ConnectionStatus::Removed {
            return Ok(());
        }
        let record = tx.upsert_provider_account(upsert).await?;
        tx.set_provider_account_error(record.id, Some(ACCOUNT_NOT_STORED.to_owned()))
            .await?;
        tx.commit().await
    }
}

/// Whether the outcome of a run that opened `opened` may still be recorded
/// on the connection, now `stored`: it is not removed and holds the same
/// sealed credentials. A reconnect while the run was fetching replaces
/// them, and the run's outcome then says nothing about the new ones.
fn still_current(stored: &ConnectionRecord, opened: &ConnectionRecord) -> bool {
    stored.status != ConnectionStatus::Removed && stored.credentials == opened.credentials
}

/// The status and detail of a run that applied `totals`. When the
/// connection was removed meanwhile, the accounts applied before the removal
/// committed stay and the run says the rest was abandoned.
fn outcome(totals: &Totals, removed: bool, connection_errors: bool) -> (SyncRunStatus, String) {
    let detail = totals.detail(connection_errors);
    if removed {
        if totals.accounts == 0 {
            (SyncRunStatus::Failed, CONNECTION_REMOVED.to_owned())
        } else {
            (
                SyncRunStatus::Partial,
                format!("{detail}; {CONNECTION_REMOVED}"),
            )
        }
    } else if connection_errors || totals.failed_accounts > 0 || totals.report.skipped > 0 {
        (SyncRunStatus::Partial, detail)
    } else {
        (SyncRunStatus::Succeeded, detail)
    }
}

/// Refuses, with [`SyncError::Conflict`] and [`CONNECTION_REMOVED`], a
/// connection that is removed.
pub(super) async fn ensure_not_removed(
    tx: &mut StoreTx,
    connection: ConnectionId,
) -> Result<ConnectionRecord, SyncError> {
    let record = tx.get_connection(connection).await?;
    if record.status == ConnectionStatus::Removed {
        return Err(SyncError::Conflict(CONNECTION_REMOVED));
    }
    Ok(record)
}

/// Maps a provider-call error to the failure recorded for it.
fn failure(err: &SyncError) -> Failure {
    match err {
        SyncError::Provider(ProviderError::ReauthRequired) => Failure::Connection {
            status: ConnectionStatus::ReauthRequired,
            text: ProviderError::ReauthRequired.to_string(),
        },
        SyncError::Provider(err) => Failure::Connection {
            status: ConnectionStatus::Error,
            text: err.record_text(),
        },
        other => Failure::Connection {
            status: ConnectionStatus::Error,
            text: other.to_string(),
        },
    }
}

/// The first day to fetch, so that every provider account the provider
/// still reports gets the records it missed: `overlap_days` before the day
/// its data was last applied, or the provider's maximum window for one
/// never applied; the earliest of those over `applied`. Without any such
/// account (the first run, or a connection reporting none), the same rule
/// on the connection's last success. Never more than `max_days` before
/// `today`, however long ago that was.
#[must_use]
fn window_start(
    applied: &[Option<Timestamp>],
    last_success: Option<Timestamp>,
    today: Date,
    timezone: &TimeZone,
    max_days: i64,
    overlap_days: i64,
) -> Date {
    let earliest = days_before(today, max_days);
    let start = |at: Option<Timestamp>| match at {
        None => earliest,
        Some(at) => {
            let day = at.to_zoned(timezone.clone()).date();
            days_before(day, overlap_days).max(earliest)
        }
    };
    if applied.is_empty() {
        start(last_success)
    } else {
        applied
            .iter()
            .map(|&at| start(at))
            .min()
            .unwrap_or(earliest)
    }
}

/// `day` minus `days` (a negative count counts as none), saturating at the
/// earliest representable day.
fn days_before(day: Date, days: i64) -> Date {
    Span::new()
        .try_days(days.max(0))
        .ok()
        .and_then(|span| day.checked_sub(span).ok())
        .unwrap_or(Date::MIN)
}

/// The connection errors, sanitized and joined; `None` when there are none.
fn joined_errors(errors: &[String]) -> Option<String> {
    let sanitized: Vec<String> = errors
        .iter()
        .map(|error| sanitize_provider_message(error))
        .filter(|error| !error.is_empty())
        .collect();
    if sanitized.is_empty() {
        return None;
    }
    Some(
        sanitized
            .join("; ")
            .chars()
            .take(MAX_PROVIDER_MESSAGE_CHARS)
            .collect(),
    )
}

/// Whether `id` can identify a provider account: 1 to 255 characters
/// without control characters.
fn valid_provider_account_id(id: &str) -> bool {
    (1..=MAX_PROVIDER_ID_CHARS).contains(&id.chars().count()) && !id.chars().any(char::is_control)
}

/// The bound account and the currency to convert amounts in, or the error
/// to record on the provider account.
async fn check_account(
    tx: &mut StoreTx,
    record: &ProviderAccountRecord,
    data: &ProviderAccountData,
) -> Result<Result<(Option<AccountRecord>, Currency), String>, SyncError> {
    if let Some(error) = &data.error {
        let error = sanitize_provider_message(error);
        return Ok(Err(if error.is_empty() {
            ACCOUNT_ERROR.to_owned()
        } else {
            error
        }));
    }
    let Ok(currency) = Currency::parse(&data.currency) else {
        return Ok(Err(UNSUPPORTED_CURRENCY.to_owned()));
    };
    let account = match record.account {
        Some(id) => Some(tx.get_account(id).await?),
        None => None,
    };
    if account
        .as_ref()
        .is_some_and(|account| account.currency != currency)
    {
        return Ok(Err(CURRENCY_DIFFERS.to_owned()));
    }
    Ok(Ok((account, currency)))
}

/// An account's records and balance in its currency's minor units.
struct Converted {
    transactions: Vec<NormalizedTransaction>,
    /// The provider's unusable records, then the records whose amount does
    /// not convert exactly.
    unusable: Vec<Option<String>>,
    /// The balance and the day it describes.
    balance: Option<(Date, i64)>,
}

/// The account's records and balance in `currency`'s minor units, a
/// pending record without a date dated `today`. A record whose amount does
/// not convert exactly, or a posted one without a date, joins `unusable`;
/// `None` when the balance does not convert.
fn convert(
    records: Vec<ProviderTransaction>,
    mut unusable: Vec<Option<String>>,
    balance: Option<(Date, DecimalAmount)>,
    currency: Currency,
    today: Date,
) -> Option<Converted> {
    let balance = match balance {
        Some((on, amount)) => Some((on, amount.to_minor_units(currency).ok()?)),
        None => None,
    };
    let mut transactions = Vec::with_capacity(records.len());
    for record in records {
        if record.posted_on.is_none() && !record.pending {
            unusable.push(Some(record.provider_id));
            continue;
        }
        let Ok(amount_minor) = record.amount.to_minor_units(currency) else {
            unusable.push(Some(record.provider_id));
            continue;
        };
        transactions.push(NormalizedTransaction {
            provider_id: record.provider_id,
            posted_on: record.posted_on.unwrap_or(today),
            date_reported: record.posted_on.is_some(),
            transacted_on: record.transacted_on,
            amount_minor,
            amount_raw: record.amount.as_str().to_owned(),
            description: record.description,
            payee: record.payee,
            memo: record.memo,
            pending: record.pending,
            replaces_provider_id: record.replaces_provider_id,
        });
    }
    Some(Converted {
        transactions,
        unusable,
        balance,
    })
}

/// Writes the day's `sync` snapshot of `account`, audited when it changes
/// the day's value or source.
async fn write_balance(
    tx: &mut StoreTx,
    now: Timestamp,
    account: AccountId,
    on: Date,
    balance_minor: i64,
) -> Result<(), SyncError> {
    let previous = tx
        .upsert_balance(BalanceSnapshot {
            account,
            on,
            balance_minor,
            source: BalanceSource::Sync,
            recorded_at: now,
        })
        .await?;
    let unchanged = previous.as_ref().is_some_and(|previous| {
        previous.balance_minor == balance_minor && previous.source == BalanceSource::Sync
    });
    if unchanged {
        return Ok(());
    }
    let changes = AuditChanges::new()
        .set("on", on)
        .change(
            "balance_minor",
            previous.as_ref().map(|p| p.balance_minor),
            balance_minor,
        )
        .change(
            "source",
            previous.as_ref().map(|p| p.source.as_str()),
            BalanceSource::Sync.as_str(),
        );
    tx.append_audit(
        now,
        &AuditEntry {
            actor: sync_actor(),
            action: "balance.set".to_owned(),
            entity_type: "account".to_owned(),
            entity_id: account.to_string(),
            changes,
        },
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use jiff::civil::date;
    use jiff::tz::TimeZone;

    use coffer_domain::sync::SyncRunStatus;

    use super::{
        AccountOutcome, ApplyReport, CONNECTION_REMOVED, Totals, days_before, joined_errors,
        outcome, window_start,
    };

    /// A run whose connection is removed after some accounts were applied
    /// is partial and says so; one removed before any is failed.
    #[test]
    fn removal_mid_run_outcomes() {
        let mut totals = Totals::default();
        assert_eq!(
            outcome(&totals, true, false),
            (SyncRunStatus::Failed, CONNECTION_REMOVED.to_owned())
        );
        totals.add(&AccountOutcome::Applied(ApplyReport {
            created: 2,
            ..ApplyReport::default()
        }));
        totals.add(&AccountOutcome::ConnectionRemoved);
        let (status, detail) = outcome(&totals, true, false);
        assert_eq!(status, SyncRunStatus::Partial);
        assert_eq!(
            detail,
            "1 account, 0 with errors; 2 created, 0 attached, 0 updated, 0 deleted, 0 held, \
             0 skipped; the connection is removed"
        );
        assert_eq!(outcome(&totals, false, false).0, SyncRunStatus::Succeeded);
        assert_eq!(outcome(&totals, false, true).0, SyncRunStatus::Partial);
        totals.add(&AccountOutcome::Failed);
        assert!(
            totals
                .detail(false)
                .starts_with("2 accounts, 1 with errors;"),
            "more than one account is plural"
        );
    }

    #[test]
    fn window_rules() {
        let utc = TimeZone::UTC;
        let today = date(2026, 10, 31);
        assert_eq!(
            window_start(&[], None, today, &utc, 90, 14),
            date(2026, 8, 2)
        );
        let yesterday: jiff::Timestamp = "2026-10-30T23:00:00Z".parse().unwrap();
        assert_eq!(
            window_start(&[], Some(yesterday), today, &utc, 90, 14),
            date(2026, 10, 16)
        );
        // The day of the last success is the household's day.
        let auckland = TimeZone::get("Pacific/Auckland").unwrap();
        assert_eq!(
            window_start(&[], Some(yesterday), today, &auckland, 90, 14),
            date(2026, 10, 17)
        );
        let long_ago: jiff::Timestamp = "2025-01-01T00:00:00Z".parse().unwrap();
        assert_eq!(
            window_start(&[], Some(long_ago), today, &utc, 90, 14),
            date(2026, 8, 2),
            "clamped after a long gap"
        );
        // With reported accounts, the one applied longest ago decides, and
        // one never applied gets the maximum window.
        let week_ago: jiff::Timestamp = "2026-10-24T12:00:00Z".parse().unwrap();
        assert_eq!(
            window_start(
                &[Some(yesterday), Some(week_ago)],
                Some(yesterday),
                today,
                &utc,
                90,
                14
            ),
            date(2026, 10, 10)
        );
        assert_eq!(
            window_start(
                &[Some(yesterday), None],
                Some(yesterday),
                today,
                &utc,
                90,
                14
            ),
            date(2026, 8, 2)
        );
        assert_eq!(
            window_start(&[Some(long_ago)], Some(yesterday), today, &utc, 90, 14),
            date(2026, 8, 2),
            "clamped for an account applied long ago"
        );
        assert_eq!(days_before(today, -3), today);
        assert_eq!(days_before(today, i64::MAX), jiff::civil::Date::MIN);
    }

    #[test]
    fn connection_errors_join_sanitized() {
        assert_eq!(joined_errors(&[]), None);
        assert_eq!(joined_errors(&["\u{7}".into()]), None);
        assert_eq!(
            joined_errors(&["a https://u:p@h.example".into(), "b".into()]).as_deref(),
            Some("a https://***@h.example; b")
        );
        let long = joined_errors(&["x".repeat(400), "y".repeat(400)]).unwrap();
        assert_eq!(long.chars().count(), 500);
    }
}
