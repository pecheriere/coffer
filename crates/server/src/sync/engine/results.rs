//! How a run and its provider accounts fared.

use coffer_domain::sync::SyncRunStatus;
use coffer_store::ConnectionStatus;

use super::CONNECTION_REMOVED;
use crate::sync::SyncError;
use crate::sync::apply::ApplyReport;
use crate::sync::provider::{MAX_PROVIDER_MESSAGE_CHARS, ProviderError, sanitize_provider_message};

/// How one provider account fared.
pub(super) enum AccountOutcome {
    Applied(ApplyReport),
    /// Nothing of it was applied. `refetch` says the fetch held records of
    /// it that a later fetch could still store; `passed` counts the records
    /// the cursor moves past for good because a member asked for that.
    Failed {
        refetch: bool,
        passed: u32,
    },
    /// The connection was removed before the account could be applied:
    /// nothing of it was written.
    ConnectionRemoved,
}

/// Why a run could not use its connection's provider.
pub(super) enum Failure {
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
pub(super) struct Totals {
    pub(super) accounts: u32,
    pub(super) failed_accounts: u32,
    pub(super) report: ApplyReport,
    /// Whether the connection was removed while the run applied its data.
    pub(super) connection_removed: bool,
    /// Whether some failed account's records must be fetched again.
    pub(super) refetch: bool,
}

impl Totals {
    pub(super) fn add(&mut self, outcome: &AccountOutcome) {
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
            AccountOutcome::Failed { refetch, passed } => {
                self.failed_accounts += 1;
                self.refetch |= refetch;
                self.report.skipped = self.report.skipped.saturating_add(*passed);
            }
            AccountOutcome::ConnectionRemoved => {}
        }
    }

    pub(super) fn detail(&self, connection_errors: bool) -> String {
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
/// The status and detail of a run that applied `totals`. When the
/// connection was removed meanwhile, the accounts applied before the removal
/// committed stay and the run says the rest was abandoned.
pub(super) fn outcome(
    totals: &Totals,
    removed: bool,
    connection_errors: bool,
) -> (SyncRunStatus, String) {
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
/// Maps a provider-call error to the failure recorded for it.
pub(super) fn failure(err: &SyncError) -> Failure {
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
/// The connection errors, sanitized and joined; `None` when there are none.
pub(super) fn joined_errors(errors: &[String]) -> Option<String> {
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
