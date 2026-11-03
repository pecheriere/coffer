//! What a net worth report takes in and what it answers (spec §8.10).
//!
//! Balances are as stored: negative is money owed. `total`, a class's `amount`
//! and an account point's `amount` are in the display currency; `by_currency`
//! and a point's `balance` are in the account's own.

use std::str::FromStr;

use jiff::civil::Date;
use serde::{Deserialize, Serialize};

use super::daily::DailyValue;
use super::periods::NetWorthInterval;
use crate::accounts::{BalanceSource, BelongsTo, LiquidityClass};
use crate::ids::AccountId;
use crate::money::{Currency, Money};
use crate::transactions::string_enum;

/// Why a stored net worth word was rejected. Messages never echo the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum NetWorthValueError {
    #[error("unknown conversion state")]
    UnknownConversionState,
    #[error("unknown net worth interval")]
    UnknownInterval,
}

string_enum!(
    /// How an account's balance on a day became an amount in the display
    /// currency.
    ConversionState, NetWorthValueError::UnknownConversionState,
    {
        /// The account is in the display currency: no rate was involved.
        Same => "same",
        /// Converted at the rate of `rate_on`.
        Converted => "converted",
        /// No amount: a rate on or before the day is missing.
        Missing => "missing",
        /// No amount: it does not fit in the 64-bit range of minor units.
        TooLarge => "too_large",
        /// No amount: the household has not set a display currency.
        NoDisplayCurrency => "no_display_currency",
    }
);

/// Everything one report reads, gathered by the caller in one read. It is given
/// visible accounts only, so it cannot leak what it never holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetWorthInput {
    /// The household's day.
    pub today: Date,
    pub from: Date,
    pub to: Date,
    pub interval: NetWorthInterval,
    /// The last day the daily table holds, if any.
    pub through: Option<Date>,
    /// The daily table has to be rebuilt before it can answer.
    pub rebuild_needed: bool,
    /// `None` until an admin sets one; it is never guessed.
    pub display: Option<Currency>,
    /// A counted balance older than this on the latest point is flagged.
    pub stale_after_days: u16,
    /// Whether each account's own points are answered too.
    pub account_points: bool,
    /// The visible accounts, in the order to list them.
    pub accounts: Vec<ReportAccount>,
    /// Daily values. Rows of any other account or day are ignored.
    pub rows: Vec<DailyValue>,
    /// The first day each account has a value, whatever the range. From then on an
    /// included account with no row on a point's day is a hole, reported as one.
    pub first_on: Vec<(AccountId, Date)>,
    /// Why rates may be missing, for the currencies of `accounts` and `display`.
    pub coverage: Vec<CurrencyState>,
}

/// One visible account, as it stands now: these apply at read, so changing one
/// needs no rebuild.
#[derive(Clone, PartialEq, Eq)]
pub struct ReportAccount {
    pub id: AccountId,
    pub name: String,
    pub currency: Currency,
    pub liquidity: LiquidityClass,
    pub include_in_net_worth: bool,
    /// The household's day it was closed.
    pub closed_on: Option<Date>,
    /// Whose money it is: anything but `household` puts it in a bucket.
    pub belongs_to: BelongsTo,
    /// Who or what it is held for, which names its bucket.
    pub beneficiary: Option<String>,
}

/// Leaves out `beneficiary`, which may be a child's name, and the account's `name`.
impl std::fmt::Debug for ReportAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReportAccount")
            .field("id", &self.id)
            .field("currency", &self.currency)
            .field("liquidity", &self.liquidity)
            .field("include_in_net_worth", &self.include_in_net_worth)
            .field("closed_on", &self.closed_on)
            .field("belongs_to", &self.belongs_to)
            .finish_non_exhaustive()
    }
}

/// Why a currency's rates may be missing. `reason` is `None` when it has rates,
/// which leaves only that none is that early.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CurrencyState {
    pub currency: Currency,
    pub reason: Option<RateMissingReason>,
}

/// Why a report could not be answered. Messages are fixed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum NetWorthError {
    /// The range has more points than a report may answer.
    #[error("the range has too many points: at most 1000")]
    TooManyPoints,
    /// The range starts after it ends, or after today.
    #[error("the range starts after it ends")]
    RangeInverted,
    /// A total does not fit in `i64`, or a date is beyond the calendar. Never a
    /// wrapped number.
    #[error("the report's totals are too large to represent")]
    OutOfRange,
}

/// The answer: what the visible accounts come to, period by period.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NetWorthReport {
    /// What `total` is in; `null` until an admin sets it.
    #[cfg_attr(feature = "openapi", schema(required))]
    pub display_currency: Option<Currency>,
    pub interval: NetWorthInterval,
    /// The first day of the range answered.
    pub from: Date,
    /// The last day of the range as asked: it can be later than the last point, which is
    /// never after today or `through`.
    pub to: Date,
    /// The last day with values: no later date is answered. `null` while the values are
    /// being rebuilt.
    #[cfg_attr(feature = "openapi", schema(required))]
    pub through: Option<Date>,
    /// How far to trust the numbers: `firm`, `provisional` or `unavailable`.
    pub status: NetWorthStatus,
    /// One per period holding a counted balance or a missing value, oldest first.
    pub points: Vec<NetWorthPoint>,
    /// Every visible account that is the household's, with whether it counts.
    pub accounts: Vec<AccountSeries>,
    /// Why there is no number; not empty exactly when `status` is `unavailable`.
    pub blockers: Vec<NetWorthBlocker>,
    /// What keeps the numbers but stops them being firm.
    pub cautions: Vec<NetWorthCaution>,
    /// Money that isn't the household's, in no number above and added to nothing:
    /// `someone_else` before `given_away`, and within one no label first, then labels by
    /// their bytes.
    pub buckets: Vec<NetWorthBucket>,
}

/// One `belongs_to` and exact `beneficiary` among the visible accounts that aren't the
/// household's: the same calculation as the household's, over its own accounts alone. No
/// total adds it to net worth or to another bucket.
#[derive(Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NetWorthBucket {
    /// Whose money it is: never `household`.
    pub belongs_to: BelongsTo,
    /// `null` when no name was given. Two spellings of a name are two buckets.
    #[cfg_attr(feature = "openapi", schema(required))]
    pub beneficiary: Option<String>,
    /// How far to trust this bucket's numbers: its own, whatever the household's is.
    pub status: NetWorthStatus,
    /// One per period holding a balance of this bucket or a missing value, oldest first.
    pub points: Vec<NetWorthPoint>,
    /// Every visible account in this bucket, and in no other list.
    pub accounts: Vec<AccountSeries>,
    /// Why this bucket has no number; not empty exactly when `status` is `unavailable`.
    pub blockers: Vec<NetWorthBlocker>,
    /// What keeps this bucket's numbers but stops them being firm.
    pub cautions: Vec<NetWorthCaution>,
}

/// Leaves out `beneficiary`, which may be a child's name, and its `accounts`.
impl std::fmt::Debug for NetWorthBucket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NetWorthBucket")
            .field("belongs_to", &self.belongs_to)
            .field("status", &self.status)
            .field("points", &self.points)
            .field("blockers", &self.blockers)
            .field("cautions", &self.cautions)
            .finish_non_exhaustive()
    }
}

/// How far to trust the numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum NetWorthStatus {
    /// Numbers, with nothing to warn about.
    Firm,
    /// Numbers, with cautions.
    Provisional,
    /// See `blockers`.
    Unavailable,
}

/// The value at the end of one period.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NetWorthPoint {
    /// The day valued: the last of the period, or the last available.
    pub on: Date,
    /// The first day of the period inside the range: never before the range's `from`.
    pub period_start: Date,
    /// `on` is not the natural end of the period.
    pub partial: bool,
    /// The sum of the counted accounts' amounts; `null` when one has none.
    #[cfg_attr(feature = "openapi", schema(required))]
    pub total: Option<Money>,
    /// One line per liquidity class holding a counted account.
    pub by_liquidity: Vec<LiquidityLine>,
    /// Native sums of the counted balances, sorted by currency. Always present,
    /// and leaving out only an account in `unconverted` with no value that day.
    pub by_currency: Vec<Money>,
    /// Accounts this point has no amount for: counted with no conversion though
    /// there is a display currency, or holding no value on this day though they
    /// did earlier.
    pub unconverted: Vec<AccountId>,
    /// An amount on this point was converted with a rate an admin entered, so `total`, if
    /// there is one, rests on it: see the caution `rate_manual`.
    pub manual_rate: bool,
}

/// One liquidity class on one day.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct LiquidityLine {
    pub liquidity: LiquidityClass,
    /// `null` when a counted account in it has no amount.
    #[cfg_attr(feature = "openapi", schema(required))]
    pub amount: Option<Money>,
    /// An amount in this class was converted with a rate an admin entered.
    pub manual_rate: bool,
}

/// One visible account and its values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AccountSeries {
    pub account: AccountId,
    pub name: String,
    pub liquidity: LiquidityClass,
    pub currency: Currency,
    /// Whether it is included in the sums this list sits beside at all, the household's or
    /// its bucket's and never both: true in a bucket is not a place in net worth. A point
    /// says whether that day counted.
    pub counted: bool,
    #[cfg_attr(feature = "openapi", schema(required))]
    pub not_counted: Option<NotCountedReason>,
    /// Its first day with a value: a jump on that day is an account arriving,
    /// not money.
    #[cfg_attr(feature = "openapi", schema(required))]
    pub first_on: Option<Date>,
    /// The household's day it closed: from then it counts only while it owes, so
    /// a drop on that day is an account closing, not money.
    #[cfg_attr(feature = "openapi", schema(required))]
    pub closed_on: Option<Date>,
    /// Its value at each point, if it had one by then. An account switched out of net
    /// worth (`not_counted`) has none.
    pub points: Vec<AccountPoint>,
}

/// Why an account is not part of net worth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema), schema(as = NetWorthNotCountedReason))]
#[serde(rename_all = "snake_case")]
pub enum NotCountedReason {
    /// It is switched out of net worth.
    ExcludedByFlag,
}

/// One account at one point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AccountPoint {
    pub on: Date,
    /// In the account's own currency.
    pub balance: Money,
    /// The date of the snapshot this balance comes from.
    pub balance_on: Date,
    pub balance_source: BalanceSource,
    /// Whether it counts on this day: not on or after its closing day, unless it
    /// still owes.
    pub counted: bool,
    /// In the display currency.
    #[cfg_attr(feature = "openapi", schema(required))]
    pub amount: Option<Money>,
    /// The older of the rate dates used.
    #[cfg_attr(feature = "openapi", schema(required))]
    pub rate_on: Option<Date>,
    /// `amount` was converted with a rate an admin entered.
    pub manual_rate: bool,
    pub conversion: ConversionState,
}

/// Why there is no number to trust. `display_currency_not_set`: an admin has to choose
/// one, and points still hold their sums per currency. `rebuilding`: the daily values are
/// being rebuilt, so there are no points. `no_counted_accounts`: nothing visible counts in
/// this range, which is not a net worth of zero; on a bucket, nothing in it has a value in this
/// range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NetWorthBlocker {
    /// An admin has to choose one. Points still hold their sums per currency.
    DisplayCurrencyNotSet,
    /// The daily values are being rebuilt.
    Rebuilding,
    /// Nothing visible counts in this range. Not a net worth of zero.
    NoCountedAccounts,
}

/// Something that keeps the numbers but stops them being firm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NetWorthCaution {
    /// Between these points a currency's rate was missing, so amounts in it are
    /// `null`.
    RateMissing {
        currency: Currency,
        reason: RateMissingReason,
        /// The first point lacking it.
        first_on: Date,
        /// The last point lacking it.
        last_on: Date,
    },
    /// The point at which an amount used the oldest rate, older than a week.
    RateStale {
        currency: Currency,
        on: Date,
        rate_on: Date,
        age_days: u32,
    },
    /// Between these points amounts were converted with a rate of this currency that an
    /// admin entered, not one a source published, so they are only as good as that entry.
    RateManual {
        currency: Currency,
        /// The first point using it.
        first_on: Date,
        /// The last point using it.
        last_on: Date,
    },
    /// The last point at which this account's amount did not fit, so it is
    /// `null`.
    AmountTooLarge { account: AccountId, on: Date },
    /// On the latest point, this counted balance dates from `on`.
    BalanceStale {
        account: AccountId,
        on: Date,
        age_days: u32,
    },
    /// The last filled day is before the end of the range, or today if that is
    /// earlier: later dates are not answered yet.
    ToDateNotYetFilled { through: Date },
    /// Between these points an account already holding values has none, so what
    /// holds it is `null`, it is in `unconverted`, and its balance is in no
    /// `by_currency`. A hole in the daily values, not money leaving.
    DailyValueMissing {
        account: AccountId,
        first_on: Date,
        last_on: Date,
    },
    /// From `from` up to `to` the range's periods hold nothing counted, so the
    /// last point is older than the range's end and is not the value now: every
    /// account counted by then has closed, and a closed account's `closed_on`
    /// says when.
    NothingCountedAfterLastPoint { from: Date, to: Date },
    /// A liability shows money in hand, perhaps entered with the wrong sign. It
    /// was added as it stands. `balance` is from the last point where it did.
    LiabilityBalancePositive { account: AccountId, balance: Money },
    /// A closed account still owes money, which keeps counting. `balance` is
    /// from the last point where it did.
    ClosedAccountOwing { account: AccountId, balance: Money },
}

/// Why a currency has no rate for a day.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum RateMissingReason {
    NotFetchedYet,
    NotPublished,
    FetchFailing,
    /// This server is configured with no rate provider.
    ProviderOff,
    /// It has rates, none on or before that day.
    NoneThatEarly,
}
