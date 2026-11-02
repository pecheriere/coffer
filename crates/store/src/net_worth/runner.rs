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

use jiff::civil::Date;

use super::{ROWS_PER_TRANSACTION, RebuildBatch};
use crate::{Store, StoreError};

/// The wait after each transaction. A writer waiting for the lock sleeps up to 100 ms
/// between tries, so a shorter gap could pass unnoticed time after time.
pub(super) const PAUSE_BETWEEN_BATCHES: Duration = Duration::from_millis(150);

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
    /// and other writers get their turn between batches. Answers the rows it wrote.
    ///
    /// # Errors
    ///
    /// Backend failures, including a stored row that no longer parses. What committed
    /// stays, and [`Self::finish_net_worth_rebuild`] goes on from there.
    pub async fn rebuild_net_worth(&self, through: Date) -> Result<u64, StoreError> {
        let _claim = loop {
            if let Some(claim) = RebuildClaim::take(&self.rebuild_claim)? {
                break claim;
            }
            tokio::time::sleep(PAUSE_BETWEEN_BATCHES).await;
        };
        let mut tx = self.begin().await?;
        tx.ask_for_net_worth_rebuild().await?;
        let first = tx.rebuild_batch(through, ROWS_PER_TRANSACTION).await?;
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
    pub async fn finish_net_worth_rebuild(&self, through: Date) -> Result<Option<u64>, StoreError> {
        let Some(_claim) = RebuildClaim::take(&self.rebuild_claim)? else {
            return Ok(None);
        };
        self.run_batches(through).await.map(Some)
    }

    async fn run_batches(&self, through: Date) -> Result<u64, StoreError> {
        let mut written = 0;
        loop {
            let mut tx = self.begin().await?;
            let batch: RebuildBatch = tx.rebuild_batch(through, ROWS_PER_TRANSACTION).await?;
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
    pub async fn fill_net_worth(&self, through: Date) -> Result<u64, StoreError> {
        let mut written = 0;
        loop {
            let mut tx = self.begin().await?;
            written += tx.fill_net_worth(through).await?;
            let state = tx.net_worth_state().await?;
            tx.commit().await?;
            if state.current_through().is_none_or(|day| day >= through) {
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
