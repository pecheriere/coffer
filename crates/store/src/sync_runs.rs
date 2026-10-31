//! Sync runs: one row per requested sync of a connection, from queued to
//! its outcome.

use coffer_domain::ids::{AgentTokenId, ConnectionId, MemberId, SyncRunId};
use coffer_domain::sync::{SyncRunStatus, SyncTrigger};
use jiff::Timestamp;
use sqlx::sqlite::SqliteRow;
use sqlx::{QueryBuilder, Row, Sqlite, SqliteExecutor};

use crate::convert::{corrupt, parsed, parsed_opt, text_opt, time, time_opt};
use crate::{Page, PageRequest, Store, StoreError, StoreTx};

macro_rules! columns {
    () => {
        "id, connection_id, trigger, requested_by_member, requested_by_agent, status, detail, \
         queued_at, started_at, finished_at"
    };
}

/// A stored sync run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncRunRecord {
    pub id: SyncRunId,
    pub connection: ConnectionId,
    pub trigger: SyncTrigger,
    pub status: SyncRunStatus,
    /// The member who asked for it, and the agent token they used; `None`
    /// for the schedule.
    pub requested_by: Option<(MemberId, Option<AgentTokenId>)>,
    /// What happened, for a finished run.
    pub detail: Option<String>,
    pub queued_at: Timestamp,
    pub started_at: Option<Timestamp>,
    pub finished_at: Option<Timestamp>,
}

/// A run to queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSyncRun {
    pub connection: ConnectionId,
    pub trigger: SyncTrigger,
    pub requested_by: Option<(MemberId, Option<AgentTokenId>)>,
    pub queued_at: Timestamp,
}

/// A connection's recent runs, for rate limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunCounts {
    /// Runs queued or running, plus finished runs started at or after the
    /// given instant.
    pub started_or_pending: u32,
    /// When the most recent run started.
    pub last_started_at: Option<Timestamp>,
    /// Whether a run is queued or running.
    pub in_flight: bool,
}

fn sync_run_from_row(row: &SqliteRow) -> Result<SyncRunRecord, StoreError> {
    let member: Option<MemberId> = parsed_opt(row, "requested_by_member")?;
    let agent: Option<AgentTokenId> = parsed_opt(row, "requested_by_agent")?;
    let requested_by = match (member, agent) {
        (Some(member), agent) => Some((member, agent)),
        (None, None) => None,
        (None, Some(_)) => return Err(corrupt("an agent without its member")),
    };
    Ok(SyncRunRecord {
        id: parsed(row, "id")?,
        connection: parsed(row, "connection_id")?,
        trigger: parsed(row, "trigger")?,
        status: parsed(row, "status")?,
        requested_by,
        detail: text_opt(row, "detail")?,
        queued_at: time(row, "queued_at")?,
        started_at: time_opt(row, "started_at")?,
        finished_at: time_opt(row, "finished_at")?,
    })
}

async fn fetch_sync_run<'e>(
    executor: impl SqliteExecutor<'e>,
    id: SyncRunId,
) -> Result<SyncRunRecord, StoreError> {
    let row = sqlx::query(concat!(
        "SELECT ",
        columns!(),
        " FROM sync_runs WHERE id = ?"
    ))
    .bind(id.to_string())
    .fetch_one(executor)
    .await
    .map_err(StoreError::from_sqlx)?;
    sync_run_from_row(&row)
}

impl StoreTx {
    /// Queue a run.
    ///
    /// # Errors
    ///
    /// Backend failures, including a connection, member or agent token that
    /// does not exist.
    pub async fn insert_sync_run(&mut self, new: NewSyncRun) -> Result<SyncRunRecord, StoreError> {
        let id = SyncRunId::new();
        let (member, agent) = match new.requested_by {
            Some((member, agent)) => (Some(member.to_string()), agent.map(|a| a.to_string())),
            None => (None, None),
        };
        sqlx::query(
            "INSERT INTO sync_runs (id, connection_id, trigger, requested_by_member, \
             requested_by_agent, status, queued_at) VALUES (?, ?, ?, ?, ?, 'queued', ?)",
        )
        .bind(id.to_string())
        .bind(new.connection.to_string())
        .bind(new.trigger.as_str())
        .bind(member)
        .bind(agent)
        .bind(new.queued_at.as_millisecond())
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        fetch_sync_run(&mut *self.tx, id).await
    }

    /// Mark a queued run `running`.
    ///
    /// # Errors
    ///
    /// `NotFound` if the run does not exist; [`StoreError::Conflict`] if it
    /// is not queued.
    pub async fn start_sync_run(
        &mut self,
        id: SyncRunId,
        at: Timestamp,
    ) -> Result<SyncRunRecord, StoreError> {
        let result = sqlx::query(
            "UPDATE sync_runs SET status = 'running', started_at = ? \
             WHERE id = ? AND status = 'queued'",
        )
        .bind(at.as_millisecond())
        .bind(id.to_string())
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        let run = fetch_sync_run(&mut *self.tx, id).await?;
        if result.rows_affected() == 0 {
            return Err(StoreError::Conflict("the sync run is not queued"));
        }
        Ok(run)
    }

    /// Record the outcome of a running run.
    ///
    /// # Errors
    ///
    /// [`StoreError::Invalid`] if `status` is not an outcome; `NotFound` if
    /// the run does not exist; [`StoreError::Conflict`] if it is not
    /// running.
    pub async fn finish_sync_run(
        &mut self,
        id: SyncRunId,
        status: SyncRunStatus,
        detail: Option<String>,
        at: Timestamp,
    ) -> Result<SyncRunRecord, StoreError> {
        if !status.is_finished() {
            return Err(StoreError::Invalid("a sync run finishes with an outcome"));
        }
        let result = sqlx::query(
            "UPDATE sync_runs SET status = ?, detail = ?, finished_at = ? \
             WHERE id = ? AND status = 'running'",
        )
        .bind(status.as_str())
        .bind(detail)
        .bind(at.as_millisecond())
        .bind(id.to_string())
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        let run = fetch_sync_run(&mut *self.tx, id).await?;
        if result.rows_affected() == 0 {
            return Err(StoreError::Conflict("the sync run is not running"));
        }
        Ok(run)
    }

    /// Mark every running run `failed` with `detail`, as after a restart
    /// interrupted them, and return how many there were. Queued runs stay
    /// queued.
    ///
    /// # Errors
    ///
    /// Backend failures.
    pub async fn fail_running_runs(
        &mut self,
        detail: &str,
        at: Timestamp,
    ) -> Result<u32, StoreError> {
        let result = sqlx::query(
            "UPDATE sync_runs SET status = 'failed', detail = ?, finished_at = ? \
             WHERE status = 'running'",
        )
        .bind(detail)
        .bind(at.as_millisecond())
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        Ok(u32::try_from(result.rows_affected()).unwrap_or(u32::MAX))
    }
}

impl Store {
    /// Fetch a run.
    ///
    /// # Errors
    ///
    /// `NotFound` if there is no such run.
    pub async fn get_sync_run(&self, id: SyncRunId) -> Result<SyncRunRecord, StoreError> {
        fetch_sync_run(&self.pool, id).await
    }

    /// A connection's runs, newest first by id (`UUIDv7`, so queueing
    /// order). The returned cursor continues after the last item of the
    /// page.
    ///
    /// # Errors
    ///
    /// [`StoreError::InvalidCursor`] if `page.after` is not a cursor issued by
    /// this method; otherwise a backend error.
    pub async fn list_sync_runs(
        &self,
        connection: ConnectionId,
        page: &PageRequest,
    ) -> Result<Page<SyncRunRecord>, StoreError> {
        let limit = page.effective_limit();
        let after = page
            .after
            .as_deref()
            .map(|cursor| {
                cursor
                    .parse::<SyncRunId>()
                    .map_err(|_| StoreError::InvalidCursor)
            })
            .transpose()?;
        let mut query = QueryBuilder::<Sqlite>::new(concat!(
            "SELECT ",
            columns!(),
            " FROM sync_runs WHERE connection_id = "
        ));
        query.push_bind(connection.to_string());
        if let Some(after) = after {
            query.push(" AND id < ").push_bind(after.to_string());
        }
        // One extra row tells whether another page exists.
        query
            .push(" ORDER BY id DESC LIMIT ")
            .push_bind(i64::from(limit) + 1);
        let rows = query
            .build()
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::from_sqlx)?;
        let mut items = rows
            .iter()
            .map(sync_run_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        let next = if items.len() > limit as usize {
            items.truncate(limit as usize);
            items.last().map(|run| run.id.to_string())
        } else {
            None
        };
        Ok(Page { items, next })
    }

    /// The counts rate limits use for `connection`: runs queued, running or
    /// started at or after `since`, the last start, and whether a run is in
    /// flight.
    ///
    /// # Errors
    ///
    /// Backend failures.
    pub async fn sync_run_counts(
        &self,
        connection: ConnectionId,
        since: Timestamp,
    ) -> Result<RunCounts, StoreError> {
        let row = sqlx::query(
            "SELECT \
             COALESCE(SUM(status IN ('queued', 'running') OR started_at >= ?), 0) AS counted, \
             MAX(started_at) AS last_started_at, \
             COALESCE(MAX(status IN ('queued', 'running')), 0) AS in_flight \
             FROM sync_runs WHERE connection_id = ?",
        )
        .bind(since.as_millisecond())
        .bind(connection.to_string())
        .fetch_one(&self.pool)
        .await
        .map_err(StoreError::from_sqlx)?;
        let counted: i64 = row.try_get("counted").map_err(StoreError::from_sqlx)?;
        let in_flight: i64 = row.try_get("in_flight").map_err(StoreError::from_sqlx)?;
        Ok(RunCounts {
            started_or_pending: u32::try_from(counted).map_err(StoreError::backend)?,
            last_started_at: time_opt(&row, "last_started_at")?,
            in_flight: in_flight != 0,
        })
    }

    /// The oldest queued run of any connection.
    ///
    /// # Errors
    ///
    /// Backend failures, or a row that no longer parses.
    pub async fn next_queued_run(&self) -> Result<Option<SyncRunRecord>, StoreError> {
        sqlx::query(concat!(
            "SELECT ",
            columns!(),
            " FROM sync_runs WHERE status = 'queued' ORDER BY queued_at, id LIMIT 1"
        ))
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::from_sqlx)?
        .as_ref()
        .map(sync_run_from_row)
        .transpose()
    }
}
