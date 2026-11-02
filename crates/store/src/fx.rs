//! Exchange rates and their coverage (spec §8.10). A rate is stored as the
//! canonical text of an `FxRate`, so two texts are equal exactly when the
//! rates are, and the euro has no row.

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

impl StoreTx {
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
                earliest = Some(earliest.map_or(day, |known| known.min(day)));
            }
        }
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
        if let Some(earliest) = earliest {
            self.refresh_after_rate(currency, earliest).await?;
        }
        Ok(earliest)
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
