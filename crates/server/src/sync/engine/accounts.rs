//! Applying each provider account of a run in its own store transaction.

use coffer_domain::audit::{AuditChanges, AuditEntry};
use coffer_domain::ids::{ConnectionId, ProviderAccountId};
use coffer_store::{
    ConnectionRecord, ConnectionStatus, ConnectionStatusChange, ProviderAccountRecord,
    ProviderAccountUpsert, RawBalance, StoreError, StoreTx, SyncRunRecord,
};
use std::collections::HashMap;

use jiff::Timestamp;
use jiff::civil::Date;

use super::convert::{Converted, check_account, convert};
use super::results::{AccountOutcome, Totals, joined_errors, outcome};
use super::{SyncService, same_credentials, still_current, write_balance};
use crate::sync::SyncError;
use crate::sync::apply::{
    AccountBatch, ApplyReport, Coverage, apply_account_batch, sync_actor, valid_provider_id,
};
use crate::sync::provider::{
    FetchMode, FetchResult, ProviderAccountData, sanitize_provider_message,
};

const BALANCE_UNREPRESENTABLE: &str =
    "the provider reported a balance too large to store, so it was left out";
pub(super) const ACCOUNT_NOT_STORED: &str =
    "the account's data could not be stored; the next sync fetches the missing records again";
const RELEASE_WITHDRAWN_REASON: &str =
    "the account's refusal differs from the one recorded when the member released";
const RELEASE_OUTGROWN_REASON: &str =
    "the fetch holds more records for the account than the member agreed to";
const RECORDS_GIVEN_UP: &str =
    "records of this account that could not be stored were given up, as a member asked";

/// What an account's fetched data means for the cursor.
#[derive(Clone, Copy)]
pub(super) struct Records {
    /// Whether records of it could still be stored by a later fetch, and the
    /// cursor must not pass them. Never for a provider without a cursor.
    pub(super) holds: bool,
    /// How many records the cursor passes if it moves on without applying
    /// them.
    pub(super) count: u32,
}

impl Records {
    fn of(data: &ProviderAccountData, cursor: bool) -> Self {
        if !cursor {
            return Self {
                holds: false,
                count: 0,
            };
        }
        let stored = data.transactions.len() + data.removed.len();
        Self {
            holds: stored > 0,
            count: u32::try_from(stored + data.unusable.len()).unwrap_or(u32::MAX),
        }
    }
}

/// The outcome of an account that was not applied. A cursor is held by it
/// when `fixable` records of it wait for a later fetch; counted, so that a
/// hold lasting for good shows. Otherwise nothing waits for it: any hold or
/// release it had ends, and the cursor passes its records.
async fn settle_failure(
    tx: &mut StoreTx,
    account: &ProviderAccountRecord,
    records: Records,
    fixable: bool,
) -> Result<AccountOutcome, StoreError> {
    if !(records.holds && fixable) {
        tx.clear_cursor_hold(account.id).await?;
        return Ok(AccountOutcome::Failed {
            refetch: false,
            passed: records.count,
        });
    }
    tx.count_cursor_hold(account.id, records.count).await?;
    Ok(AccountOutcome::Failed {
        refetch: true,
        passed: 0,
    })
}

/// Takes back a member's release of `account`, which no longer covers what the
/// member saw: the account stays held, and the member has to look again. The
/// audit entry carries the fixed `reason` and no provider text.
async fn withdraw_release(
    tx: &mut StoreTx,
    now: Timestamp,
    account: &ProviderAccountRecord,
    reason: &'static str,
) -> Result<(), StoreError> {
    tx.withdraw_cursor_release(account.id).await?;
    tx.append_audit(
        now,
        &AuditEntry {
            actor: sync_actor(),
            action: "connection.release_withdrawn".to_owned(),
            entity_type: "connection".to_owned(),
            entity_id: account.connection.to_string(),
            changes: AuditChanges::new()
                .set("provider_accounts", account.id.to_string())
                .set("reason", reason),
        },
    )
    .await?;
    Ok(())
}

/// Gives up, for good, the records of the released accounts `passed`, in the
/// transaction that moves the cursor past them: their holds end, those told
/// to expect the records again no longer are, and the pass is audited with
/// its count.
async fn record_pass(
    tx: &mut StoreTx,
    now: Timestamp,
    connection: ConnectionId,
    passed: &[(ProviderAccountId, u32)],
) -> Result<(), SyncError> {
    if passed.is_empty() {
        return Ok(());
    }
    let records: u32 = passed.iter().map(|(_, count)| *count).sum();
    let accounts: Vec<String> = passed.iter().map(|(id, _)| id.to_string()).collect();
    for (id, _) in passed {
        tx.clear_cursor_hold(*id).await?;
        if tx.get_provider_account(*id).await?.error.as_deref() == Some(ACCOUNT_NOT_STORED) {
            tx.set_provider_account_error(*id, Some(RECORDS_GIVEN_UP.to_owned()))
                .await?;
        }
    }
    tx.append_audit(
        now,
        &AuditEntry {
            actor: sync_actor(),
            action: "connection.cursor_passed".to_owned(),
            entity_type: "connection".to_owned(),
            entity_id: connection.to_string(),
            changes: AuditChanges::new()
                .set("records", i64::from(records))
                .set("provider_accounts", accounts.join(", ")),
        },
    )
    .await?;
    Ok(())
}

impl SyncService {
    pub(super) async fn apply(
        &self,
        run: &SyncRunRecord,
        connection: &ConnectionRecord,
        result: FetchResult,
        mode: FetchMode,
        today: Date,
        started_at: Timestamp,
    ) -> Result<SyncRunRecord, SyncError> {
        let mut totals = Totals::default();
        let cursor = matches!(mode, FetchMode::Cursor);
        let present: Vec<String> = result
            .accounts
            .iter()
            .map(|data| data.provider_account_id.clone())
            .collect();
        let released: HashMap<String, (ProviderAccountId, u32)> = self
            .store
            .list_provider_accounts(Some(connection.id))
            .await?
            .into_iter()
            .filter(|account| account.hold_released)
            .map(|account| {
                let agreed = (account.id, account.released_records);
                (account.provider_account_id, agreed)
            })
            .collect();
        for data in result.accounts {
            let release = released.get(&data.provider_account_id).copied();
            let outcome = self
                .apply_account(connection.id, data, today, (cursor, release))
                .await;
            totals.add(&outcome);
            if totals.connection_removed {
                // Removed while the run was fetching or applying: nothing
                // more is written for it.
                break;
            }
        }
        totals.report.skipped = totals.report.skipped.saturating_add(result.skipped);
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
        let reported_errors = connection_errors.is_some();
        let current = still_current(&stored, connection);
        // A cursor belongs to the credentials it was given for, so it outlives
        // a reauthorization: the records it covers were applied above. Unless
        // the provider reported errors, which a stale run cannot record: the
        // cursor then stays, for the next run to fetch and report them itself.
        let new_cursor = if !removed
            && same_credentials(&stored, connection)
            && (current || !reported_errors)
            && !totals.refetch
            && matches!(mode, FetchMode::Cursor)
        {
            result.cursor
        } else {
            None
        };
        // Records a member asked to pass are passed only now that the cursor
        // moves past them; if it does not, the release stays for a later run.
        let passed = if new_cursor.is_some() {
            std::mem::take(&mut totals.released)
        } else {
            Vec::new()
        };
        let passed_records: u32 = passed.iter().map(|(_, count)| *count).sum();
        totals.report.skipped = totals.report.skipped.saturating_add(passed_records);
        let (status, detail) = outcome(&totals, removed, reported_errors);
        if !removed && current {
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
        }
        if let Some(cursor) = new_cursor {
            tx.set_provider_state(connection.id, Some(cursor)).await?;
        }
        record_pass(&mut tx, now, connection.id, &passed).await?;
        if !removed && same_credentials(&stored, connection) {
            // A completed fetch without an account says nothing waits for it.
            tx.clear_cursor_holds_except(connection.id, &present)
                .await?;
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
        (cursor, released): (bool, Option<(ProviderAccountId, u32)>),
    ) -> AccountOutcome {
        let records = Records::of(&data, cursor);
        // A member asked for the cursor to move past its failing records:
        // applied one by one, so only the ones that fail are given up.
        // Only for a delta, whose records stand alone.
        let mut release =
            released.is_some() && records.holds && matches!(data.coverage, Coverage::Delta);
        if !valid_provider_id(&data.provider_account_id) {
            // Nothing can be recorded on an account without a usable id, and
            // no later fetch could store its records: the run counts it among
            // the accounts with errors and its records among the skipped.
            tracing::warn!(
                connection = %connection,
                "provider account with an invalid id skipped"
            );
            return AccountOutcome::Failed {
                refetch: false,
                passed: records.count,
            };
        }
        let now = self.clock.now();
        if let Some((id, agreed)) = released
            && release
            && records.count > agreed
        {
            // Records arrived that the member did not agree to give up. In a
            // transaction of its own, since the failure of an apply would
            // roll it back; if it fails, the next sync takes this path again.
            release = false;
            if let Err(err) = self.withdraw_outgrown_release(connection, id, now).await {
                tracing::warn!(connection = %connection, error = ?err, "a release was not withdrawn");
            }
        }
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
        match self
            .try_apply_account(upsert.clone(), data, today, (records, release))
            .await
        {
            Ok(outcome) => outcome,
            Err(err) => {
                tracing::warn!(connection = %connection, error = ?err, "provider account not applied");
                match self.record_account_failure(upsert, records).await {
                    Ok(outcome) => outcome,
                    Err(err) => {
                        tracing::error!(
                            connection = %connection,
                            error = ?err,
                            "could not record a provider account's failure"
                        );
                        AccountOutcome::Failed {
                            refetch: records.holds,
                            passed: 0,
                        }
                    }
                }
            }
        }
    }

    async fn withdraw_outgrown_release(
        &self,
        connection: ConnectionId,
        id: ProviderAccountId,
        now: Timestamp,
    ) -> Result<(), StoreError> {
        let mut tx = self.store.begin().await?;
        if tx.get_connection(connection).await?.status == ConnectionStatus::Removed {
            return Ok(());
        }
        let record = tx.get_provider_account(id).await?;
        if record.hold_released {
            withdraw_release(&mut tx, now, &record, RELEASE_OUTGROWN_REASON).await?;
        }
        tx.commit().await
    }

    async fn try_apply_account(
        &self,
        mut upsert: ProviderAccountUpsert,
        data: ProviderAccountData,
        today: Date,
        (records, release): (Records, bool),
    ) -> Result<AccountOutcome, SyncError> {
        let now = upsert.seen_at;
        let connection = upsert.connection;
        // The exact text, whatever becomes of the value.
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
        // A report with an error or no currency says nothing about what the
        // account is, and the account is refused below.
        let record = if data.error.is_some() || data.currency.is_empty() {
            tx.upsert_failed_provider_account(upsert).await?
        } else {
            tx.upsert_provider_account(upsert).await?
        };

        let checked = check_account(&mut tx, &record, &data).await?;
        let (account, currency) = match checked {
            Ok(checked) => checked,
            Err(refusal) => {
                let known = record.error.as_deref() == Some(refusal.text.as_str());
                tx.set_provider_account_error(record.id, Some(refusal.text))
                    .await?;
                if release && known && !refusal.lasting {
                    // What the member released is still what is wrong, and
                    // none of its records can be stored.
                    tx.commit().await?;
                    return Ok(AccountOutcome::Released {
                        account: record.id,
                        report: ApplyReport::default(),
                        failing: records.count,
                    });
                }
                // Any other refusal is not what the member agreed to give
                // the records up for, however long it lasts. Read from this
                // transaction, so a release made during the run counts.
                if record.hold_released && !known && records.holds && !refusal.lasting {
                    withdraw_release(&mut tx, now, &record, RELEASE_WITHDRAWN_REASON).await?;
                }
                let outcome = settle_failure(&mut tx, &record, records, !refusal.lasting).await?;
                tx.commit().await?;
                return Ok(outcome);
            }
        };
        let Converted {
            transactions,
            unusable,
        } = convert(data.transactions, data.unusable, currency, today);
        // A balance is a valuation: rounded to fit, and if it still cannot be
        // stored the account's records apply without it.
        let (balance, note) = match data.balance {
            Some((on, amount)) => match amount.to_minor_units_rounded(currency) {
                Ok(minor) => (Some((on, minor)), None),
                Err(_) => (None, Some(BALANCE_UNREPRESENTABLE.to_owned())),
            },
            None => (None, None),
        };
        if record.error != note {
            tx.set_provider_account_error(record.id, note).await?;
        }
        if release {
            tx.commit().await?;
            let job = super::released::OneByOne {
                connection,
                provider_account: record.id,
                transactions,
                removed: data.removed,
                skipped: u32::try_from(unusable.len()).unwrap_or(u32::MAX),
                balance,
                today,
                now,
            };
            return self.apply_one_by_one(job).await;
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
        records: Records,
    ) -> Result<AccountOutcome, StoreError> {
        let mut tx = self.store.begin().await?;
        if tx.get_connection(upsert.connection).await?.status == ConnectionStatus::Removed {
            return Ok(AccountOutcome::ConnectionRemoved);
        }
        let record = tx.upsert_failed_provider_account(upsert).await?;
        tx.set_provider_account_error(record.id, Some(ACCOUNT_NOT_STORED.to_owned()))
            .await?;
        let outcome = settle_failure(&mut tx, &record, records, true).await?;
        tx.commit().await?;
        Ok(outcome)
    }
}
