//! Rates an admin entered (spec §8.10 N16). They are kept apart from `fx_rates`, so a fetch
//! can't touch one and a published rate under one comes back when it is removed. Which of
//! the two applies on a day is the domain's [`RateHistory::merged`] alone.
//!
//! Unlike a fetched rate, entering or removing one is audited, in the transaction that does
//! it, by the caller.
//!
//! [`RateHistory::merged`]: coffer_domain::fx::RateHistory::merged

use coffer_domain::fx::{FxRate, ManualCoverage, ManualRate};
use coffer_domain::money::Currency;
use jiff::Timestamp;
use jiff::civil::Date;
use sqlx::SqliteConnection;
use sqlx::sqlite::SqliteRow;

use crate::convert::{parsed, text, time};
use crate::{Store, StoreError, StoreTx};

/// The most rates one `Store::manual_rates` call returns.
const MAX_MANUAL_RATES_PER_READ: i64 = 1_000;

fn currency_from_row(row: &SqliteRow, column: &str) -> Result<Currency, StoreError> {
    Currency::parse(&text(row, column)?).map_err(StoreError::backend)
}

fn manual_rate_from_row(row: &SqliteRow) -> Result<ManualRate, StoreError> {
    Ok(ManualRate {
        currency: currency_from_row(row, "quote")?,
        on: parsed(row, "on_date")?,
        rate: parsed::<FxRate>(row, "rate")?,
        entered_at: time(row, "entered_at")?,
    })
}

/// Those of `currencies` holding a manual rate.
pub(crate) async fn currencies_with_manual_rates(
    conn: &mut SqliteConnection,
    currencies: &[Currency],
) -> Result<Vec<Currency>, StoreError> {
    let mut found = Vec::new();
    for &currency in currencies {
        let holds: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM fx_manual_rates WHERE quote = ?)")
                .bind(currency.as_str())
                .fetch_one(&mut *conn)
                .await
                .map_err(StoreError::from_sqlx)?;
        if holds {
            found.push(currency);
        }
    }
    Ok(found)
}

impl Store {
    /// Every rate an admin entered, the newest day first, then by currency: at most 1,000,
    /// the newest of a longer list.
    ///
    /// Which currencies have one can reveal the currencies accounts hold: a caller must
    /// not return it to anyone below `all` accounts at `balances`.
    ///
    /// # Errors
    ///
    /// Backend failures, including a stored row that no longer parses.
    pub async fn manual_rates(&self) -> Result<Vec<ManualRate>, StoreError> {
        sqlx::query("SELECT * FROM fx_manual_rates ORDER BY on_date DESC, quote LIMIT ?")
            .bind(MAX_MANUAL_RATES_PER_READ)
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::from_sqlx)?
            .iter()
            .map(manual_rate_from_row)
            .collect()
    }

    /// Which days each currency has a rate an admin entered, by currency.
    ///
    /// As [`Store::manual_rates`], a caller must not return it to anyone below `all`
    /// accounts at `balances`.
    ///
    /// # Errors
    ///
    /// Backend failures, including a stored row that no longer parses.
    pub async fn manual_coverage(&self) -> Result<Vec<ManualCoverage>, StoreError> {
        sqlx::query(
            "SELECT quote, MIN(on_date) AS first_on, MAX(on_date) AS last_on \
             FROM fx_manual_rates GROUP BY quote ORDER BY quote",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::from_sqlx)?
        .iter()
        .map(|row| {
            Ok(ManualCoverage {
                currency: currency_from_row(row, "quote")?,
                first_on: parsed(row, "first_on")?,
                last_on: parsed(row, "last_on")?,
            })
        })
        .collect()
    }
}

impl StoreTx {
    /// The rate an admin entered for `currency` on `on`, if any.
    ///
    /// # Errors
    ///
    /// Backend failures, including a stored row that no longer parses.
    pub async fn manual_rate(
        &mut self,
        currency: Currency,
        on: Date,
    ) -> Result<Option<ManualRate>, StoreError> {
        sqlx::query("SELECT * FROM fx_manual_rates WHERE quote = ? AND on_date = ?")
            .bind(currency.as_str())
            .bind(on.to_string())
            .fetch_optional(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?
            .as_ref()
            .map(manual_rate_from_row)
            .transpose()
    }

    /// Stores `rate` as what an admin entered for `currency` on `on`, replacing any
    /// they entered before. The daily rows it moves are rewritten in this transaction,
    /// or if there are too many, asks for a rebuild.
    ///
    /// # Errors
    ///
    /// Backend failures, including the schema refusing the euro.
    pub async fn set_manual_rate(
        &mut self,
        currency: Currency,
        on: Date,
        rate: FxRate,
        now: Timestamp,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO fx_manual_rates (quote, on_date, rate, entered_at) VALUES (?, ?, ?, ?) \
             ON CONFLICT (quote, on_date) DO UPDATE SET rate = excluded.rate, \
             entered_at = excluded.entered_at",
        )
        .bind(currency.as_str())
        .bind(on.to_string())
        .bind(rate.to_string())
        .bind(now.as_millisecond())
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        self.refresh_after_rate(currency, on).await
    }

    /// Removes the rate an admin entered for `currency` on `on`, so what it covered falls
    /// back on the published rate of that day or an earlier rate. The daily rows move as
    /// with [`StoreTx::set_manual_rate`].
    ///
    /// # Errors
    ///
    /// Backend failures.
    pub async fn remove_manual_rate(
        &mut self,
        currency: Currency,
        on: Date,
    ) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM fx_manual_rates WHERE quote = ? AND on_date = ?")
            .bind(currency.as_str())
            .bind(on.to_string())
            .execute(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;
        self.refresh_after_rate(currency, on).await
    }
}
