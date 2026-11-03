//! The daily table (spec §8.10 N3): one row per account per day, derived from
//! `balance_snapshots`, `fx_rates` and the display currency. Only this module
//! writes it. Every change to a source calls in here within its own transaction,
//! so no caller can forget, and every path computes rows with `daily_values`, so
//! they share one rule. `filled_through` and `rebuild_needed` say what the table
//! can't hold yet.
//!
//! A change that rewrites more rows than one transaction should hold asks for a
//! full rebuild instead, and a full rebuild runs in batches of whole accounts, in id
//! order, each in its own short transaction, so other writers get their turn
//! whatever the table's size. While it runs `rebuild_needed` stays set, so no report
//! reads a mix of old and new rows. `rebuilt_up_to` says how far it has come: a
//! change keeps the accounts up to there current as usual and leaves the others to
//! the rebuild, so a write between two batches is never lost, and a crash resumes
//! from there. A change that alters every row starts it over. `runner` runs the
//! batches, one process at a time, with a gap after each.

use std::collections::BTreeMap;

use coffer_domain::fx::{RateBook, RateLeg};
use coffer_domain::ids::AccountId;
use coffer_domain::money::Currency;
use coffer_domain::net_worth::{DailyValue, MIN_VALUE_DATE, SnapshotPoint, daily_values};
use jiff::Span;
use jiff::civil::Date;
use sqlx::sqlite::SqliteRow;
use sqlx::{QueryBuilder, Sqlite, SqliteConnection, SqliteExecutor};

use crate::convert::{corrupt, int, parsed, parsed_opt, text};
use crate::fx::load_rate_book;
use crate::household::fetch_display_currency;
use crate::{StoreError, StoreTx};

mod reads;
mod runner;

pub use reads::NetWorthData;

/// Rows per `INSERT`: ten binds each stay far below `SQLite`'s limit.
const BATCH_ROWS: usize = 500;

/// The most rows a transaction here rewrites for a change or a batch of a rebuild,
/// unless one account alone holds more. That is about an account's whole history from
/// 1970, which a back-dated balance rewrites in its own transaction anyway.
const ROWS_PER_TRANSACTION: usize = 25_000;

/// How far the daily table can be trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetWorthState {
    /// The last day the table holds, if any.
    pub filled_through: Option<Date>,
    /// The table has to be rebuilt before it can answer.
    pub rebuild_needed: bool,
}

impl NetWorthState {
    /// The last day of a table that can answer. While a rebuild runs there is none,
    /// whatever day it is rebuilding up to.
    #[must_use]
    pub fn current_through(self) -> Option<Date> {
        self.filled_through.filter(|_| !self.rebuild_needed)
    }
}

/// What one batch of a rebuild did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RebuildBatch {
    /// The rows it wrote.
    pub written: u64,
    /// No rebuild is left to run.
    pub finished: bool,
}

/// The rows a change to a source has to keep current.
struct KeptCurrent {
    through: Date,
    /// The last account a running rebuild has finished; with none running, every
    /// account is kept current.
    up_to: Option<AccountId>,
}

impl KeptCurrent {
    fn covers(&self, account: AccountId) -> bool {
        self.up_to.is_none_or(|last| account <= last)
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

/// The rates converting `holdings` for `[from, through]`.
async fn rate_book(
    conn: &mut SqliteConnection,
    holdings: &[Holding],
    display: Option<Currency>,
    from: Date,
    through: Date,
) -> Result<RateBook, StoreError> {
    let Some(display) = display else {
        return Ok(RateBook::default());
    };
    let mut currencies: Vec<_> = holdings.iter().map(|one| one.currency).collect();
    currencies.push(display);
    currencies.sort_unstable();
    currencies.dedup();
    load_rate_book(conn, &currencies, from, through).await
}

async fn rows_of_holding(
    conn: &mut SqliteConnection,
    holding: &Holding,
    display: Option<Currency>,
    book: &RateBook,
    from: Date,
    through: Date,
) -> Result<Vec<DailyValue>, StoreError> {
    let snapshots = snapshot_points(conn, holding.id, from, through).await?;
    Ok(daily_values(
        holding.id,
        holding.currency,
        &snapshots,
        display,
        book,
        from,
        through,
    ))
}

async fn insert_all(conn: &mut SqliteConnection, rows: &[DailyValue]) -> Result<u64, StoreError> {
    let mut written = 0;
    for batch in rows.chunks(BATCH_ROWS) {
        written += insert_rows(&mut *conn, batch).await?;
    }
    Ok(written)
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
    let book = rate_book(&mut *conn, holdings, display, from, through).await?;
    let mut written = 0;
    for holding in holdings {
        let rows = rows_of_holding(&mut *conn, holding, display, &book, from, through).await?;
        written += insert_all(&mut *conn, &rows).await?;
    }
    Ok(written)
}

/// The first snapshot's day of each account that has one.
async fn first_days(conn: &mut SqliteConnection) -> Result<BTreeMap<AccountId, Date>, StoreError> {
    sqlx::query("SELECT account_id, MIN(on_date) AS first_on FROM balance_snapshots GROUP BY 1")
        .fetch_all(conn)
        .await
        .map_err(StoreError::from_sqlx)?
        .iter()
        .map(|row| Ok((parsed(row, "account_id")?, parsed(row, "first_on")?)))
        .collect()
}

/// The rows an account has in `[from, through]`: one per day from its first snapshot,
/// or from `from` if later, as `daily_values` makes them.
fn row_count(first_day: Option<Date>, from: Date, through: Date) -> Result<usize, StoreError> {
    let Some(first_day) = first_day else {
        return Ok(0);
    };
    let start = first_day.max(from).max(MIN_VALUE_DATE);
    if start > through {
        return Ok(0);
    }
    let days = through.since(start).map_err(StoreError::backend)?;
    usize::try_from(days.get_days())
        .map_or_else(|err| Err(StoreError::backend(err)), |days| Ok(days + 1))
}

/// Whether writing `holdings` for `[from, through]` stays within one transaction's rows.
async fn fits_one_transaction(
    conn: &mut SqliteConnection,
    holdings: &[Holding],
    from: Date,
    through: Date,
) -> Result<bool, StoreError> {
    let first_days = first_days(conn).await?;
    let mut total = 0;
    for holding in holdings {
        total += row_count(first_days.get(&holding.id).copied(), from, through)?;
    }
    Ok(total <= ROWS_PER_TRANSACTION)
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

    /// What a change has to keep current: nothing while a rebuild needed hasn't
    /// finished an account yet.
    async fn kept_current(&mut self) -> Result<Option<KeptCurrent>, StoreError> {
        let state = self.net_worth_state().await?;
        let Some(through) = state.filled_through else {
            return Ok(None);
        };
        if !state.rebuild_needed {
            return Ok(Some(KeptCurrent {
                through,
                up_to: None,
            }));
        }
        Ok(self.rebuilt_up_to().await?.map(|last| KeptCurrent {
            through,
            up_to: Some(last),
        }))
    }

    async fn rebuilt_up_to(&mut self) -> Result<Option<AccountId>, StoreError> {
        let row = sqlx::query("SELECT rebuilt_up_to FROM net_worth_state WHERE id = 1")
            .fetch_one(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;
        parsed_opt(&row, "rebuilt_up_to")
    }

    /// Asks for a full rebuild, from the first account, within this transaction; a
    /// running one starts over. This is how a change that alters every row says so
    /// without holding the lock for them: from its commit a report answers as rebuilding,
    /// and the daily job runs the rebuild at its next pass, within a minute. A caller
    /// that can't wait for that calls [`crate::Store::finish_net_worth_rebuild`] after its
    /// commit.
    ///
    /// # Errors
    ///
    /// Backend failures.
    pub async fn ask_for_net_worth_rebuild(&mut self) -> Result<(), StoreError> {
        sqlx::query("UPDATE net_worth_state SET rebuild_needed = 1, rebuilt_up_to = NULL")
            .execute(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;
        Ok(())
    }

    async fn set_filled_through(&mut self, through: Date) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE net_worth_state SET filled_through = ?, rebuild_needed = 0, \
             rebuilt_up_to = NULL",
        )
        .bind(through.to_string())
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        Ok(())
    }

    /// Add the rows of the days after `filled_through`, up to `through` or as many days
    /// as fit in one transaction's rows, and move `filled_through` to the last of them.
    /// It never moves back, so a changed time zone can't lose a day, and while a rebuild
    /// is needed it does nothing. Answers the rows written. [`crate::Store::fill_net_worth`]
    /// repeats it up to `through`.
    ///
    /// # Errors
    ///
    /// Backend failures, including a stored row that no longer parses.
    pub async fn fill_net_worth(&mut self, through: Date) -> Result<u64, StoreError> {
        let Some(filled) = self.net_worth_state().await?.current_through() else {
            return Ok(0);
        };
        if through <= filled {
            return Ok(0);
        }
        let from = filled.tomorrow().map_err(StoreError::backend)?;
        let holdings = every_holding(&mut self.tx).await?;
        // Each account has at most a row a day, and a day always fits, or it never ends.
        let days = ROWS_PER_TRANSACTION / holdings.len().max(1);
        let more = i64::try_from(days.max(1) - 1).map_err(StoreError::backend)?;
        let end = from.saturating_add(Span::new().days(more)).min(through);
        let written = write_rows(&mut self.tx, &holdings, from, end).await?;
        self.set_filled_through(end).await?;
        Ok(written)
    }

    /// One batch of the rebuild that is needed, for a test to stop between two.
    /// Anything else goes through the runner, which keeps the gap between batches.
    ///
    /// # Errors
    ///
    /// Backend failures, including a stored row that no longer parses.
    #[cfg(any(test, feature = "testkit"))]
    pub async fn rebuild_net_worth_batch(
        &mut self,
        through: Date,
        max_rows: usize,
    ) -> Result<RebuildBatch, StoreError> {
        self.rebuild_batch(through, max_rows).await
    }

    /// One batch of the rebuild that is needed: replaces the rows of the next accounts
    /// in id order, computing each from its first snapshot, stopping before `max_rows`
    /// unless the first account alone is more. The last batch clears `rebuild_needed`.
    ///
    /// The first batch makes `through` the day the rebuild runs up to, and that day
    /// becomes `filled_through` even if it is earlier: everything is recomputed, so this
    /// is how a clock that jumped ahead is undone. Later batches keep to that day,
    /// whatever `through` they are given, as the rows before them were computed for it.
    /// With no rebuild needed this does nothing.
    async fn rebuild_batch(
        &mut self,
        through: Date,
        max_rows: usize,
    ) -> Result<RebuildBatch, StoreError> {
        let state = self.net_worth_state().await?;
        if !state.rebuild_needed {
            return Ok(RebuildBatch {
                written: 0,
                finished: true,
            });
        }
        let done = self.rebuilt_up_to().await?;
        let through = match (done, state.filled_through) {
            (Some(_), Some(day)) => day,
            _ => through,
        };
        let waiting: Vec<_> = every_holding(&mut self.tx)
            .await?
            .into_iter()
            .filter(|one| done.is_none_or(|last| one.id > last))
            .collect();
        let first_days = first_days(&mut self.tx).await?;
        let mut batch = 0;
        let mut in_batch = 0;
        for holding in &waiting {
            let first_day = first_days.get(&holding.id).copied();
            let rows = row_count(first_day, MIN_VALUE_DATE, through)?;
            if batch > 0 && in_batch + rows > max_rows {
                break;
            }
            in_batch += rows;
            batch += 1;
        }
        let (holdings, rest) = waiting.split_at(batch);

        let display = fetch_display_currency(&mut *self.tx).await?;
        let book = rate_book(&mut self.tx, holdings, display, MIN_VALUE_DATE, through).await?;
        let mut written = 0;
        for holding in holdings {
            sqlx::query("DELETE FROM account_daily_values WHERE account_id = ?")
                .bind(holding.id.to_string())
                .execute(&mut *self.tx)
                .await
                .map_err(StoreError::from_sqlx)?;
            let rows = rows_of_holding(
                &mut self.tx,
                holding,
                display,
                &book,
                MIN_VALUE_DATE,
                through,
            )
            .await?;
            written += insert_all(&mut self.tx, &rows).await?;
        }
        match holdings.last().filter(|_| !rest.is_empty()) {
            Some(last) => {
                sqlx::query("UPDATE net_worth_state SET filled_through = ?, rebuilt_up_to = ?")
                    .bind(through.to_string())
                    .bind(last.id.to_string())
                    .execute(&mut *self.tx)
                    .await
                    .map_err(StoreError::from_sqlx)?;
            }
            None => self.set_filled_through(through).await?,
        }
        Ok(RebuildBatch {
            written,
            finished: rest.is_empty(),
        })
    }

    /// A snapshot's value or source changed: rewrite its stretch, from its day to
    /// the day before the next snapshot.
    pub(crate) async fn refresh_after_snapshot(
        &mut self,
        account: AccountId,
        on: Date,
    ) -> Result<(), StoreError> {
        let Some(kept) = self
            .kept_current()
            .await?
            .filter(|kept| kept.covers(account))
        else {
            return Ok(());
        };
        let filled = kept.through;
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
        let Some(kept) = self
            .kept_current()
            .await?
            .filter(|kept| kept.covers(account))
        else {
            return Ok(());
        };
        self.refresh_account(account, first_day, kept.through).await
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
    /// every account in another. With no display currency nothing is converted. Too
    /// many rows for one transaction, as a first fetch of a long history can be, asks
    /// for a rebuild instead.
    pub(crate) async fn refresh_after_rate(
        &mut self,
        currency: Currency,
        from: Date,
    ) -> Result<(), StoreError> {
        let Some(kept) = self.kept_current().await? else {
            return Ok(());
        };
        let Some(display) = fetch_display_currency(&mut *self.tx).await? else {
            return Ok(());
        };
        let holdings: Vec<_> = every_holding(&mut self.tx)
            .await?
            .into_iter()
            .filter(|one| {
                kept.covers(one.id) && (one.currency == currency) != (currency == display)
            })
            .collect();
        if fits_one_transaction(&mut self.tx, &holdings, from, kept.through).await? {
            write_rows(&mut self.tx, &holdings, from, kept.through).await?;
            Ok(())
        } else {
            self.ask_for_net_worth_rebuild().await
        }
    }

    /// The display currency changed, so every row has to. In this one transaction
    /// when they fit in one; if not, or if a rebuild is running, whose rows so far are
    /// in the old currency, a rebuild from the first account.
    pub(crate) async fn refresh_after_display_change(&mut self) -> Result<(), StoreError> {
        let Some(filled) = self.net_worth_state().await?.current_through() else {
            return self.ask_for_net_worth_rebuild().await;
        };
        let holdings = every_holding(&mut self.tx).await?;
        if fits_one_transaction(&mut self.tx, &holdings, MIN_VALUE_DATE, filled).await? {
            write_rows(&mut self.tx, &holdings, MIN_VALUE_DATE, filled).await?;
            Ok(())
        } else {
            self.ask_for_net_worth_rebuild().await
        }
    }
}

#[cfg(test)]
mod tests;
