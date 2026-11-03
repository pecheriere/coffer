//! Exchange rates an admin enters by hand (spec §8.10 N16), for a currency no source
//! publishes or a day it got wrong.
//!
//! A wrong rate moves net worth, so every write is validated, audited with its old and new
//! value, and marked in everything built from it. Whether a code converts at once depends on
//! the currencies in use, so a write spans every account as a display currency does (N8).

use coffer_domain::audit::AuditChanges;
use coffer_domain::fx::{FxRate, ManualRate, UNUSUAL_RATE_NEARBY_DAYS, unusual_against};
use coffer_domain::identity::Actor;
use coffer_domain::money::Currency;
use coffer_domain::net_worth::MIN_VALUE_DATE;
use coffer_domain::policy::{AccountLevel, Action, authorize};
use jiff::Span;
use jiff::civil::Date;
use serde::Serialize;
use utoipa::ToSchema;

use super::{Ledger, LedgerError, audit};

/// The audit entity of a manual rate.
const ENTITY: &str = "fx_manual_rate";

const NOT_THE_EURO: &str = "the euro is 1 on every day, so it takes no rate";
const NOT_THE_DISPLAY_CURRENCY: &str = "a rate can't be entered for the display currency";
const NOT_IN_THE_FUTURE: &str = "a rate can't be entered for a day after today";
const NOT_BEFORE_THE_TABLE: &str = "a rate can't be entered for a day before 1970";

/// The rates admins have entered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ManualRateList {
    /// The newest day first, then by currency: at most 1,000 from `offset`.
    pub items: Vec<ManualRate>,
    /// There are older rates than these: ask again with `offset` past them.
    pub truncated: bool,
}

/// An audit entity's id: one currency's one day.
fn entity_id(currency: Currency, on: Date) -> String {
    format!("{}:{on}", currency.as_str())
}

impl Ledger {
    /// The rates admins have entered, skipping the `offset` newest.
    ///
    /// # Errors
    ///
    /// As [`Self::fx_coverage`].
    pub async fn manual_rates(
        &self,
        actor: &Actor,
        offset: u32,
    ) -> Result<ManualRateList, LedgerError> {
        self.authorize_fx(actor).await?;
        let (items, truncated) = self.store.manual_rates(offset).await?;
        Ok(ManualRateList { items, truncated })
    }

    /// Enters `rate` for `currency` on `on`, replacing what an admin entered for that day.
    /// It rewrites the daily values it moves, in this transaction or if there are too many
    /// for one, in a rebuild the daily job runs. The same rate again writes nothing. A rate
    /// over 1.5 times from the published rate nearest its day, within 30 days, or within 2% of
    /// its inverse when that rate is at least 5% away from 1, is refused unless
    /// `confirm_unusual`, and the audit entry of one kept says what it was against.
    /// With no published rate nearby there is nothing to compare it with.
    ///
    /// # Errors
    ///
    /// [`LedgerError::Denied`] without [`Action::ManageHousehold`] and `balances` access to
    /// every account; [`LedgerError::Invalid`] for the euro, the display currency, a day
    /// after today or before 1970; [`LedgerError::UnusualRate`] as above; a failure to
    /// resolve access; storage failures.
    pub async fn set_manual_rate(
        &self,
        actor: &Actor,
        currency: Currency,
        on: Date,
        rate: FxRate,
        confirm_unusual: bool,
    ) -> Result<ManualRate, LedgerError> {
        self.authorize_manual_rate(actor).await?;
        if currency.as_str() == "EUR" {
            return Err(LedgerError::Invalid(NOT_THE_EURO));
        }
        if on < MIN_VALUE_DATE {
            return Err(LedgerError::Invalid(NOT_BEFORE_THE_TABLE));
        }
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        // Under the write lock, so a change of zone can't land between asking and writing.
        if on > self.time.today_seen_by(&mut tx).await? {
            return Err(LedgerError::Invalid(NOT_IN_THE_FUTURE));
        }
        if tx.display_currency().await? == Some(currency) {
            return Err(LedgerError::Invalid(NOT_THE_DISPLAY_CURRENCY));
        }
        let before = tx.manual_rate(currency, on).await?;
        if let Some(same) = before.filter(|before| before.rate == rate) {
            return Ok(same);
        }
        let reach = Span::new().days(UNUSUAL_RATE_NEARBY_DAYS);
        let nearby = tx
            .published_rates_between(currency, on.saturating_sub(reach), on.saturating_add(reach))
            .await?;
        let unusual = unusual_against(rate, on, &nearby);
        if let (Some(unusual), false) = (unusual, confirm_unusual) {
            return Err(LedgerError::UnusualRate {
                entered: rate,
                unusual,
            });
        }
        tx.set_manual_rate(currency, on, rate, now).await?;
        let mut changes = AuditChanges::new().change(
            "rate",
            before.map(|before| before.rate.to_string()),
            rate.to_string(),
        );
        if let Some(unusual) = unusual {
            changes = changes.set(
                "confirmed_against_published",
                unusual.published().rate.to_string(),
            );
        }
        audit(
            &mut tx,
            now,
            actor,
            "fx.manual_rate.set",
            ENTITY,
            entity_id(currency, on),
            changes,
        )
        .await?;
        // Answered as stored, which keeps milliseconds: the list shows the same instant.
        let saved = tx.manual_rate(currency, on).await?;
        tx.commit().await?;
        saved.ok_or(LedgerError::Internal("the saved rate was not found"))
    }

    /// Removes the rate an admin entered for `currency` on `on`: what it covered falls back
    /// on the published rate of that day, an earlier rate or none. The daily values move as
    /// with [`Self::set_manual_rate`].
    ///
    /// # Errors
    ///
    /// [`LedgerError::Denied`] as [`Self::set_manual_rate`]; [`LedgerError::NotFound`] with
    /// no such rate, so a mistyped day doesn't read as done; a failure to resolve access;
    /// storage failures.
    pub async fn remove_manual_rate(
        &self,
        actor: &Actor,
        currency: Currency,
        on: Date,
    ) -> Result<(), LedgerError> {
        self.authorize_manual_rate(actor).await?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let Some(before) = tx.manual_rate(currency, on).await? else {
            return Err(LedgerError::NotFound);
        };
        tx.remove_manual_rate(currency, on).await?;
        audit(
            &mut tx,
            now,
            actor,
            "fx.manual_rate.remove",
            ENTITY,
            entity_id(currency, on),
            AuditChanges::new().change("rate", before.rate.to_string(), None::<String>),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Decided from the actor alone, before anything is read or checked.
    async fn authorize_manual_rate(&self, actor: &Actor) -> Result<(), LedgerError> {
        authorize(actor, Action::ManageHousehold)?;
        self.authorize_every_account_access(actor, AccountLevel::Balances)
            .await
    }
}
