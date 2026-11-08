//! What free money answers (money redesign §4.3, §4.4, §10.1).

use jiff::civil::Date;
use serde::Serialize;

use crate::forecast::{Blocker, Caution, ForecastStatus};
use crate::money::{Currency, Money};
use crate::plan::EmergencyFundTarget;

/// How much cash is free today in each currency: what nothing in the plan, no goal and no
/// emergency fund has claimed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct FreeMoney {
    /// The household's day it is worked out at.
    pub as_of: Date,
    /// The last day a must pay occurrence still counts on: `as_of` plus 30 days.
    pub must_pay_through: Date,
    /// `unavailable` when no currency has a number, `provisional` with any blocker or caution,
    /// `firm` otherwise.
    pub status: ForecastStatus,
    /// One per currency in use, sorted by currency. Never added together.
    pub currencies: Vec<FreeMoneyCurrency>,
    /// What leaves a currency, or every one, without a number.
    pub blockers: Vec<Blocker>,
    /// What keeps the numbers but stops them being firm.
    pub cautions: Vec<Caution>,
}

/// One currency's free money and how it is reached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct FreeMoneyCurrency {
    pub currency: Currency,
    /// What the forecast counts as cash: the household's cash balances, less short-term debts.
    /// Investments never count.
    pub cash_available: Money,
    pub still_need: StillNeed,
    /// What goals have set aside by today, less what was spent from them.
    pub set_aside: Money,
    /// The target as set; null when none is set for this currency.
    #[cfg_attr(feature = "openapi", schema(required))]
    pub emergency_fund: Option<EmergencyFundTarget>,
    /// The target in money: 0 when none is set.
    pub emergency_fund_target: Money,
    /// How much of the target the cash covers now, from 0 to the target. Null when a blocker
    /// leaves this currency without a number.
    #[cfg_attr(feature = "openapi", schema(required))]
    pub emergency_fund_held: Option<Money>,
    /// What is left once the plan, the goals and the emergency fund are covered, never below 0.
    /// Null when a blocker leaves this currency without a number.
    #[cfg_attr(feature = "openapi", schema(required))]
    pub free_money: Option<Money>,
    /// How far the cash falls short of what the plan still needs and goals have set aside; 0
    /// when it doesn't. Null when a blocker leaves this currency without a number.
    #[cfg_attr(feature = "openapi", schema(required))]
    pub shortfall: Option<Money>,
}

/// What the plan's outflows still need, by tier: what each item plans less what was spent on
/// it, never below 0, so one overspent item takes nothing from another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct StillNeed {
    /// Must pay occurrences in this calendar month not yet paid, from before today too.
    pub must_pay_this_month: Money,
    /// Must pay occurrences after this month, through `must_pay_through`, not yet paid.
    pub must_pay_coming: Money,
    /// What this month's essentials still need.
    pub essentials: Money,
    /// What this month's flexible items still need.
    pub flexible: Money,
    /// The four above added up.
    pub total: Money,
}
