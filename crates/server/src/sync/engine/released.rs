//! Applying a provider account record by record when a member asked for the
//! sync cursor to move past its failing records.

use coffer_store::{AccountRecord, ProviderAccountRecord};
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
    pub(super) record: ProviderAccountRecord,
    pub(super) account: Option<AccountRecord>,
    pub(super) transactions: Vec<NormalizedTransaction>,
    pub(super) removed: Vec<String>,
    /// Records already left out as unusable.
    pub(super) skipped: u32,
    pub(super) balance: Option<(Date, i64)>,
    pub(super) today: Date,
    pub(super) now: Timestamp,
}

impl SyncService {
    /// Applies each record of the account in a transaction of its own, so
    /// one that cannot be stored costs only itself. A record counts as
    /// failing after [`ATTEMPTS`] failures; the outcome says how many, and
    /// the cursor passes them only if the run moves it. The balance is
    /// written either way, and the account counts as applied only if
    /// nothing failed.
    pub(super) async fn apply_one_by_one(
        &self,
        job: OneByOne,
    ) -> Result<AccountOutcome, SyncError> {
        let mut report = ApplyReport {
            skipped: job.skipped,
            ..ApplyReport::default()
        };
        let mut failing = 0_u32;
        let mut batches = job
            .transactions
            .iter()
            .map(|transaction| (vec![transaction.clone()], Vec::new()))
            .chain(job.removed.iter().map(|id| (Vec::new(), vec![id.clone()])));
        for (transactions, removed) in batches.by_ref() {
            let mut outcome = None;
            for _ in 0..ATTEMPTS {
                match self
                    .apply_alone(&job, transactions.clone(), removed.clone())
                    .await
                {
                    Ok(done) => {
                        outcome = Some(done);
                        break;
                    }
                    Err(err) => {
                        tracing::warn!(error = ?err, "a provider record was not applied");
                    }
                }
            }
            match outcome {
                Some(None) => return Ok(AccountOutcome::ConnectionRemoved),
                Some(Some(done)) => report.absorb(&done),
                None => failing += 1,
            }
        }

        let mut tx = self.store.begin().await?;
        if super::ensure_not_removed(&mut tx, job.record.connection)
            .await
            .is_err()
        {
            return Ok(AccountOutcome::ConnectionRemoved);
        }
        if let (Some(account), Some((on, balance_minor))) = (&job.account, job.balance) {
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
            tx.set_provider_account_applied(job.record.id, job.now)
                .await?;
        } else {
            tx.set_provider_account_error(job.record.id, Some(ACCOUNT_NOT_STORED.to_owned()))
                .await?;
        }
        tx.commit().await?;
        if failing == 0 {
            return Ok(AccountOutcome::Applied(report));
        }
        Ok(AccountOutcome::Released {
            account: job.record.id,
            report,
            failing,
        })
    }

    /// Applies one record, or one removal, in its own transaction; `None`
    /// when the connection is removed.
    async fn apply_alone(
        &self,
        job: &OneByOne,
        transactions: Vec<NormalizedTransaction>,
        removed: Vec<String>,
    ) -> Result<Option<ApplyReport>, SyncError> {
        let mut tx = self.store.begin().await?;
        if super::ensure_not_removed(&mut tx, job.record.connection)
            .await
            .is_err()
        {
            return Ok(None);
        }
        let report = apply_account_batch(
            &mut tx,
            job.now,
            job.record.connection,
            AccountBatch {
                provider_account: job.record.clone(),
                account: job.account.clone(),
                transactions,
                removed,
                coverage: Coverage::Delta,
                unusable: Vec::new(),
            },
        )
        .await?;
        tx.commit().await?;
        Ok(Some(report))
    }
}
