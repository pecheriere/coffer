//! What a forecast takes in and what it answers (spec §6.8, §8.9).
//!
//! Output amounts are [`Money`]. A planned item's amounts are positive
//! magnitudes with a [`PlanDirection`]; balances, `cash_available`,
//! `projected_cash` and `surplus` carry a sign, and a negative `surplus` is a
//! shortfall.

use std::collections::BTreeMap;

use jiff::civil::Date;
use serde::Serialize;

use crate::accounts::{BelongsTo, LiquidityClass};
use crate::fx::RateBook;
use crate::ids::{
    AccountId, CategoryId, ConnectionId, PlannedItemId, ProviderAccountId, TransactionId,
    TransferLinkId,
};
use crate::money::{Currency, Money};
use crate::plan::{
    CalendarMonth, PlanDirection, PlanItemType, PlanSchedule, PlanSpending, SafetyFloor,
    SettlementKind,
};
use crate::transfer_rate::CrossCurrencyLink;

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
    /// Actual spending per category in `as_of`'s month, up to `as_of` and no later
    /// than its account's balance date, since a balance does not show what is newer.
    pub category_spent: Vec<CategorySpent>,
    /// The same spending, one row per transaction and category, from
    /// [`payments_read_from`](super::payments_read_from): what can pay a bill.
    pub spent_transactions: Vec<SpentTransaction>,
    /// Spending in `as_of`'s month that is not settled yet.
    pub loose_ends: Vec<LooseEnds>,
    /// The same for last month, where a bill's payment may be waiting to be filed.
    pub last_month_loose_ends: Vec<LooseEnds>,
    /// The first posted transaction on any of the household's accounts, whatever their state, so
    /// that closing or switching one can't move it forward. Before it, coffer has no evidence of
    /// whether last month's bill was paid.
    pub earliest_transaction: Option<Date>,
    /// Outflows on counted accounts that their balances may not show yet.
    pub newer_spending: Vec<NewerSpending>,
    /// What rules filed under a `transfer` category with no link in `as_of`'s month. Only reported.
    pub rule_filed_transfers: Vec<RuleFiledTransfers>,
    /// Links between counted accounts with a leg its provider took back and a leg the look-back
    /// reaches.
    pub links_with_leg_removed: Vec<TransferLinkId>,
    /// Links between counted accounts in two currencies with a leg in `as_of`'s month.
    pub cross_currency_links: Vec<CrossCurrencyLink>,
    /// The rates to compare their legs at. Nothing else reads them: no number of the
    /// forecast depends on a rate.
    pub rates: RateBook,
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
    /// Whose money it is. Only the household's can count, whatever else holds (spec §8.11
    /// B8).
    pub belongs_to: BelongsTo,
    pub include_in_forecast: bool,
    pub closed: bool,
    /// The latest balance on or before `as_of`.
    pub balance: Option<BalanceInput>,
    pub feed_problems: Vec<FeedProblem>,
    /// Whether spending linked to a goal on this account lowers a goal's need, which it does
    /// on any account, the household's or not.
    pub goal_spending: bool,
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
    /// Whether what is linked to this goal is on both the household's accounts and an account
    /// that isn't the household's. False for anything else.
    pub goal_linked_both_sides: bool,
    /// The months a person settled it for. Only a stored recurring bill or one-off
    /// outflow has any: a what-if can't settle, and the ledger refuses anything else.
    pub settled: BTreeMap<CalendarMonth, SettlementKind>,
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

/// What one transaction spent in one category, by the rules of [`CategorySpent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpentTransaction {
    pub transaction: TransactionId,
    pub category: CategoryId,
    pub currency: Currency,
    pub posted_on: Date,
    /// Positive for money out, negative for a refund.
    pub spent_minor: i64,
    /// It may be another transaction again, so it pays no bill.
    pub possible_duplicate: bool,
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

/// What counted accounts spent in one currency that their balances may not show yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewerSpending {
    pub currency: Currency,
    /// Posted outflows after the balances' dates, positive for money out.
    pub total_minor: i64,
    /// Pending outflows up to `as_of`, whatever the balances' dates, positive for
    /// money out.
    pub pending_minor: i64,
}

/// Money out that rules filed under a `transfer` category in `as_of`'s month, in one currency,
/// with no transfer link. A too-broad rule can hide a real expense this way, so it is worth a
/// look; it is only reported, and changes no number and no status, since a sweep rule is
/// legitimate. A person's own filing under `transfer` isn't counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RuleFiledTransfers {
    pub currency: Currency,
    /// The transactions.
    pub count: u32,
    /// What was filed under `transfer` on them, a positive amount.
    pub amount: Money,
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
    /// The accounts its provider accounts are bound to. It cautions the forecast only if it
    /// could change the number: it has no provider account, one of these is read by the number, or it
    /// has a provider account in [`ForecastInput::unbound`].
    pub serves: Vec<AccountId>,
    /// Whether the connection has any provider account, bound or not. None means a first sync
    /// that never found the bank's accounts.
    pub has_accounts: bool,
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
    /// Every item that adds to the window or has a payment to show, in input order.
    pub items: Vec<ItemLine>,
    /// One bucket per calendar month the window touches.
    pub months: Vec<MonthBucket>,
    pub blockers: Vec<Blocker>,
    pub cautions: Vec<Caution>,
    /// What rules filed as `transfer` with no link in `as_of`'s month, one row per currency
    /// with any, sorted by currency. Informational: it never changes `status` or a number.
    /// `GET /transactions?unmatched_transfer=true&rule=<rule>` lists them.
    pub rule_filed_transfers: Vec<RuleFiledTransfers>,
    /// What a person settled, and so what counts as nothing above: in input order, then
    /// by month. Settling raises no caution of its own, since a person decided.
    pub settled: Vec<SettledLine>,
    /// Budgets lowered by spending that a bill settled as paid may have claimed: informational,
    /// since a settled bill is a person's decision, so it never changes `status`. Empty when no
    /// budget was lowered that way.
    pub settled_budgets: Vec<SettledBudget>,
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
    /// Planned inflows left out because `include_income` is off. Income expected in an account
    /// that doesn't count isn't among them, since turning it on wouldn't add it.
    pub excluded_inflows: Money,
    /// `planned_outflows + goals + floor - planned_inflows`.
    pub cash_needed: Money,
    /// `cash_available - cash_needed`. Null when a blocker other than
    /// `mixed_currencies` applies.
    #[cfg_attr(feature = "openapi", schema(required))]
    pub surplus: Option<Money>,
}

/// What became of an item hinted at an account that does not count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum HintEffect {
    /// An outflow still counts as needed, since overstating the need is the safe direction.
    StillNeeded,
    /// An inflow into an account that doesn't count may never reach counted cash, so it stays
    /// out of the number. That is already right: removing the hint would add it.
    LeftOut,
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
    /// The money is someone else's, or given away (spec §8.11 B8). Nothing about the
    /// account's flags changes this: only saying it is the household's does.
    NotHousehold,
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
    /// How many times it falls in the window, and for a stored budget earlier in `as_of`'s
    /// month too: 0 for a goal. An inflow counts only from the day after `as_of`, and a bill
    /// already paid, up to a week early, isn't counted (it is in `payments`).
    pub occurrences: u32,
    #[cfg_attr(feature = "openapi", schema(required))]
    pub first_on: Option<Date>,
    #[cfg_attr(feature = "openapi", schema(required))]
    pub last_on: Option<Date>,
    /// The whole amount of `occurrences`, before any reduction. 0 only with `unpaid` or
    /// `payments`.
    pub gross: Money,
    /// What this month's actual spending takes off `gross`.
    pub reduced_by: Money,
    /// What a recurring bill or one-off dated before the window still needs, back through last
    /// month: the occurrences no transaction paid.
    pub unpaid: Money,
    /// What goes into the totals, `gross - reduced_by + unpaid`: 0 for an inflow unless
    /// `include_income`, and always for one hinted at an account that isn't the household's.
    pub counted: Money,
    /// The sinking fund's own figures, for a goal.
    #[cfg_attr(feature = "openapi", schema(required))]
    pub goal: Option<GoalLine>,
    /// For a stored recurring bill or one-off: each occurrence from the start of last month up
    /// to and including `as_of`, and each later one a payment already paid, by date, and what
    /// paid it. A settled one isn't here but in `settled`. Empty for anything else.
    pub payments: Vec<OccurrencePayment>,
}

/// One occurrence of a bill, and the transaction that paid it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct OccurrencePayment {
    pub on: Date,
    pub amount: Money,
    /// Null when no transaction matched, so it counts as still needed, in `unpaid` if it is dated
    /// before `as_of`, unless `before_history`. Paid, it needs nothing, whatever its date.
    #[cfg_attr(feature = "openapi", schema(required))]
    pub paid_by: Option<PaidBy>,
    /// Dated last month, before the household's first transaction, so there is no evidence either
    /// way: it takes no payment and isn't treated as unpaid.
    pub before_history: bool,
}

/// The transaction matched to an occurrence. If it is wrong, settling the occurrence's month
/// sends it to the next one, and filing it in another category takes it away.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PaidBy {
    pub transaction: TransactionId,
    pub posted_on: Date,
    /// What it spent in the item's category, a positive magnitude, after any refund of it.
    pub amount: Money,
    /// `amount` less the occurrence's: negative when it paid less than planned. Whatever it is,
    /// the occurrence counts as paid.
    pub difference: Money,
}

/// One item's occurrences in one month that a person settled: what would have counted, had
/// they not, as a bill's whole amount or a gap, and taken a share of its category's spending.
/// A month before last month or past the window isn't listed, since it changes nothing here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SettledLine {
    pub source: LineSource,
    pub name: String,
    pub item_type: PlanItemType,
    #[cfg_attr(feature = "openapi", schema(required))]
    pub category: Option<CategoryId>,
    pub month: CalendarMonth,
    pub kind: SettlementKind,
    /// How many of the month's occurrences the forecast looks at, back to the first of last
    /// month and so before `as_of` too.
    pub occurrences: u32,
    /// The whole amount of `occurrences`. A bill spending had already covered wasn't in
    /// the number, so the number may differ by less than this.
    pub amount: Money,
}

/// A budget that spending reduced where a bill settled as paid shares its category, and nothing
/// says what paid that bill. Settled as paid, the bill's own amount already came out of the
/// category's spending; what reduced the budget is the rest, which may be the bill's too, as with
/// a charge past the tolerance of the bill. If it was, the budget needs up to `amount` more.
/// Only `as_of`'s month is reduced by spending, so it is the one named.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SettledBudget {
    pub source: LineSource,
    pub name: String,
    #[cfg_attr(feature = "openapi", schema(required))]
    pub category: Option<CategoryId>,
    pub month: CalendarMonth,
    /// What came off the budget's need: its `reduced_by`, a positive amount.
    pub amount: Money,
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
    /// Planned inflows, whether or not `include_income` counts them, but for one hinted at an
    /// account that isn't the household's.
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
    /// A counted balance is older than `stale_after_days`, its age measured to the earlier
    /// of `as_of` and today.
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
    /// The window does not start today. What falls due before last month, as seen from
    /// `as_of`, is left out, as is
    /// planned income dated on or before `as_of` and any hypothetical item dated before it.
    AsOfNotToday { today: Date },
    /// Recurring bills and one-offs dated earlier this month that no transaction paid, summed
    /// per category. They count as still needed; if one was paid under another category or at
    /// another amount, the number shows less to invest, which is the safe direction. `items` are
    /// the items they belong to, in input order.
    BillNotSeen {
        category: CategoryId,
        amount: Money,
        items: Vec<LineSource>,
    },
    /// A bill or one-off dated last month that no transaction paid and nobody settled. It counts
    /// as still needed until one does, or until a person settles that month.
    BillUnpaidLastMonth {
        item: LineSource,
        on: Date,
        amount: Money,
    },
    /// Last month's bills or one-offs dated before the household's first transaction (`starts_on`,
    /// null with none yet). Coffer can't tell whether they were paid, so they aren't treated as
    /// unpaid: `occurrences` of them, `amount` in all, are in none of the totals. This month's
    /// always count.
    BillHistoryMissing {
        #[cfg_attr(feature = "openapi", schema(required))]
        starts_on: Option<Date>,
        currency: Currency,
        occurrences: u32,
        amount: Money,
    },
    /// A refund in a category where a bill is paid, too small or large to count against a charge
    /// there, so the bill stays paid. If it was part of that payment, the surplus reads high by up
    /// to `amount`.
    RefundBesidePaidBill {
        category: CategoryId,
        transaction: TransactionId,
        amount: Money,
    },
    /// Last month has money that can't pay a bill until a person files it, and a bill from last
    /// month is unpaid: an uncategorized transaction or a possible duplicate may be its payment.
    LastMonthLooseEnds {
        currency: Currency,
        uncategorized_count: u32,
        uncategorized_total: Money,
        possible_duplicates: u32,
    },
    /// A transaction is close to this bill's amount, but its category also has a budget that
    /// could have spent it, so it isn't taken as the payment. The bill counts as still needed,
    /// and at most the bill's amount of it stays with the budget. Settling the month as `paid`
    /// says it was the payment, and takes it off the budget.
    BillPaymentUnclear {
        item: LineSource,
        on: Date,
        amount: Money,
        transaction: TransactionId,
        transaction_amount: Money,
    },
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
    /// One goal has spending linked to it on both the household's accounts, counted or not,
    /// and an account that isn't the household's. A contribution and the payment it funded would then both lower its need,
    /// and the data can't tell, so the number stays and stops being firm. Link the payment or
    /// the contribution, never both. A finished goal raises nothing.
    GoalLinkedBothSides { goal: LineSource },
    /// An item is hinted at an account that does not count, and `effect` says what became
    /// of it.
    ItemHintNotCounted {
        item: LineSource,
        account: AccountId,
        effect: HintEffect,
    },
    /// Uncategorized spending this month might have reduced a recurring item.
    UncategorizedSpending {
        currency: Currency,
        count: u32,
        total: Money,
    },
    /// A provider took back one leg of this transfer link between counted accounts, so its
    /// money counts as it is filed again, spending included, until a person says what happened.
    TransferLegRemoved { link: TransferLinkId },
    /// The legs of this transfer link between counted accounts in two currencies differ by
    /// more than the tolerance at the rate for the day they posted, which is the case for a
    /// slipped decimal point. What moved is still taken as it stands, so spending that wasn't
    /// really a movement may be missing from the number until a person checks the link.
    TransferLinkRateMismatch {
        link: TransferLinkId,
        /// What left, negative.
        outflow: Money,
        /// What arrived.
        inflow: Money,
        /// What the outflow comes to in the inflow's currency at the rate used.
        expected_inflow: Money,
        /// The date of the older rate used.
        rate_on: Date,
        /// An admin entered a rate used.
        manual_rate: bool,
    },
    /// Possible duplicates this month might be inflating a category's spending, which
    /// could shrink a reduction. One pays no bill until a person says it isn't a duplicate.
    PossibleDuplicates { count: u32 },
    /// Money out of counted accounts that their balances may not show yet: `total` posted
    /// after a balance's date, and separately `pending`, not yet posted, whatever that
    /// date. A transaction is one or the other. The number stays as it is, since a
    /// provider's balance date may lag what its balance reflects, and guessing could
    /// count the money twice or not at all. Where a balance doesn't show it, the cash
    /// available is overstated by up to these.
    SpendingNewerThanBalance {
        currency: Currency,
        total: Money,
        pending: Money,
    },
    /// The first month whose closing cash is below the floor: income arriving
    /// later cannot pay for it.
    CashDipsBelowFloor {
        month: CalendarMonth,
        currency: Currency,
        projected_cash: Money,
    },
}
