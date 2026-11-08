//! The emergency fund (money redesign §4.4, §10.4): one target per currency, a fixed amount
//! in it or some months of its recurring outflows. Setting or removing one is an admin's,
//! audited with the old and new canonical text. Reading one takes the forecast's access, as
//! [`super::settings`] says, so both answer the settings as that actor may see them.

use coffer_domain::audit::AuditChanges;
use coffer_domain::identity::Actor;
use coffer_domain::money::Currency;
use coffer_domain::plan::EmergencyFundTarget;
use coffer_domain::policy::{AccountLevel, Action, authorize, authorize_every_account};

use super::settings::{HouseholdSettings, invalid};
use super::{Ledger, LedgerError, audit};

/// The audit entity of every emergency fund change, the household's settings.
const ENTITY: &str = "household";

/// The audit field of `currency`'s target.
fn field(currency: Currency) -> String {
    format!("emergency_fund.{currency}")
}

impl Ledger {
    /// Sets `currency`'s emergency fund target, replacing any it had. The same target again
    /// writes nothing, audit included. A currency the household doesn't use may have one.
    ///
    /// # Errors
    ///
    /// [`LedgerError::Denied`] without [`Action::ManageHousehold`]; [`LedgerError::Invalid`]
    /// for a target out of range; a failure to resolve access; storage failures.
    pub async fn set_emergency_fund(
        &self,
        actor: &Actor,
        currency: Currency,
        target: EmergencyFundTarget,
    ) -> Result<HouseholdSettings, LedgerError> {
        authorize(actor, Action::ManageHousehold)?;
        target.validate().map_err(invalid)?;
        let access = self.access.resolve(actor).await?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let current = tx.emergency_funds().await?.get(&currency).copied();
        if current == Some(target) {
            return self.household_settings_in(&mut tx, actor, &access).await;
        }
        tx.put_emergency_fund(currency, &target).await?;
        let changes = AuditChanges::new().change(
            &field(currency),
            current.map(|old| old.canonical()),
            target.canonical(),
        );
        audit(
            &mut tx,
            now,
            actor,
            "household.emergency_fund.set",
            ENTITY,
            ENTITY,
            changes,
        )
        .await?;
        let answer = self.household_settings_in(&mut tx, actor, &access).await?;
        tx.commit().await?;
        Ok(answer)
    }

    /// Removes `currency`'s emergency fund target. Whether one is set is what a token below
    /// the forecast's access may not learn, and a missing one is not found, so removing
    /// takes that access too, decided before anything is read: the refusal is the same
    /// whatever is set.
    ///
    /// # Errors
    ///
    /// [`LedgerError::Denied`] without [`Action::ManageHousehold`] or `transactions` access
    /// to every account; [`LedgerError::NotFound`] when it has none, and nothing is
    /// written; a failure to resolve access; storage failures.
    pub async fn remove_emergency_fund(
        &self,
        actor: &Actor,
        currency: Currency,
    ) -> Result<HouseholdSettings, LedgerError> {
        authorize(actor, Action::ManageHousehold)?;
        let access = self.access.resolve(actor).await?;
        authorize_every_account(actor, &access, AccountLevel::Transactions)?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let Some(current) = tx.emergency_funds().await?.get(&currency).copied() else {
            return Err(LedgerError::NotFound);
        };
        tx.delete_emergency_fund(currency).await?;
        let changes = AuditChanges::new().unset(&field(currency), current.canonical());
        audit(
            &mut tx,
            now,
            actor,
            "household.emergency_fund.remove",
            ENTITY,
            ENTITY,
            changes,
        )
        .await?;
        let answer = self.household_settings_in(&mut tx, actor, &access).await?;
        tx.commit().await?;
        Ok(answer)
    }
}
