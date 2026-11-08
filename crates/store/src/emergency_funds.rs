//! The emergency fund: at most one target per currency (money redesign §4.4, §10.4).

use coffer_domain::money::Currency;
use coffer_domain::plan::{EmergencyFundTarget, EmergencyFunds};
use sqlx::sqlite::SqliteRow;
use sqlx::{Row as _, SqliteExecutor};

use crate::convert::{corrupt, text};
use crate::{Store, StoreError, StoreTx};

fn target_from_row(row: &SqliteRow) -> Result<(Currency, EmergencyFundTarget), StoreError> {
    let currency = Currency::parse(&text(row, "currency")?).map_err(StoreError::backend)?;
    let minor: Option<i64> = row.try_get("minor").map_err(StoreError::from_sqlx)?;
    let months: Option<i64> = row.try_get("months").map_err(StoreError::from_sqlx)?;
    let target = match (text(row, "kind")?.as_str(), minor, months) {
        ("fixed", Some(minor_units), None) => EmergencyFundTarget::Fixed { minor_units },
        ("months_of_outflows", None, Some(months)) => EmergencyFundTarget::MonthsOfOutflows {
            months: u8::try_from(months)
                .map_err(|_| corrupt("emergency fund months out of range"))?,
        },
        _ => return Err(corrupt("emergency fund columns do not form a target")),
    };
    target
        .validate()
        .map_err(|_| corrupt("stored emergency fund is invalid"))?;
    Ok((currency, target))
}

pub(crate) async fn fetch_emergency_funds<'e>(
    executor: impl SqliteExecutor<'e>,
) -> Result<EmergencyFunds, StoreError> {
    sqlx::query("SELECT currency, kind, minor, months FROM emergency_funds")
        .fetch_all(executor)
        .await
        .map_err(StoreError::from_sqlx)?
        .iter()
        .map(target_from_row)
        .collect()
}

impl Store {
    /// Every currency's emergency fund target; a currency missing has none.
    ///
    /// # Errors
    ///
    /// Backend failures, including a stored row that is not a valid target.
    pub async fn emergency_funds(&self) -> Result<EmergencyFunds, StoreError> {
        fetch_emergency_funds(&self.pool).await
    }
}

impl StoreTx {
    /// The emergency fund targets as seen by this transaction; see
    /// [`Store::emergency_funds`].
    ///
    /// # Errors
    ///
    /// Backend failures, including a stored row that is not a valid target.
    pub async fn emergency_funds(&mut self) -> Result<EmergencyFunds, StoreError> {
        fetch_emergency_funds(&mut *self.tx).await
    }

    /// Set `currency`'s target, replacing any it had. The caller validates it; the
    /// schema's CHECKs refuse what is out of range.
    ///
    /// # Errors
    ///
    /// Backend failures.
    pub async fn put_emergency_fund(
        &mut self,
        currency: Currency,
        target: &EmergencyFundTarget,
    ) -> Result<(), StoreError> {
        let (kind, minor, months) = match *target {
            EmergencyFundTarget::Fixed { minor_units } => ("fixed", Some(minor_units), None),
            EmergencyFundTarget::MonthsOfOutflows { months } => {
                ("months_of_outflows", None, Some(i64::from(months)))
            }
        };
        sqlx::query(
            "INSERT INTO emergency_funds (currency, kind, minor, months) VALUES (?, ?, ?, ?) \
             ON CONFLICT (currency) DO UPDATE SET kind = excluded.kind, \
             minor = excluded.minor, months = excluded.months",
        )
        .bind(currency.as_str())
        .bind(kind)
        .bind(minor)
        .bind(months)
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        Ok(())
    }

    /// Remove `currency`'s target; `true` when it had one.
    ///
    /// # Errors
    ///
    /// Backend failures.
    pub async fn delete_emergency_fund(&mut self, currency: Currency) -> Result<bool, StoreError> {
        let deleted = sqlx::query("DELETE FROM emergency_funds WHERE currency = ?")
            .bind(currency.as_str())
            .execute(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;
        Ok(deleted.rows_affected() > 0)
    }
}
