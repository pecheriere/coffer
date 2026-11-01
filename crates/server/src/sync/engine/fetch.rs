//! Fetching a connection's records for a run: the window asked for and the outcome of a failed fetch.

use coffer_domain::sync::SyncRunStatus;
use coffer_store::{ConnectionRecord, ConnectionStatus, ConnectionStatusChange, SyncRunRecord};
use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{Span, Timestamp};

use super::results::{Failure, failure};
use super::{CONNECTION_REMOVED, PROVIDER_NOT_OFFERED, SyncService, still_current};
use crate::sync::SyncError;
use crate::sync::provider::{FetchMode, FetchRequest, FetchResult, ProviderKind};
use crate::vault::SealedCredentials;

impl SyncService {
    /// Calls the provider with the connection's credentials, opened only
    /// for the call. `applied` is when each provider account it still
    /// reports was last applied (see [`window_start`]).
    pub(super) async fn fetch(
        &self,
        connection: &ConnectionRecord,
        accounts: &[(Timestamp, Option<Timestamp>)],
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
                    accounts,
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

    pub(super) async fn record_failure(
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
}

/// The first day to fetch, so that every provider account the provider
/// reported within its maximum window gets the records it missed:
/// `overlap_days` before the day its data was last applied, or the
/// maximum window for one never applied; the earliest of those over
/// `accounts` (each `(last seen, last applied)`). An account the provider
/// left out of a few answers still counts, so its missing days are fetched
/// when it returns; one not seen for longer than the window (a closed bank
/// account) no longer holds every fetch at the maximum. Without any such
/// account (the first run, or a connection reporting none), the same rule
/// on the connection's last success. Never more than `max_days` before
/// `today`, however long ago that was.
#[must_use]
pub(super) fn window_start(
    accounts: &[(Timestamp, Option<Timestamp>)],
    last_success: Option<Timestamp>,
    today: Date,
    timezone: &TimeZone,
    max_days: i64,
    overlap_days: i64,
) -> Date {
    let earliest = days_before(today, max_days);
    let day = |at: Timestamp| at.to_zoned(timezone.clone()).date();
    let start = |at: Option<Timestamp>| match at {
        None => earliest,
        Some(at) => days_before(day(at), overlap_days).max(earliest),
    };
    accounts
        .iter()
        .filter(|&&(seen, _)| day(seen) >= earliest)
        .map(|&(_, applied)| start(applied))
        .min()
        .unwrap_or_else(|| start(last_success))
}

/// `day` minus `days` (a negative count counts as none), saturating at the
/// earliest representable day.
pub(super) fn days_before(day: Date, days: i64) -> Date {
    Span::new()
        .try_days(days.max(0))
        .ok()
        .and_then(|span| day.checked_sub(span).ok())
        .unwrap_or(Date::MIN)
}
