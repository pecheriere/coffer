//! Exchange rates and their coverage (spec §8.10). A rate is stored as the
//! canonical text of an `FxRate`, so two texts are equal exactly when the
//! rates are, and the euro has no row.

use std::collections::BTreeSet;

use coffer_domain::fx::{
    FxCoverage, FxErrorCode, FxFetchStatus, FxRate, FxSource, RateBook, RateHistory,
    RateObservation,
};
use coffer_domain::money::Currency;
use jiff::Timestamp;
use jiff::civil::Date;
use sqlx::sqlite::SqliteRow;
use sqlx::{QueryBuilder, Sqlite, SqliteConnection};

use crate::convert::{parsed, parsed_opt, text, time, time_opt};
use crate::{Store, StoreError, StoreTx};

/// The most rates one `Store::fx_rates` call returns.
const MAX_RATES_PER_READ: i64 = 1_000;

/// Rows per `INSERT`: five binds each stay far below `SQLite`'s limit.
const UPSERT_CHUNK: usize = 500;

/// The earlier of two days, either of which may be missing.
fn earlier(one: Option<Date>, other: Option<Date>) -> Option<Date> {
    match (one, other) {
        (Some(one), Some(other)) => Some(one.min(other)),
        (one, other) => one.or(other),
    }
}

fn observation_from_row(row: &SqliteRow) -> Result<RateObservation, StoreError> {
    Ok(RateObservation {
        on: parsed(row, "on_date")?,
        rate: parsed::<FxRate>(row, "rate")?,
    })
}

pub(crate) fn coverage_from_row(row: &SqliteRow) -> Result<FxCoverage, StoreError> {
    Ok(FxCoverage {
        currency: Currency::parse(&text(row, "currency")?).map_err(StoreError::backend)?,
        status: parsed(row, "status")?,
        first_on: parsed_opt(row, "first_on")?,
        last_on: parsed_opt(row, "last_on")?,
        last_attempt_at: time(row, "last_attempt_at")?,
        last_success_at: time_opt(row, "last_success_at")?,
        last_error: parsed_opt(row, "last_error")?,
    })
}

/// The rates `currencies` need to value every day in `[from, to]`: each one's
/// rates inside the window and its latest before it, which a weekend or a
/// holiday carries forward. The euro has no rates to load.
pub(crate) async fn load_rate_book(
    conn: &mut SqliteConnection,
    currencies: &[Currency],
    from: Date,
    to: Date,
) -> Result<RateBook, StoreError> {
    let mut book = RateBook::default();
    for &currency in currencies {
        if currency.as_str() == "EUR" {
            continue;
        }
        let mut rows = sqlx::query(
            "SELECT on_date, rate FROM fx_rates \
             WHERE quote = ? AND on_date BETWEEN ? AND ?",
        )
        .bind(currency.as_str())
        .bind(from.to_string())
        .bind(to.to_string())
        .fetch_all(&mut *conn)
        .await
        .map_err(StoreError::from_sqlx)?;
        rows.extend(
            sqlx::query(
                "SELECT on_date, rate FROM fx_rates \
                 WHERE quote = ? AND on_date < ? ORDER BY on_date DESC LIMIT 1",
            )
            .bind(currency.as_str())
            .bind(from.to_string())
            .fetch_all(&mut *conn)
            .await
            .map_err(StoreError::from_sqlx)?,
        );
        let observations = rows
            .iter()
            .map(observation_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        book.insert(currency, RateHistory::new(observations));
    }
    Ok(book)
}

impl Store {
    /// What is known about fetching each currency's rates, by currency. A
    /// currency never tried has no entry.
    ///
    /// This reveals every currency an account holds: a caller must not
    /// return it to anyone below `all` accounts at `balances`.
    ///
    /// # Errors
    ///
    /// Backend failures, including a stored row that no longer parses.
    pub async fn fx_coverage(&self) -> Result<Vec<FxCoverage>, StoreError> {
        sqlx::query("SELECT * FROM fx_coverage ORDER BY currency")
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::from_sqlx)?
            .iter()
            .map(coverage_from_row)
            .collect()
    }

    /// `currency`'s rates for the days in `[from, to]`, newest first, at most
    /// 1,000 of them: the newest of a longer range.
    ///
    /// Which currencies have rates reveals the currencies accounts hold: a
    /// caller must not return it to anyone below `all` accounts at
    /// `balances`.
    ///
    /// # Errors
    ///
    /// Backend failures, including a stored rate that no longer parses.
    pub async fn fx_rates(
        &self,
        currency: Currency,
        from: Date,
        to: Date,
    ) -> Result<Vec<RateObservation>, StoreError> {
        sqlx::query(
            "SELECT on_date, rate FROM fx_rates \
             WHERE quote = ? AND on_date BETWEEN ? AND ? \
             ORDER BY on_date DESC LIMIT ?",
        )
        .bind(currency.as_str())
        .bind(from.to_string())
        .bind(to.to_string())
        .bind(MAX_RATES_PER_READ)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::from_sqlx)?
        .iter()
        .map(observation_from_row)
        .collect()
    }

    /// The currencies accounts hold, closed ones included, in code order.
    ///
    /// This reveals every account's currency: a caller must not return it to
    /// anyone below `all` accounts at `balances`.
    ///
    /// # Errors
    ///
    /// Backend failures, including a stored code that no longer parses.
    pub async fn currencies_in_use(&self) -> Result<Vec<Currency>, StoreError> {
        sqlx::query("SELECT DISTINCT currency FROM accounts ORDER BY currency")
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::from_sqlx)?
            .iter()
            .map(|row| Currency::parse(&text(row, "currency")?).map_err(StoreError::backend))
            .collect()
    }
}

#[cfg(any(test, feature = "testkit"))]
impl Store {
    /// Make every later write of GBP's coverage fail, as a store that can read
    /// and can't write would for that row.
    ///
    /// # Errors
    ///
    /// Fails if no connection is available or a trigger cannot be created.
    pub async fn inject_fx_coverage_fault_for_tests(&self) -> Result<(), StoreError> {
        for trigger in [
            "CREATE TRIGGER injected_fx_coverage_insert_fault BEFORE INSERT ON fx_coverage \
             WHEN NEW.currency = 'GBP' BEGIN SELECT RAISE(ABORT, 'injected fault'); END",
            "CREATE TRIGGER injected_fx_coverage_update_fault BEFORE UPDATE ON fx_coverage \
             WHEN NEW.currency = 'GBP' BEGIN SELECT RAISE(ABORT, 'injected fault'); END",
        ] {
            sqlx::query(trigger)
                .execute(&self.pool)
                .await
                .map_err(StoreError::from_sqlx)?;
        }
        Ok(())
    }

    /// Make every later write of a GBP rate fail, so an answer can't be stored
    /// though its attempt can.
    ///
    /// # Errors
    ///
    /// Fails if no connection is available or the trigger cannot be created.
    pub async fn inject_fx_rate_fault_for_tests(&self) -> Result<(), StoreError> {
        sqlx::query(
            "CREATE TRIGGER injected_fx_rate_fault BEFORE INSERT ON fx_rates \
             WHEN NEW.quote = 'GBP' BEGIN SELECT RAISE(ABORT, 'injected fault'); END",
        )
        .execute(&self.pool)
        .await
        .map_err(StoreError::from_sqlx)?;
        Ok(())
    }
}

impl StoreTx {
    /// Record that `currency`'s rates are being asked for, before asking, so a
    /// store that can't write means no request and a crash during one still
    /// counts as a try. What is known stays. A currency never tried reads as
    /// `failing`, with no code, until the answer settles it: if none comes, it
    /// is tried again within the hour.
    ///
    /// # Errors
    ///
    /// Backend failures.
    pub async fn record_fx_attempt(
        &mut self,
        currency: Currency,
        now: Timestamp,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO fx_coverage (currency, status, last_attempt_at) \
             VALUES (?, 'failing', ?) \
             ON CONFLICT (currency) DO UPDATE SET last_attempt_at = excluded.last_attempt_at",
        )
        .bind(currency.as_str())
        .bind(now.as_millisecond())
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        Ok(())
    }

    /// Store `rates` of `currency`, writing only those that are new or differ
    /// from what is stored, and mark the currency `ready` when it has any
    /// rate; with none stored, even after an empty answer, it is
    /// `not_published`. The daily rows it moves are rewritten in this transaction.
    /// Answers the earliest day that changed, or `None` when nothing did.
    ///
    /// # Errors
    ///
    /// Backend failures, including the schema refusing the euro.
    pub async fn upsert_fx_rates(
        &mut self,
        currency: Currency,
        source: FxSource,
        rates: &[RateObservation],
        now: Timestamp,
    ) -> Result<Option<Date>, StoreError> {
        let written = self.write_rates(currency, source, rates, now).await?;
        self.settle_rates(currency, written, now).await?;
        Ok(written)
    }

    /// As [`StoreTx::upsert_fx_rates`], for an answer about the days from `from`
    /// on: also removes the stored rates of the days the answer speaks for and
    /// doesn't hold, such as a false rate on a day with none.
    ///
    /// An answer speaks for the days from `from` to its own last row. Rates after
    /// that stay, since an answer cut off at the end of a line looks just like a
    /// shorter one; the next answer to pass them settles them. So an empty answer
    /// removes nothing: it can't be the whole truth about a window holding a rate
    /// the source published, and it must not cost every rate there. With a rate
    /// stored it says nothing of the source's health either, so only the time of
    /// the attempt moves. Since a non-empty answer leaves its own rows behind,
    /// this can't empty a currency.
    ///
    /// # Errors
    ///
    /// Backend failures, including the schema refusing the euro.
    pub async fn replace_fx_rates(
        &mut self,
        currency: Currency,
        source: FxSource,
        from: Date,
        rates: &[RateObservation],
        now: Timestamp,
    ) -> Result<Option<Date>, StoreError> {
        if rates.is_empty() && self.holds_a_rate(currency).await? {
            self.record_fx_attempt(currency, now).await?;
            return Ok(None);
        }
        let removed = self.remove_absent_rates(currency, from, rates).await?;
        let written = self.write_rates(currency, source, rates, now).await?;
        let earliest = earlier(removed, written);
        self.settle_rates(currency, earliest, now).await?;
        Ok(earliest)
    }

    async fn holds_a_rate(&mut self, currency: Currency) -> Result<bool, StoreError> {
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM fx_rates WHERE quote = ?)")
            .bind(currency.as_str())
            .fetch_one(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)
    }

    /// Removes `currency`'s rates from `from` to the last of `rates` on days
    /// `rates` doesn't hold. Answers the earliest it removed.
    async fn remove_absent_rates(
        &mut self,
        currency: Currency,
        from: Date,
        rates: &[RateObservation],
    ) -> Result<Option<Date>, StoreError> {
        let Some(last) = rates.iter().map(|rate| rate.on).max() else {
            return Ok(None);
        };
        let answered: BTreeSet<Date> = rates.iter().map(|rate| rate.on).collect();
        let stored = sqlx::query(
            "SELECT on_date FROM fx_rates WHERE quote = ? AND on_date BETWEEN ? AND ? \
             ORDER BY on_date",
        )
        .bind(currency.as_str())
        .bind(from.to_string())
        .bind(last.to_string())
        .fetch_all(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        let mut earliest = None;
        for row in &stored {
            let day: Date = parsed(row, "on_date")?;
            if answered.contains(&day) {
                continue;
            }
            sqlx::query("DELETE FROM fx_rates WHERE quote = ? AND on_date = ?")
                .bind(currency.as_str())
                .bind(day.to_string())
                .execute(&mut *self.tx)
                .await
                .map_err(StoreError::from_sqlx)?;
            earliest = earliest.or(Some(day));
        }
        Ok(earliest)
    }

    /// Writes those of `rates` that are new or differ from what is stored.
    /// Answers the earliest day that changed.
    async fn write_rates(
        &mut self,
        currency: Currency,
        source: FxSource,
        rates: &[RateObservation],
        now: Timestamp,
    ) -> Result<Option<Date>, StoreError> {
        let mut earliest: Option<Date> = None;
        for chunk in rates.chunks(UPSERT_CHUNK) {
            let mut query = QueryBuilder::<Sqlite>::new(
                "INSERT INTO fx_rates (quote, on_date, rate, source, fetched_at) ",
            );
            query.push_values(chunk, |mut row, observation| {
                row.push_bind(currency.as_str())
                    .push_bind(observation.on.to_string())
                    .push_bind(observation.rate.to_string())
                    .push_bind(source.as_str())
                    .push_bind(now.as_millisecond());
            });
            query.push(
                " ON CONFLICT (quote, on_date) DO UPDATE SET rate = excluded.rate, \
                 source = excluded.source, fetched_at = excluded.fetched_at \
                 WHERE fx_rates.rate <> excluded.rate RETURNING on_date",
            );
            for row in query
                .build()
                .fetch_all(&mut *self.tx)
                .await
                .map_err(StoreError::from_sqlx)?
            {
                let day: Date = parsed(&row, "on_date")?;
                earliest = earlier(earliest, Some(day));
            }
        }
        Ok(earliest)
    }

    /// After a change to `currency`'s rates: brings its coverage up to what is
    /// stored and rewrites the daily rows from `changed` on.
    async fn settle_rates(
        &mut self,
        currency: Currency,
        changed: Option<Date>,
        now: Timestamp,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO fx_coverage (currency, status, first_on, last_on, last_attempt_at, \
             last_success_at, last_error) \
             SELECT ?1, IIF(COUNT(*) > 0, 'ready', 'not_published'), MIN(on_date), \
             MAX(on_date), ?2, IIF(COUNT(*) > 0, ?2, NULL), NULL \
             FROM fx_rates WHERE quote = ?1 \
             ON CONFLICT (currency) DO UPDATE SET status = excluded.status, \
             first_on = excluded.first_on, last_on = excluded.last_on, \
             last_attempt_at = excluded.last_attempt_at, \
             last_success_at = excluded.last_success_at, last_error = NULL",
        )
        .bind(currency.as_str())
        .bind(now.as_millisecond())
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        if let Some(changed) = changed {
            self.refresh_after_rate(currency, changed).await?;
        }
        Ok(())
    }

    /// Record that fetching `currency`'s rates did not give rates. What is
    /// known of its coverage (its first and last day, its last success) stays.
    ///
    /// # Errors
    ///
    /// Backend failures.
    pub async fn record_fx_failure(
        &mut self,
        currency: Currency,
        status: FxFetchStatus,
        error: Option<FxErrorCode>,
        now: Timestamp,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO fx_coverage (currency, status, last_attempt_at, last_error) \
             VALUES (?, ?, ?, ?) \
             ON CONFLICT (currency) DO UPDATE SET status = excluded.status, \
             last_attempt_at = excluded.last_attempt_at, last_error = excluded.last_error",
        )
        .bind(currency.as_str())
        .bind(status.as_str())
        .bind(now.as_millisecond())
        .bind(error.map(FxErrorCode::as_str))
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
