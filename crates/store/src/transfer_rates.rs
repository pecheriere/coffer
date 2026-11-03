//! The links across currencies, with the rates to compare their legs at (spec §8.12 T8).
//! Nothing here changes what a link moves: it only gathers what the domain's check reads.

use std::collections::BTreeSet;

use coffer_domain::fx::RateBook;
use coffer_domain::money::{Currency, Money};
use coffer_domain::transfer_rate::CrossCurrencyLink;
use jiff::civil::Date;
use sqlx::SqliteConnection;
use sqlx::sqlite::SqliteRow;

use crate::convert::{int, parsed, text};
use crate::fx::load_rate_book;
use crate::transfer_links::{
    both_counted, both_counted_when_posted, counted_cash, counted_when_posted, link_legs,
    link_stands,
};
use crate::{Store, StoreError, StoreTx};

/// The links still standing between accounts that are `$counted`, in two currencies, with a
/// leg posted from `?1` and `$until` `?2`, and at least one leg posted: pending amounts
/// are in no count, so there is nothing to caution about.
macro_rules! cross_currency_links {
    ($counted:ident, $until:literal) => {
        concat!(
            "SELECT lk.id AS link, lo.amount_minor AS out_minor, lo.currency AS out_currency, \
             lo.posted_on AS out_on, li.amount_minor AS in_minor, li.currency AS in_currency, \
             li.posted_on AS in_on FROM ",
            link_legs!(),
            " WHERE lo.currency <> li.currency AND ",
            link_stands!(),
            " AND ",
            $counted!(),
            " AND (lo.pending = 0 OR li.pending = 0) AND ((lo.posted_on >= ?1 AND lo.posted_on ",
            $until,
            " ?2) OR (li.posted_on >= ?1 AND li.posted_on ",
            $until,
            " ?2)) ORDER BY lk.id"
        )
    };
}

/// For a forecast: both accounts counted now, and a leg from the month's start up to and
/// including `as_of`.
const FOR_FORECAST: &str = cross_currency_links!(both_counted, "<=");

/// For a report: both accounts counted when their legs posted, and a leg in `[?1, ?2)`.
const FOR_REPORT: &str = cross_currency_links!(both_counted_when_posted, "<");

fn link_from_row(row: &SqliteRow) -> Result<CrossCurrencyLink, StoreError> {
    let money = |amount: &str, currency: &str| -> Result<Money, StoreError> {
        Ok(Money {
            minor_units: int(row, amount)?,
            currency: Currency::parse(&text(row, currency)?).map_err(StoreError::backend)?,
        })
    };
    Ok(CrossCurrencyLink {
        link: parsed(row, "link")?,
        outflow: money("out_minor", "out_currency")?,
        inflow: money("in_minor", "in_currency")?,
        outflow_on: parsed(row, "out_on")?,
        inflow_on: parsed(row, "in_on")?,
    })
}

async fn gather(
    conn: &mut SqliteConnection,
    query: &'static str,
    from: Date,
    until: Date,
) -> Result<(Vec<CrossCurrencyLink>, RateBook), StoreError> {
    let links: Vec<CrossCurrencyLink> = sqlx::query(query)
        .bind(from.to_string())
        .bind(until.to_string())
        .fetch_all(&mut *conn)
        .await
        .map_err(StoreError::from_sqlx)?
        .iter()
        .map(link_from_row)
        .collect::<Result<_, _>>()?;
    let days = links.iter().map(CrossCurrencyLink::rate_day);
    let (Some(first), Some(last)) = (days.clone().min(), days.max()) else {
        return Ok((links, RateBook::default()));
    };
    let currencies: Vec<Currency> = links
        .iter()
        .flat_map(|link| [link.outflow.currency, link.inflow.currency])
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let rates = load_rate_book(conn, &currencies, first, last).await?;
    Ok((links, rates))
}

/// The links between counted accounts in two currencies with a leg from `month_start` to
/// `as_of`, with their rates.
pub(crate) async fn for_forecast(
    conn: &mut SqliteConnection,
    month_start: Date,
    as_of: Date,
) -> Result<(Vec<CrossCurrencyLink>, RateBook), StoreError> {
    gather(conn, FOR_FORECAST, month_start, as_of).await
}

/// The links that were between counted accounts in two currencies with a leg in
/// `[from, to_exclusive)`, with their rates.
pub(crate) async fn for_report(
    conn: &mut SqliteConnection,
    from: Date,
    to_exclusive: Date,
) -> Result<(Vec<CrossCurrencyLink>, RateBook), StoreError> {
    gather(conn, FOR_REPORT, from, to_exclusive).await
}

impl Store {
    /// The rates of `currencies` for `on`, carried forward from an earlier day as a
    /// calculation would.
    ///
    /// # Errors
    ///
    /// Backend failures, including a stored rate that no longer parses.
    pub async fn rates_on(
        &self,
        currencies: &[Currency],
        on: Date,
    ) -> Result<RateBook, StoreError> {
        let mut conn = self.pool.acquire().await.map_err(StoreError::from_sqlx)?;
        load_rate_book(&mut conn, currencies, on, on).await
    }
}

impl StoreTx {
    /// As [`Store::rates_on`], as this transaction sees them.
    ///
    /// # Errors
    ///
    /// Backend failures, including a stored rate that no longer parses.
    pub async fn rates_on(
        &mut self,
        currencies: &[Currency],
        on: Date,
    ) -> Result<RateBook, StoreError> {
        load_rate_book(&mut self.tx, currencies, on, on).await
    }
}
