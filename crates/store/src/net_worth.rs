//! The daily table (spec §8.10 N3): one row per account per day, derived from
//! `balance_snapshots`, `fx_rates` and the display currency. Only this module
//! writes it. Every change to a source calls in here within its own transaction,
//! so no caller can forget, and every path computes rows with `daily_values`, so
//! they share one rule. `filled_through` and `rebuild_needed` say what the table
//! can't hold yet, and while a rebuild is needed nothing is written at all.

use coffer_domain::fx::{RateBook, RateLeg};
use coffer_domain::ids::AccountId;
use coffer_domain::money::Currency;
use coffer_domain::net_worth::{DailyValue, MIN_VALUE_DATE, SnapshotPoint, daily_values};
use jiff::civil::Date;
use sqlx::sqlite::SqliteRow;
use sqlx::{QueryBuilder, Sqlite, SqliteConnection, SqliteExecutor};

use crate::convert::{corrupt, int, parsed, parsed_opt, text};
use crate::fx::load_rate_book;
use crate::household::fetch_display_currency;
use crate::{StoreError, StoreTx};

mod reads;

pub use reads::NetWorthData;

/// Rows per `INSERT`: ten binds each stay far below `SQLite`'s limit.
const BATCH_ROWS: usize = 500;

/// How far the daily table can be trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetWorthState {
    /// The last day the table holds, if any.
    pub filled_through: Option<Date>,
    /// The table has to be rebuilt before it can answer.
    pub rebuild_needed: bool,
}

impl NetWorthState {
    /// The last day of a table that can answer and that changes keep current.
    fn current_through(self) -> Option<Date> {
        self.filled_through.filter(|_| !self.rebuild_needed)
    }
}

/// An account as the table needs it.
struct Holding {
    id: AccountId,
    currency: Currency,
}

fn holding_from_row(row: &SqliteRow) -> Result<Holding, StoreError> {
    Ok(Holding {
        id: parsed(row, "id")?,
        currency: Currency::parse(&text(row, "currency")?).map_err(StoreError::backend)?,
    })
}

async fn every_holding(conn: &mut SqliteConnection) -> Result<Vec<Holding>, StoreError> {
    sqlx::query("SELECT id, currency FROM accounts ORDER BY id")
        .fetch_all(conn)
        .await
        .map_err(StoreError::from_sqlx)?
        .iter()
        .map(holding_from_row)
        .collect()
}

pub(crate) async fn fetch_state<'e>(
    executor: impl SqliteExecutor<'e>,
) -> Result<NetWorthState, StoreError> {
    let row =
        sqlx::query("SELECT filled_through, rebuild_needed FROM net_worth_state WHERE id = 1")
            .fetch_one(executor)
            .await
            .map_err(StoreError::from_sqlx)?;
    Ok(NetWorthState {
        filled_through: parsed_opt(&row, "filled_through")?,
        rebuild_needed: match int(&row, "rebuild_needed")? {
            0 => false,
            1 => true,
            _ => return Err(corrupt("flag is neither 0 nor 1")),
        },
    })
}

/// The snapshots behind `[from, through]`: those inside it and the latest before
/// it, which it carries forward.
async fn snapshot_points(
    conn: &mut SqliteConnection,
    account: AccountId,
    from: Date,
    through: Date,
) -> Result<Vec<SnapshotPoint>, StoreError> {
    let mut rows = sqlx::query(
        "SELECT on_date, balance_minor, source FROM balance_snapshots \
         WHERE account_id = ? AND on_date < ? ORDER BY on_date DESC LIMIT 1",
    )
    .bind(account.to_string())
    .bind(from.to_string())
    .fetch_all(&mut *conn)
    .await
    .map_err(StoreError::from_sqlx)?;
    rows.extend(
        sqlx::query(
            "SELECT on_date, balance_minor, source FROM balance_snapshots \
             WHERE account_id = ? AND on_date BETWEEN ? AND ?",
        )
        .bind(account.to_string())
        .bind(from.to_string())
        .bind(through.to_string())
        .fetch_all(&mut *conn)
        .await
        .map_err(StoreError::from_sqlx)?,
    );
    rows.iter()
        .map(|row| {
            Ok(SnapshotPoint {
                on: parsed(row, "on_date")?,
                balance_minor: int(row, "balance_minor")?,
                source: parsed(row, "source")?,
            })
        })
        .collect()
}

/// Writes one batch, replacing any row of the same account and day.
async fn insert_rows(conn: &mut SqliteConnection, rows: &[DailyValue]) -> Result<u64, StoreError> {
    let mut query = QueryBuilder::<Sqlite>::new(
        "INSERT OR REPLACE INTO account_daily_values (account_id, on_date, balance_minor, \
         balance_on, balance_source, display_currency, display_minor, rate_on, rate_leg, \
         conversion) ",
    );
    query.push_values(rows, |mut bound, row| {
        bound
            .push_bind(row.account.to_string())
            .push_bind(row.on.to_string())
            .push_bind(row.balance_minor)
            .push_bind(row.balance_on.to_string())
            .push_bind(row.balance_source.as_str())
            .push_bind(row.display.map(|code| code.to_string()))
            .push_bind(row.display_minor)
            .push_bind(row.rate_on.map(|on| on.to_string()))
            .push_bind(row.rate_leg.map(RateLeg::as_str))
            .push_bind(row.conversion.as_str());
    });
    let done = query
        .build()
        .execute(conn)
        .await
        .map_err(StoreError::from_sqlx)?;
    Ok(done.rows_affected())
}

/// Computes `holdings` for `[from, through]` and writes each row, in batches.
/// Answers how many.
async fn write_rows(
    conn: &mut SqliteConnection,
    holdings: &[Holding],
    from: Date,
    through: Date,
) -> Result<u64, StoreError> {
    let display = fetch_display_currency(&mut *conn).await?;
    let book = match display {
        Some(display) => {
            let mut currencies: Vec<_> = holdings.iter().map(|one| one.currency).collect();
            currencies.push(display);
            currencies.sort_unstable();
            currencies.dedup();
            load_rate_book(&mut *conn, &currencies, from, through).await?
        }
        None => RateBook::default(),
    };
    let mut written = 0;
    for holding in holdings {
        let snapshots = snapshot_points(&mut *conn, holding.id, from, through).await?;
        let rows = daily_values(
            holding.id,
            holding.currency,
            &snapshots,
            display,
            &book,
            from,
            through,
        );
        for batch in rows.chunks(BATCH_ROWS) {
            written += insert_rows(&mut *conn, batch).await?;
        }
    }
    Ok(written)
}

impl StoreTx {
    /// The table's state, as seen by this transaction.
    ///
    /// # Errors
    ///
    /// Backend failures, including a stored row that no longer parses.
    pub async fn net_worth_state(&mut self) -> Result<NetWorthState, StoreError> {
        fetch_state(&mut *self.tx).await
    }

    async fn current_through(&mut self) -> Result<Option<Date>, StoreError> {
        Ok(self.net_worth_state().await?.current_through())
    }

    async fn set_filled_through(&mut self, through: Date) -> Result<(), StoreError> {
        sqlx::query("UPDATE net_worth_state SET filled_through = ?, rebuild_needed = 0")
            .bind(through.to_string())
            .execute(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;
        Ok(())
    }

    /// Add the rows of `(filled_through, through]` and move `filled_through` to
    /// `through`. It never moves back, so a `COFFER_TIMEZONE` changed at a restart
    /// can't lose a day, and while a rebuild is needed it does nothing. Answers the
    /// rows written.
    ///
    /// # Errors
    ///
    /// Backend failures, including a stored row that no longer parses.
    pub async fn fill_net_worth(&mut self, through: Date) -> Result<u64, StoreError> {
        let Some(filled) = self.current_through().await? else {
            return Ok(0);
        };
        if through <= filled {
            return Ok(0);
        }
        let from = filled.tomorrow().map_err(StoreError::backend)?;
        let holdings = every_holding(&mut self.tx).await?;
        let written = write_rows(&mut self.tx, &holdings, from, through).await?;
        self.set_filled_through(through).await?;
        Ok(written)
    }

    /// Replace every row, computing every account from its first snapshot, up to
    /// `through`, and clear `rebuild_needed`. `through` becomes `filled_through` even
    /// if it is earlier: everything is recomputed, so this is how a clock that jumped
    /// ahead is undone. Answers the rows written.
    ///
    /// It holds the write lock throughout. 110,000 rows took about 0.5 s in a release
    /// build on a desktop machine; it has not been measured on a small board, and the
    /// first deploy measures it on the Pi against the store's 15 s busy timeout.
    ///
    /// # Errors
    ///
    /// Backend failures, including a stored row that no longer parses.
    pub async fn rebuild_net_worth(&mut self, through: Date) -> Result<u64, StoreError> {
        sqlx::query("DELETE FROM account_daily_values")
            .execute(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;
        let holdings = every_holding(&mut self.tx).await?;
        let written = write_rows(&mut self.tx, &holdings, MIN_VALUE_DATE, through).await?;
        self.set_filled_through(through).await?;
        Ok(written)
    }

    /// A snapshot's value or source changed: rewrite its stretch, from its day to
    /// the day before the next snapshot.
    pub(crate) async fn refresh_after_snapshot(
        &mut self,
        account: AccountId,
        on: Date,
    ) -> Result<(), StoreError> {
        let Some(filled) = self.current_through().await? else {
            return Ok(());
        };
        let next: Option<String> = sqlx::query_scalar(
            "SELECT MIN(on_date) FROM balance_snapshots WHERE account_id = ? AND on_date > ?",
        )
        .bind(account.to_string())
        .bind(on.to_string())
        .fetch_one(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        let through = match next {
            Some(next) => {
                let next: Date = next.parse().map_err(StoreError::backend)?;
                next.yesterday().map_err(StoreError::backend)?.min(filled)
            }
            None => filled,
        };
        self.refresh_account(account, on, through).await
    }

    /// A bind wrote kept balances from `first_day` on. They interleave with the
    /// snapshots already there, so the whole account is rewritten from that day.
    pub(crate) async fn refresh_after_bring_in(
        &mut self,
        account: AccountId,
        first_day: Date,
    ) -> Result<(), StoreError> {
        let Some(filled) = self.current_through().await? else {
            return Ok(());
        };
        self.refresh_account(account, first_day, filled).await
    }

    async fn refresh_account(
        &mut self,
        account: AccountId,
        from: Date,
        through: Date,
    ) -> Result<(), StoreError> {
        let row = sqlx::query("SELECT id, currency FROM accounts WHERE id = ?")
            .bind(account.to_string())
            .fetch_one(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;
        let holding = holding_from_row(&row)?;
        write_rows(&mut self.tx, &[holding], from, through).await?;
        Ok(())
    }

    /// `currency`'s rate changed from `from` on: rewrite the accounts converted
    /// with it. That is those in `currency`, and when it is the display currency,
    /// every account in another. With no display currency nothing is converted.
    pub(crate) async fn refresh_after_rate(
        &mut self,
        currency: Currency,
        from: Date,
    ) -> Result<(), StoreError> {
        let Some(filled) = self.current_through().await? else {
            return Ok(());
        };
        let Some(display) = fetch_display_currency(&mut *self.tx).await? else {
            return Ok(());
        };
        let holdings: Vec<_> = every_holding(&mut self.tx)
            .await?
            .into_iter()
            .filter(|one| (one.currency == currency) != (currency == display))
            .collect();
        write_rows(&mut self.tx, &holdings, from, filled).await?;
        Ok(())
    }

    /// The household's zone changed, and its day is now `today`. A table ending on
    /// or before that is right, and the daily job fills any day it lacks. One past it
    /// holds a day still to come, so it is rebuilt to end today, in this transaction;
    /// the days after come back one at a time as they arrive, from the same snapshots,
    /// so there is no gap and no day twice. A table waiting for a rebuild gets it from
    /// the job, in whatever zone applies by then.
    pub(crate) async fn refresh_after_zone_change(
        &mut self,
        today: Date,
    ) -> Result<(), StoreError> {
        if self
            .current_through()
            .await?
            .is_some_and(|filled| filled > today)
        {
            self.rebuild_net_worth(today).await?;
        }
        Ok(())
    }

    /// The display currency changed: every row, in this one transaction.
    pub(crate) async fn refresh_after_display_change(&mut self) -> Result<(), StoreError> {
        let Some(filled) = self.current_through().await? else {
            return Ok(());
        };
        let holdings = every_holding(&mut self.tx).await?;
        write_rows(&mut self.tx, &holdings, MIN_VALUE_DATE, filled).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
