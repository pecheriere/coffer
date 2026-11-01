//! Daily balance snapshots: one per account per day, the last write wins.

use coffer_domain::accounts::BalanceSource;
use coffer_domain::ids::AccountId;
use jiff::Timestamp;
use jiff::civil::Date;
use sqlx::sqlite::SqliteRow;
use sqlx::{QueryBuilder, Row, Sqlite};

use crate::convert::{parsed, time};
use crate::{Page, PageRequest, Store, StoreError, StoreTx};

/// An account's balance at the end of one calendar day, in the account's
/// currency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BalanceSnapshot {
    pub account: AccountId,
    pub on: Date,
    pub balance_minor: i64,
    pub source: BalanceSource,
    /// When this value was written.
    pub recorded_at: Timestamp,
}

macro_rules! columns {
    () => {
        "account_id, on_date, balance_minor, source, recorded_at"
    };
}

fn snapshot_from_row(row: &SqliteRow) -> Result<BalanceSnapshot, StoreError> {
    Ok(BalanceSnapshot {
        account: parsed(row, "account_id")?,
        on: parsed(row, "on_date")?,
        balance_minor: row
            .try_get("balance_minor")
            .map_err(StoreError::from_sqlx)?,
        source: parsed(row, "source")?,
        recorded_at: time(row, "recorded_at")?,
    })
}

impl StoreTx {
    /// Record `snapshot`, replacing any snapshot of the same account and day,
    /// and return the one it replaced.
    ///
    /// # Errors
    ///
    /// Backend failures, including an account that does not exist.
    pub async fn upsert_balance(
        &mut self,
        snapshot: BalanceSnapshot,
    ) -> Result<Option<BalanceSnapshot>, StoreError> {
        let account = snapshot.account.to_string();
        let on = snapshot.on.to_string();
        let previous = sqlx::query(concat!(
            "SELECT ",
            columns!(),
            " FROM balance_snapshots WHERE account_id = ? AND on_date = ?"
        ))
        .bind(&account)
        .bind(&on)
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?
        .as_ref()
        .map(snapshot_from_row)
        .transpose()?;
        sqlx::query(
            "INSERT INTO balance_snapshots \
             (account_id, on_date, balance_minor, source, recorded_at) VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT (account_id, on_date) DO UPDATE SET \
             balance_minor = excluded.balance_minor, source = excluded.source, \
             recorded_at = excluded.recorded_at",
        )
        .bind(&account)
        .bind(&on)
        .bind(snapshot.balance_minor)
        .bind(snapshot.source.as_str())
        .bind(snapshot.recorded_at.as_millisecond())
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        Ok(previous)
    }
}

impl Store {
    /// An account's snapshots, newest day first. `from` is inclusive and
    /// `to` exclusive. The returned cursor continues after the last item of
    /// the page.
    ///
    /// # Errors
    ///
    /// [`StoreError::InvalidCursor`] if `page.after` is not a cursor issued by
    /// this method; otherwise a backend error.
    pub async fn list_balances(
        &self,
        account: AccountId,
        from: Option<Date>,
        to: Option<Date>,
        page: &PageRequest,
    ) -> Result<Page<BalanceSnapshot>, StoreError> {
        let limit = page.effective_limit();
        let after = page
            .after
            .as_deref()
            .map(|cursor| {
                cursor
                    .parse::<Date>()
                    .map_err(|_| StoreError::InvalidCursor)
            })
            .transpose()?;
        let mut query = QueryBuilder::<Sqlite>::new(concat!(
            "SELECT ",
            columns!(),
            " FROM balance_snapshots WHERE account_id = "
        ));
        query.push_bind(account.to_string());
        if let Some(from) = from {
            query.push(" AND on_date >= ").push_bind(from.to_string());
        }
        if let Some(to) = to {
            query.push(" AND on_date < ").push_bind(to.to_string());
        }
        if let Some(after) = after {
            query.push(" AND on_date < ").push_bind(after.to_string());
        }
        // One extra row tells whether another page exists.
        query
            .push(" ORDER BY on_date DESC LIMIT ")
            .push_bind(i64::from(limit) + 1);
        let rows = query
            .build()
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::from_sqlx)?;
        let mut items = rows
            .iter()
            .map(snapshot_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        let next = if items.len() > limit as usize {
            items.truncate(limit as usize);
            items.last().map(|snapshot| snapshot.on.to_string())
        } else {
            None
        };
        Ok(Page { items, next })
    }

    /// An account's most recent snapshot, if any.
    ///
    /// # Errors
    ///
    /// Backend failures, or a row that no longer parses.
    pub async fn latest_balance(
        &self,
        account: AccountId,
    ) -> Result<Option<BalanceSnapshot>, StoreError> {
        sqlx::query(concat!(
            "SELECT ",
            columns!(),
            " FROM balance_snapshots WHERE account_id = ? ORDER BY on_date DESC LIMIT 1"
        ))
        .bind(account.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::from_sqlx)?
        .as_ref()
        .map(snapshot_from_row)
        .transpose()
    }
}
