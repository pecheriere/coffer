//! Net worth over time (spec §6.9, §8.10): the daily value of each account, the
//! periods a report answers for, and the report. Pure and exact: each account
//! is rounded on its own, and totals add up the rounded values in `i128`.

mod daily;
mod periods;
mod report;
mod types;

pub use daily::{DailyValue, MIN_VALUE_DATE, SnapshotPoint, daily_values};
pub use periods::{MAX_POINTS, NetWorthInterval, PeriodEnd, period_ends};
pub use report::net_worth_report;
pub use types::{
    AccountPoint, AccountSeries, ConversionState, CurrencyState, LiquidityLine, ManualRateUse,
    NetWorthBlocker, NetWorthBucket, NetWorthCaution, NetWorthError, NetWorthInput, NetWorthPoint,
    NetWorthReport, NetWorthStatus, NetWorthValueError, NotCountedReason, RateMissingReason,
    ReportAccount,
};

#[cfg(test)]
mod tests;
