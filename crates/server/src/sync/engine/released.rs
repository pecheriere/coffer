//! Applying a provider account record by record when a member asked for the
//! sync cursor to move past its failing records.

use std::collections::HashSet;

use coffer_domain::ids::{ConnectionId, ProviderAccountId, SourceRecordId};
use coffer_store::{AccountRecord, ConnectionStatus, ProviderAccountRecord, StoreTx};
use jiff::Timestamp;
use jiff::civil::Date;

use super::accounts::ACCOUNT_NOT_STORED;
use super::results::AccountOutcome;
use super::{SyncService, write_balance};
use crate::sync::SyncError;
use crate::sync::apply::{
    AccountBatch, ApplyReport, Coverage, NormalizedTransaction, apply_account_batch,
};

/// How often a record is tried before it counts as failing: a failure that
/// goes away at once was no reason to give the record up.
const ATTEMPTS: u32 = 2;

/// What one account's one-by-one apply works on.
pub(super) struct OneByOne {
    pub(super) connection: ConnectionId,
    pub(super) provider_account: ProviderAccountId,
    pub(super) transactions: Vec<NormalizedTransaction>,
    pub(super) removed: Vec<String>,
    /// Records already left out as unusable.
    pub(super) skipped: u32,
    pub(super) balance: Option<(Date, i64)>,
    pub(super) today: Date,
    pub(super) now: Timestamp,
}

/// How one attempt at a record ended, the database having done its part.
enum Attempt {
    Applied(ApplyReport),
    ConnectionRemoved,
    /// The record itself could not be applied.
    Failed(SyncError),
}

/// The provider account as it is now, and the account it is bound to: a
/// binding may commit between two records of a run.
async fn current_binding(
    tx: &mut StoreTx,
    id: ProviderAccountId,
) -> Result<(ProviderAccountRecord, Option<AccountRecord>), SyncError> {
    let record = tx.get_provider_account(id).await?;
    let account = match record.account {
        Some(account) => Some(tx.get_account(account).await?),
        None => None,
    };
    Ok((record, account))
}

impl SyncService {
    /// Applies each record of the account in a transaction of its own, so
    /// one that cannot be stored costs only itself. A record counts as
    /// failing after [`ATTEMPTS`] failures; the outcome says how many, and
    /// the cursor passes them only if the run moves it. The balance is
    /// written either way, and the account counts as applied only if
    /// nothing failed.
    ///
    /// # Errors
    ///
    /// Any failure that is not the record's own, such as a busy or damaged
    /// database: nothing more is tried, so no record is given up for it.
    pub(super) async fn apply_one_by_one(
        &self,
        job: OneByOne,
    ) -> Result<AccountOutcome, SyncError> {
        let vanishing = self.vanishing_sources(&job).await?;
        let mut report = ApplyReport {
            skipped: job.skipped,
            ..ApplyReport::default()
        };
        let mut failing = 0_u32;
        let batches = job
            .transactions
            .iter()
            .map(|transaction| (vec![transaction.clone()], Vec::new()))
            .chain(job.removed.iter().map(|id| (Vec::new(), vec![id.clone()])));
        for (transactions, removed) in batches {
            let mut failed = true;
            for _ in 0..ATTEMPTS {
                match self
                    .apply_alone(&job, transactions.clone(), removed.clone(), &vanishing)
                    .await?
                {
                    Attempt::Applied(done) => {
                        report.absorb(&done);
                        failed = false;
                        break;
                    }
                    Attempt::ConnectionRemoved => return Ok(AccountOutcome::ConnectionRemoved),
                    Attempt::Failed(err) => {
                        tracing::warn!(error = ?err, "a provider record was not applied");
                    }
                }
            }
            failing += u32::from(failed);
        }

        let mut tx = self.store.begin().await?;
        if tx.get_connection(job.connection).await?.status == ConnectionStatus::Removed {
            return Ok(AccountOutcome::ConnectionRemoved);
        }
        let (_, account) = current_binding(&mut tx, job.provider_account).await?;
        if let (Some(account), Some((on, balance_minor))) = (&account, job.balance) {
            write_balance(
                &mut tx,
                job.now,
                account.id,
                on.min(job.today),
                balance_minor,
            )
            .await?;
        }
        if failing == 0 {
            tx.set_provider_account_applied(job.provider_account, job.now)
                .await?;
        } else {
            tx.set_provider_account_error(
                job.provider_account,
                Some(ACCOUNT_NOT_STORED.to_owned()),
            )
            .await?;
        }
        tx.commit().await?;
        if failing == 0 {
            return Ok(AccountOutcome::Applied(report));
        }
        Ok(AccountOutcome::Released {
            account: job.provider_account,
            report,
            failing,
        })
    }

    /// The account's sources the delta removes. A batch of one record must
    /// see them as gone, as it would if the delta applied at once: a posted
    /// record then finds its pending form.
    async fn vanishing_sources(
        &self,
        job: &OneByOne,
    ) -> Result<HashSet<SourceRecordId>, SyncError> {
        let mut tx = self.store.begin().await?;
        let mut vanishing = HashSet::new();
        for provider_id in &job.removed {
            if let Some(source) = tx.find_source(job.connection, provider_id).await?
                && source.provider_account == job.provider_account
            {
                vanishing.insert(source.id);
            }
        }
        Ok(vanishing)
    }

    /// Applies one record, or one removal, in its own transaction.
    async fn apply_alone(
        &self,
        job: &OneByOne,
        transactions: Vec<NormalizedTransaction>,
        removed: Vec<String>,
        vanishing: &HashSet<SourceRecordId>,
    ) -> Result<Attempt, SyncError> {
        let mut tx = self.store.begin().await?;
        if tx.get_connection(job.connection).await?.status == ConnectionStatus::Removed {
            return Ok(Attempt::ConnectionRemoved);
        }
        let (provider_account, account) = current_binding(&mut tx, job.provider_account).await?;
        let applied = apply_account_batch(
            &mut tx,
            job.now,
            job.connection,
            AccountBatch {
                provider_account,
                account,
                transactions,
                removed,
                vanishing: vanishing.clone(),
                coverage: Coverage::Delta,
                unusable: Vec::new(),
            },
        )
        .await;
        match applied {
            Ok(report) => {
                tx.commit().await?;
                Ok(Attempt::Applied(report))
            }
            Err(err) if err.is_database_fault() => Err(err),
            Err(err) => Ok(Attempt::Failed(err)),
        }
    }
}
