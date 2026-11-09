//! Free money (money redesign §4.3, §4.4, §10.1): the cash nobody has claimed yet, from the same
//! read and the same input as the forecast, so the API, MCP and pages agree with it.
//!
//! It sums every account, so it needs the forecast's access, decided before anything is read: a
//! token that can't see an account gets no answer at all rather than one that leaves it out. It
//! writes nothing.

use coffer_domain::forecast::{FreeMoney, compute_free_money};
use coffer_domain::identity::Actor;
use coffer_domain::plan::{ForecastSettings, validate_as_of};

use super::forecast::{TOO_LARGE, input};
use super::{Ledger, LedgerError, plan_items};

impl Ledger {
    /// Free money at the household's today.
    ///
    /// # Errors
    ///
    /// [`LedgerError::Denied`] without [`coffer_domain::policy::Action::ReadHousehold`] and
    /// `transactions` access to every account; [`LedgerError::Invalid`] for totals too large to
    /// represent; storage failures.
    pub async fn free_money(&self, actor: &Actor) -> Result<FreeMoney, LedgerError> {
        self.authorize_forecast(actor).await?;
        let today = self.today().await?;
        validate_as_of(today).map_err(plan_items::invalid)?;
        let mut data = self.forecast_data(today).await?;
        // Free money looks at its own window and never counts income, whatever these say.
        let settings = ForecastSettings {
            include_income: false,
            default_horizon_months: 1,
            stale_after_days: data.settings.stale_after_days,
        };
        let funds = std::mem::take(&mut data.emergency_funds);
        compute_free_money(&input(today, today, settings, funds, data, Vec::new()))
            .map_err(|_| LedgerError::Invalid(TOO_LARGE))
    }
}
