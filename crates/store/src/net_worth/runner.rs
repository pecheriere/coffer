//! Running a full rebuild, and filling many days, a transaction at a time. Whoever runs
//! one gives other writers a gap after each transaction, and only one rebuild runs at a
//! time, since two would fill each other's gaps.
//!
//! That one is decided by an operating system lock on a file beside the database, as a
//! server and `coffer rebuild-net-worth` are separate processes. It goes with whoever
//! holds it, however that ends, so a runner that died blocks nobody, and the file left
//! behind means nothing. It decides only who runs: the progress is in the database.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;
use std::time::Duration;

use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;

use super::{ROWS_PER_TRANSACTION, RebuildBatch};
use crate::household::fetch_household_zone;
use crate::{Store, StoreError, StoreTx};

/// The wait after each transaction. A writer waiting for the lock sleeps up to 100 ms
/// between tries, so a shorter gap could pass unnoticed time after time.
pub(super) const PAUSE_BETWEEN_BATCHES: Duration = Duration::from_millis(150);

/// The day a runner writes up to, worked out inside each of its transactions.
#[derive(Debug, Clone)]
pub enum RunsThrough {
    /// This day.
    Day(Date),
    /// The household's day at `at`, in the zone the transaction sees: the one chosen in the
    /// settings, else `default_zone`. A runner can start before a change of zone commits and
    /// write after it, and a day worked out earlier would then be one still to come.
    HouseholdDay {
        at: Timestamp,
        default_zone: TimeZone,
    },
}

impl From<Date> for RunsThrough {
    fn from(day: Date) -> Self {
        Self::Day(day)
    }
}

impl RunsThrough {
    async fn day(&self, tx: &mut StoreTx) -> Result<Date, StoreError> {
        match self {
            Self::Day(day) => Ok(*day),
            Self::HouseholdDay { at, default_zone } => {
                let chosen = fetch_household_zone(&mut *tx.tx).await?;
                let zone = chosen.as_ref().map_or(default_zone, |zone| zone.zone());
                Ok(at.to_zoned(zone.clone()).date())
            }
        }
    }
}

/// Being the one running a rebuild, until dropped.
#[derive(Debug)]
struct RebuildClaim {
    _locked: File,
}

impl RebuildClaim {
    /// The claim, unless someone else holds it.
    fn take(path: &Path) -> Result<Option<Self>, StoreError> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)
            .map_err(StoreError::backend)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self { _locked: file })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(err)) => Err(StoreError::backend(err)),
        }
    }
}

impl Store {
    /// Recompute the whole table up to `through`, starting over if a rebuild was left
    /// part way, once any rebuild running has ended. A table that fits in one batch
    /// changes at one moment. A larger one answers as rebuilding until the last batch,
    /// and other writers get their turn between batches. Answers the rows it wrote: more
    /// than the table holds if a change started it over, and without those a write
    /// between batches added.
    ///
    /// # Errors
    ///
    /// Backend failures, including a stored row that no longer parses. What committed
    /// stays, and [`Self::finish_net_worth_rebuild`] goes on from there.
    pub async fn rebuild_net_worth(
        &self,
        through: impl Into<RunsThrough>,
    ) -> Result<u64, StoreError> {
        let through = through.into();
        let _claim = loop {
            if let Some(claim) = RebuildClaim::take(&self.rebuild_claim)? {
                break claim;
            }
            tokio::time::sleep(PAUSE_BETWEEN_BATCHES).await;
        };
        self.start_over_and_run(&through).await
    }

    /// As [`Self::rebuild_net_worth`], but answers `None` at once if a rebuild is running,
    /// for a caller that wants to say so before waiting.
    ///
    /// # Errors
    ///
    /// As [`Self::rebuild_net_worth`].
    pub async fn try_rebuild_net_worth(
        &self,
        through: impl Into<RunsThrough>,
    ) -> Result<Option<u64>, StoreError> {
        let Some(_claim) = RebuildClaim::take(&self.rebuild_claim)? else {
            return Ok(None);
        };
        self.start_over_and_run(&through.into()).await.map(Some)
    }

    /// The whole table, for a caller holding the claim.
    async fn start_over_and_run(&self, through: &RunsThrough) -> Result<u64, StoreError> {
        let mut tx = self.begin().await?;
        tx.ask_for_net_worth_rebuild().await?;
        let day = through.day(&mut tx).await?;
        let first = tx.rebuild_batch(day, ROWS_PER_TRANSACTION).await?;
        tx.commit().await?;
        if first.finished {
            return Ok(first.written);
        }
        tokio::time::sleep(PAUSE_BETWEEN_BATCHES).await;
        Ok(first.written + self.run_batches(through).await?)
    }

    /// Run the rebuild that is needed to its end, resuming one a crash or a failure
    /// interrupted, unless someone else is running it, which answers `None` and leaves it
    /// to them. `through` is the day it runs up to if no batch has run yet. Dropping the
    /// future loses nothing. Answers the rows it wrote.
    ///
    /// # Errors
    ///
    /// Backend failures, including a stored row that no longer parses.
    pub async fn finish_net_worth_rebuild(
        &self,
        through: impl Into<RunsThrough>,
    ) -> Result<Option<u64>, StoreError> {
        let Some(_claim) = RebuildClaim::take(&self.rebuild_claim)? else {
            return Ok(None);
        };
        self.run_batches(&through.into()).await.map(Some)
    }

    async fn run_batches(&self, through: &RunsThrough) -> Result<u64, StoreError> {
        let mut written = 0;
        loop {
            let mut tx = self.begin().await?;
            let day = through.day(&mut tx).await?;
            let batch: RebuildBatch = tx.rebuild_batch(day, ROWS_PER_TRANSACTION).await?;
            tx.commit().await?;
            written += batch.written;
            if batch.finished {
                return Ok(written);
            }
            tokio::time::sleep(PAUSE_BETWEEN_BATCHES).await;
        }
    }

    /// Fill the table up to `through`, a transaction at a time, and with a gap after
    /// each, as days after a long downtime can be too many for one. The table stays
    /// good throughout and says how far it has come. Stops if a rebuild is needed, which
    /// fills everything itself. Answers the rows written.
    ///
    /// # Errors
    ///
    /// Backend failures, including a stored row that no longer parses. The days committed
    /// stay, and the next call goes on from there.
    pub async fn fill_net_worth(&self, through: impl Into<RunsThrough>) -> Result<u64, StoreError> {
        let through = through.into();
        let mut written = 0;
        loop {
            let mut tx = self.begin().await?;
            let last = through.day(&mut tx).await?;
            written += tx.fill_net_worth(last).await?;
            let state = tx.net_worth_state().await?;
            tx.commit().await?;
            if state.current_through().is_none_or(|day| day >= last) {
                return Ok(written);
            }
            tokio::time::sleep(PAUSE_BETWEEN_BATCHES).await;
        }
    }
}

#[cfg(any(test, feature = "testkit"))]
impl Store {
    /// Holds the claim on running a rebuild until the answer is dropped, as a runner in
    /// the middle of one does. `None` if it is held already.
    ///
    /// # Errors
    ///
    /// Fails if the claim's file can't be opened or locked.
    pub fn hold_net_worth_rebuild_for_tests(
        &self,
    ) -> Result<Option<impl Send + use<>>, StoreError> {
        RebuildClaim::take(&self.rebuild_claim)
    }
}
