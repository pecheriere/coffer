//! What a forecast takes in and what it answers (spec §6.8, §8.9).
//!
//! Output amounts are [`Money`]. A planned item's amounts are positive
//! magnitudes with a [`PlanDirection`]; balances, `cash_available`,
//! `projected_cash` and `surplus` carry a sign, and a negative `surplus` is a
//! shortfall.

use jiff::civil::Date;
use serde::Serialize;

use crate::accounts::LiquidityClass;
use crate::ids::{AccountId, CategoryId, ConnectionId, PlannedItemId, ProviderAccountId};
use crate::money::{Currency, Money};
use crate::plan::{
    CalendarMonth, PlanDirection, PlanItemType, PlanSchedule, PlanSpending, SafetyFloor,
};

/// Everything one forecast reads, gathered by the caller at one moment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForecastInput {
    /// The household's day.
    pub today: Date,
    /// The first day of the window.
    pub as_of: Date,
    pub horizon_months: u8,
    /// Whether planned inflows reduce the cash needed.
    pub include_income: bool,
    /// `None` until the household sets one, which blocks the number.
    pub floor: Option<SafetyFloor>,
    /// A counted balance older than this many days is flagged.
    pub stale_after_days: u16,
    /// Every account, closed and excluded ones too.
    pub accounts: Vec<AccountInput>,
    /// The active planned items and any hypothetical ones.
    pub items: Vec<ItemInput>,
    /// Actual spending per category in `as_of`'s month, up to `as_of`.
    pub category_spent: Vec<CategorySpent>,
    /// Spending in `as_of`'s month that is not settled yet.
    pub loose_ends: Vec<LooseEnds>,
    /// Provider accounts bound to no account.
    pub unbound: Vec<UnboundAccount>,
    /// Connections that are not syncing.
    pub feeds: Vec<FeedAlert>,
}

/// One account, whether or not it counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountInput {
    pub id: AccountId,
    pub name: String,
    pub currency: Currency,
    pub liquidity: LiquidityClass,
    pub include_in_forecast: bool,
    pub closed: bool,
    /// The latest balance on or before `as_of`.
    pub balance: Option<BalanceInput>,
    pub feed_problems: Vec<FeedProblem>,
}

/// A balance in the account's currency. A card owing money is negative.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BalanceInput {
    pub on: Date,
    pub minor_units: i64,
}

/// Why a provider account bound to an account may not be keeping it current.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeedProblem {
    pub provider_account: ProviderAccountId,
    pub reason: FeedProblemReason,
}

/// What is wrong with a bound provider account. Never the provider's own text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum FeedProblemReason {
    /// The provider reported an error for this account.
    ProviderError,
    /// The provider's currency is not one coffer knows.
    CurrencyUnsupported,
    /// The provider's currency differs from the account's.
    CurrencyDiffers,
}

/// One planned item, stored or hypothetical.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemInput {
    pub source: LineSource,
    pub name: String,
    pub category: Option<CategoryId>,
    /// A positive magnitude: for a goal, its target.
    pub amount: Money,
    pub direction: PlanDirection,
    pub account_hint: Option<AccountId>,
    pub schedule: PlanSchedule,
    /// Whether this month's spending reduces it: `None` unless a recurring
    /// outflow, and a recurring outflow without one is a bill.
    pub spending: Option<PlanSpending>,
    /// Spending linked to a goal, positive for money spent. 0 for anything else.
    pub goal_spent_minor: i64,
}

/// Where a line of the breakdown comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LineSource {
    /// A stored planned item.
    PlannedItem { id: PlannedItemId },
    /// The item at this position in a what-if's list.
    Hypothetical { index: u32 },
}

/// A category's actual spending this month, positive for money out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CategorySpent {
    pub category: CategoryId,
    pub currency: Currency,
    pub spent_minor: i64,
}

/// This month's money in one currency that could yet change a category's
/// spending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LooseEnds {
    pub currency: Currency,
    pub uncategorized_count: u32,
    /// Positive for money out.
    pub uncategorized_minor: i64,
    pub possible_duplicates: u32,
}

/// A provider account bound to no account, whose money the forecast cannot see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnboundAccount {
    pub provider_account: ProviderAccountId,
    pub connection: ConnectionId,
    pub name: String,
    pub institution: Option<String>,
    pub currency_raw: String,
}

/// A connection that is not syncing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedAlert {
    pub connection: ConnectionId,
    pub label: String,
    pub status: FeedStatus,
}

/// Why a connection is not syncing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum FeedStatus {
    Error,
    ReauthRequired,
}

/// Why a forecast could not be answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ForecastError {
    /// A total does not fit in `i64`, or `as_of` is before year 0. Never a
    /// wrapped number.
    #[error("the forecast's totals are too large to represent")]
    OutOfRange,
}

/// The answer: how much cash is free over the window from `as_of` up to, and
/// not including, `end_exclusive`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Forecast {
    pub as_of: Date,
    pub end_exclusive: Date,
    pub horizon_months: u8,
    pub include_income: bool,
    #[cfg_attr(feature = "openapi", schema(required))]
    pub floor: Option<SafetyFloor>,
    pub stale_after_days: u16,
    pub status: ForecastStatus,
    /// Cash available minus cash needed; negative is a shortfall. Null exactly
    /// when `blockers` is not empty.
    #[cfg_attr(feature = "openapi", schema(required))]
    pub surplus: Option<Money>,
    /// One row per currency in use, sorted by currency.
    pub totals: Vec<CurrencyTotals>,
    /// Every account, with whether it counted.
    pub accounts: Vec<AccountLine>,
    /// Every item that adds to the window, in input order.
    pub items: Vec<ItemLine>,
    /// One bucket per calendar month the window touches.
    pub months: Vec<MonthBucket>,
    pub blockers: Vec<Blocker>,
    pub cautions: Vec<Caution>,
}

/// How far to trust the number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum ForecastStatus {
    /// A number, with nothing to warn about.
    Firm,
    /// A number, with cautions.
    Provisional,
    /// No number: see `blockers`.
    Unavailable,
}

/// One currency's arithmetic. Currencies are never netted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CurrencyTotals {
    pub currency: Currency,
    /// The signed sum of counted balances.
    pub cash_available: Money,
    /// Planned outflows in the window, after this month's reduction.
    pub planned_outflows: Money,
    pub goals: Money,
    pub floor: Money,
    /// Planned inflows counted: 0 unless `include_income`.
    pub planned_inflows: Money,
    /// Planned inflows left out because `include_income` is off.
    pub excluded_inflows: Money,
    /// `planned_outflows + goals + floor - planned_inflows`.
    pub cash_needed: Money,
    /// `cash_available - cash_needed`. Null when a blocker other than
    /// `mixed_currencies` applies.
    #[cfg_attr(feature = "openapi", schema(required))]
    pub surplus: Option<Money>,
}

/// One account and what it contributed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AccountLine {
    pub account: AccountId,
    pub name: String,
    pub liquidity: LiquidityClass,
    #[cfg_attr(feature = "openapi", schema(required))]
    pub balance: Option<Money>,
    #[cfg_attr(feature = "openapi", schema(required))]
    pub balance_on: Option<Date>,
    pub counted: bool,
    #[cfg_attr(feature = "openapi", schema(required))]
    pub not_counted: Option<NotCountedReason>,
}

/// Why an account's balance is not part of the cash available.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum NotCountedReason {
    /// The account is switched out of the forecast.
    ExcludedByFlag,
    Closed,
    /// Its liquidity is neither cash nor a short-term liability.
    NotCash,
}

/// One item's part in the window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ItemLine {
    pub source: LineSource,
    pub name: String,
    pub item_type: PlanItemType,
    pub direction: PlanDirection,
    #[cfg_attr(feature = "openapi", schema(required))]
    pub category: Option<CategoryId>,
    #[cfg_attr(feature = "openapi", schema(required))]
    pub account_hint: Option<AccountId>,
    /// How many times it falls in the window: 0 for a goal.
    pub occurrences: u32,
    #[cfg_attr(feature = "openapi", schema(required))]
    pub first_on: Option<Date>,
    #[cfg_attr(feature = "openapi", schema(required))]
    pub last_on: Option<Date>,
    /// The whole amount in the window, before any reduction. 0 only with `unpaid`.
    pub gross: Money,
    /// What this month's actual spending takes off `gross`.
    pub reduced_by: Money,
    /// What a recurring bill or one-off dated earlier this month, before the window,
    /// still needs: the part its category's spending does not cover.
    pub unpaid: Money,
    /// What goes into the totals, `gross - reduced_by + unpaid`: 0 for an inflow
    /// unless `include_income`.
    pub counted: Money,
    /// The sinking fund's own figures, for a goal.
    #[cfg_attr(feature = "openapi", schema(required))]
    pub goal: Option<GoalLine>,
}

/// A sinking fund's figures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct GoalLine {
    pub target: Money,
    pub spent: Money,
    pub due_on: Date,
    /// Whether `due_on` falls in the window: false when it has already passed.
    pub due_in_horizon: bool,
}

/// One calendar month of the window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MonthBucket {
    pub month: CalendarMonth,
    /// The window does not cover the whole calendar month.
    pub partial: bool,
    /// One per currency in use, sorted by currency.
    pub lines: Vec<MonthCurrency>,
}

/// One currency in one month.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MonthCurrency {
    pub currency: Currency,
    /// Planned outflows after this month's reduction.
    pub outflows: Money,
    /// Planned inflows, whether or not `include_income` counts them.
    pub inflows: Money,
    /// What goals add to their need this month. The first month holds all they
    /// had accrued before it.
    pub goal_accrual: Money,
    /// Cash at the end of the month or the window: `cash_available` plus inflows
    /// counted minus outflows so far, without goals or the floor.
    pub projected_cash: Money,
    /// What each item contributes, in input order, without zeros.
    pub items: Vec<MonthItem>,
}

/// One item's contribution to a month: a positive magnitude, whose direction and
/// type are on its [`ItemLine`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MonthItem {
    pub source: LineSource,
    pub amount: Money,
}

/// Why there is no number at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Blocker {
    /// More than one currency is in use; each has its own row in `totals`.
    MixedCurrencies { currencies: Vec<Currency> },
    /// No safety floor is set. A silent floor of 0 would overstate the surplus.
    FloorNotSet,
    /// No account counts as cash.
    NoCountedAccounts,
    /// A counted account has no balance, so it could hold anything.
    BalanceMissing { account: AccountId, name: String },
}

/// Something that keeps the number but stops it being firm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Caution {
    /// A counted balance is older than `stale_after_days` at `as_of`.
    BalanceStale {
        account: AccountId,
        on: Date,
        age_days: u32,
    },
    /// A provider account's money is in no account.
    UnboundProviderAccount {
        provider_account: ProviderAccountId,
        connection: ConnectionId,
        name: String,
        #[cfg_attr(feature = "openapi", schema(required))]
        institution: Option<String>,
        currency_raw: String,
        currency_supported: bool,
    },
    /// A counted account's feed may not be keeping it current.
    AccountFeedProblem {
        account: AccountId,
        provider_account: ProviderAccountId,
        reason: FeedProblemReason,
    },
    /// A connection is not syncing.
    ConnectionNeedsAttention {
        connection: ConnectionId,
        label: String,
        status: FeedStatus,
    },
    /// The window does not start today, so whatever falls between is left out.
    AsOfNotToday { today: Date },
    /// Recurring bills and one-offs dated earlier this month exceed the category's
    /// spending so far. The gap counts as still needed; if it was paid under another
    /// category, the number shows less to invest, which is the safe direction. The
    /// category's spending is taken to pay these before any budget, since the data
    /// cannot tell which it was for, so money spent on a budget can hide a gap.
    BillNotSeen { category: CategoryId, amount: Money },
    /// A closed card still owes money, which counts against cash.
    ClosedLiabilityOwing { account: AccountId, balance: Money },
    /// A floor of some months of recurring outflows came to 0 in this currency:
    /// a silent floor of 0 would overstate the surplus.
    MonthsFloorIsZero { currency: Currency, months: u8 },
    /// A short-term liability shows money in hand, perhaps entered with the
    /// wrong sign. It was added as it stands.
    LiabilityBalancePositive { account: AccountId, balance: Money },
    /// A goal and a one-off may describe the same expense. Neither is
    /// de-duplicated: each counts as it would alone, so a one-off outside the
    /// window warns without adding to the number.
    GoalOneOffOverlap {
        goal: LineSource,
        one_off: LineSource,
        category: CategoryId,
    },
    /// A goal's due date has passed with money still to find.
    GoalOverdue { goal: LineSource, due_on: Date },
    /// An item is hinted at an account that does not count. It is counted
    /// anyway, since overstating the need is the safe direction.
    ItemHintNotCounted {
        item: LineSource,
        account: AccountId,
    },
    /// Uncategorized spending this month might have reduced a recurring item.
    UncategorizedSpending {
        currency: Currency,
        count: u32,
        total: Money,
    },
    /// Possible duplicates this month might be inflating a category's spending.
    PossibleDuplicates { count: u32 },
    /// The first month whose closing cash is below the floor: income arriving
    /// later cannot pay for it.
    CashDipsBelowFloor {
        month: CalendarMonth,
        currency: Currency,
        projected_cash: Money,
    },
}
