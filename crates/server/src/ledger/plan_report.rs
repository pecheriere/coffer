//! Plan versus actual (spec §6.9, §8.9 R2, R6, R21): what the store holds, turned into
//! the input of the domain's calculation.
//!
//! It needs `all` at `transactions` before anything is read, like the forecast, so there
//! is no partial answer. It writes nothing.

use std::collections::BTreeMap;

use coffer_domain::identity::Actor;
use coffer_domain::ids::PlannedItemId;
use coffer_domain::plan::{CalendarMonth, validate_as_of};
use coffer_domain::plan_report::{
    GoalMonth, PlanReport, PlanReportInput, ReportItem, plan_vs_actual, report_window,
};
use coffer_store::{PlanReportData, StoreError};
use jiff::civil::Date;
use serde::Deserialize;
use utoipa::IntoParams;

use super::settlements::settled_months;
use super::{Ledger, LedgerError, plan_items};

const DEFAULT_MONTHS: u8 = 3;
const MAX_MONTHS: u8 = 36;
const DEFAULT_AVERAGE_MONTHS: u8 = 3;
const MAX_AVERAGE_MONTHS: u8 = 12;

const MONTHS_OUT_OF_RANGE: &str = "months must be 1 to 36";
const AVERAGE_MONTHS_OUT_OF_RANGE: &str = "average_months must be 1 to 12";
const TOO_LARGE: &str = "the report's totals are too large to represent";

/// Which months a report covers.
#[derive(Debug, Clone, Copy, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct PlanReportQuery {
    /// The first month, written `YYYY-MM`, in the years 1970 to 2200.
    pub from: CalendarMonth,
    /// How many months to report, by default 3.
    #[param(minimum = 1, maximum = 36)]
    pub months: Option<u8>,
    /// How many calendar months each rolling average covers, ending at its row's month,
    /// by default 3.
    #[param(minimum = 1, maximum = 12)]
    pub average_months: Option<u8>,
}

impl Ledger {
    /// The report `query` asks for.
    ///
    /// # Errors
    ///
    /// [`LedgerError::Denied`] without the forecast's access; [`LedgerError::Invalid`] for
    /// a query out of range or totals too large to represent; storage failures.
    pub async fn plan_vs_actual(
        &self,
        actor: &Actor,
        query: PlanReportQuery,
    ) -> Result<PlanReport, LedgerError> {
        self.authorize_forecast(actor).await?;
        validate_as_of(query.from.first_day()).map_err(plan_items::invalid)?;
        let months = query.months.unwrap_or(DEFAULT_MONTHS);
        if !(1..=MAX_MONTHS).contains(&months) {
            return Err(LedgerError::Invalid(MONTHS_OUT_OF_RANGE));
        }
        let average_months = query.average_months.unwrap_or(DEFAULT_AVERAGE_MONTHS);
        if !(1..=MAX_AVERAGE_MONTHS).contains(&average_months) {
            return Err(LedgerError::Invalid(AVERAGE_MONTHS_OUT_OF_RANGE));
        }
        let today = self.today().await?;
        let (start, end_exclusive) = report_window(query.from, months, average_months)
            .map_err(|_| LedgerError::Invalid(TOO_LARGE))?;
        let data = self
            .store
            .plan_report_data(start, end_exclusive)
            .await
            .map_err(|err| match err {
                StoreError::SumTooLarge => LedgerError::Invalid(TOO_LARGE),
                other => other.into(),
            })?;
        plan_vs_actual(&input(query.from, months, average_months, today, data))
            .map_err(|_| LedgerError::Invalid(TOO_LARGE))
    }
}

/// The calculation's input. An inactive item is in it too: it plans nothing, but a goal's
/// linked splits count nowhere else, so its line has to stay.
fn input(
    from: CalendarMonth,
    months: u8,
    average_months: u8,
    today: Date,
    data: PlanReportData,
) -> PlanReportInput {
    let mut goal_spent: BTreeMap<PlannedItemId, Vec<GoalMonth>> = BTreeMap::new();
    for (goal, month) in data.goal_spent {
        goal_spent.entry(goal).or_default().push(month);
    }
    let mut settled = settled_months(&data.settlements);
    PlanReportInput {
        from,
        months,
        average_months,
        today,
        items: data
            .items
            .into_iter()
            .map(|item| ReportItem {
                goal_spent: goal_spent.remove(&item.id).unwrap_or_default(),
                settled: settled.remove(&item.id).unwrap_or_default(),
                id: item.id,
                name: item.name.as_str().to_owned(),
                versions: item.versions,
                schedule: item.schedule,
                active: item.active,
            })
            .collect(),
        actuals: data.actuals,
        uncategorized: data.uncategorized,
        not_household: data.not_household,
        moved: data.moved,
        cross_currency_links: data.cross_currency_links,
        rates: data.rates,
    }
}
