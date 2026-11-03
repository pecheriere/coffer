//! What to believe of the day a provider puts on a balance. The day is the
//! provider's to choose, and a dishonest one could use it to rewrite or extend an
//! account's history backward. An honest one can report an old day too, since a
//! balance's date may be when it last changed, so an old day alone is no reason to
//! lose the amount (spec §8.6).

use coffer_domain::accounts::BalanceSource;
use coffer_domain::ids::{AccountId, ProviderAccountId};
use coffer_domain::money::Currency;
use coffer_store::{KeptBalance, StoreTx};
use jiff::Span;
use jiff::Timestamp;
use jiff::civil::Date;

use super::write_balance;
use crate::sync::SyncError;

/// How many days back a balance a provider reports may be dated and be taken at
/// its word. A member's own balance is no report and has no such limit.
const REPORTED_BALANCE_MAX_AGE_DAYS: i64 = 31;

/// The oldest day a provider's date is taken at its word, in the household's days.
fn oldest_believed_day(today: Date) -> Date {
    today.saturating_sub(Span::new().days(REPORTED_BALANCE_MAX_AGE_DAYS))
}

/// A balance a provider reported, converted to minor units of its currency.
#[derive(Debug, Clone, Copy)]
pub(in crate::sync) struct ReportedBalance {
    pub(in crate::sync) on: Date,
    pub(in crate::sync) balance_minor: i64,
    pub(in crate::sync) currency: Currency,
}

/// What becomes of a reported balance on a bound account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::sync) enum Judged {
    /// Write it, on this day.
    Write(Date),
    /// It tells nothing the account doesn't already hold, and writes nothing.
    Known,
    /// It would add or change history at or before the latest snapshot.
    Refused,
}

/// Judges a balance of `balance_minor` that a provider dated `on`, for `account`
/// as it stands. A day after `today` counts as today, or it would hide later
/// balances. Within [`REPORTED_BALANCE_MAX_AGE_DAYS`] the date is believed, unless the
/// report [`repeats`] the latest balance on a day that holds one, which is known. Before
/// that, nothing older than the oldest believed day is ever written:
///
/// - An account with no snapshot, or whose latest is before the reported day, takes
///   the amount on the oldest believed day, later than reported. A date in 1970 adds
///   no history, a dormant account still has a balance, and the provider gets nothing
///   it couldn't by dating the report that day. That day is empty, since the latest
///   snapshot is older still.
/// - The amount of the latest snapshot, or the same day and amount as any snapshot,
///   is known. The first is what keeps an account taken on the oldest believed day
///   quiet: it goes on reporting its real, earlier date.
/// - Anything else is refused.
pub(in crate::sync) async fn judge_reported(
    tx: &mut StoreTx,
    account: AccountId,
    on: Date,
    balance_minor: i64,
    today: Date,
) -> Result<Judged, SyncError> {
    let on = on.min(today);
    let oldest = oldest_believed_day(today);
    let latest = tx.latest_balance(account).await?;
    if on >= oldest {
        let moved = latest.as_ref().is_some_and(|latest| {
            latest.source == BalanceSource::Sync
                && repeats(balance_minor, on, latest.on, latest.balance_minor)
        }) && tx.balance_on(account, on).await?.is_some();
        return Ok(if moved {
            Judged::Known
        } else {
            Judged::Write(on)
        });
    }
    let Some(latest) = latest.filter(|latest| on <= latest.on) else {
        return Ok(Judged::Write(oldest));
    };
    let same_day = tx.balance_on(account, on).await?;
    let known = latest.balance_minor == balance_minor
        || same_day.is_some_and(|snapshot| snapshot.balance_minor == balance_minor);
    Ok(if known {
        Judged::Known
    } else {
        Judged::Refused
    })
}

/// The most a change of zone can move an instant's day: the zones furthest apart differ by
/// 25 hours.
const MOST_A_ZONE_MOVES_A_DAY: i64 = 2;

/// Whether `minor`, on `on`, is what the latest provider-written balance says, from a day
/// at most a change of zone away. It is what a report of the same moment looks like after
/// the zone moved back (spec §8.7). The caller goes on only if `on` already holds a balance,
/// which writing this would replace with what that day didn't end on; an empty day loses
/// nothing by being written.
fn repeats(minor: i64, on: Date, latest_on: Date, latest_minor: i64) -> bool {
    minor == latest_minor
        && on < latest_on
        && on
            .checked_add(Span::new().days(MOST_A_ZONE_MOVES_A_DAY))
            .is_ok_and(|furthest| latest_on <= furthest)
}

/// The days after `on` and after `today` that a change of zone can have left a
/// provider-written balance on. A newer report replaces those, or that old amount would
/// come back as the household's latest when the day arrives.
fn later_days_past_today(on: Date, today: Date) -> impl Iterator<Item = Date> {
    (1..=MOST_A_ZONE_MOVES_A_DAY)
        .filter_map(move |ahead| on.checked_add(Span::new().days(ahead)).ok())
        .filter(move |day| *day > today)
}

/// Keeps a balance reported while unbound, only if its date is believed, and by the rules
/// a change of zone needs: see [`repeats`] and [`later_days_past_today`].
async fn keep_reported(
    tx: &mut StoreTx,
    now: Timestamp,
    provider_account: ProviderAccountId,
    balance: ReportedBalance,
    today: Date,
) -> Result<(), SyncError> {
    let on = balance.on.min(today);
    if on < oldest_believed_day(today) {
        tracing::debug!(provider_account = %provider_account, "a balance too old to keep was left out");
        return Ok(());
    }
    let minor = balance.balance_minor;
    let keep = |on| KeptBalance {
        provider_account,
        on,
        balance_minor: minor,
        currency: balance.currency,
        recorded_at: now,
    };
    let latest = tx.latest_kept_balance(provider_account).await?;
    let moved = latest.is_some_and(|(day, kept)| repeats(minor, on, day, kept))
        && tx.kept_balance_on(provider_account, on).await?.is_some();
    if !moved {
        tx.keep_balance(keep(on)).await?;
    }
    for day in later_days_past_today(on, today) {
        if tx.kept_balance_on(provider_account, day).await?.is_some() {
            tx.keep_balance(keep(day)).await?;
        }
    }
    Ok(())
}

/// Records a balance a provider reported for `provider_account`. On the account it
/// is bound to, as [`judge_reported`] says, noting a refusal and clearing an earlier
/// one otherwise; a report not refused also replaces what [`later_days_past_today`] names.
///
/// While unbound, it is kept for the bind to bring in only if its date is believed.
/// There is no latest snapshot to judge it against, and nothing older is needed: the
/// bind judges the last report. Keeping isn't audited: the audit log would then hold the balances
/// of an account nobody has bound, and the bind's entry counts them.
pub(super) async fn record_balance(
    tx: &mut StoreTx,
    now: Timestamp,
    provider_account: ProviderAccountId,
    account: Option<AccountId>,
    balance: ReportedBalance,
    today: Date,
) -> Result<(), SyncError> {
    let Some(account) = account else {
        return keep_reported(tx, now, provider_account, balance, today).await;
    };
    let minor = balance.balance_minor;
    let judged = judge_reported(tx, account, balance.on, minor, today).await?;
    match judged {
        Judged::Write(on) => {
            write_balance(tx, now, account, on, balance.on, minor).await?;
        }
        Judged::Known => {}
        Judged::Refused => {
            tracing::warn!(provider_account = %provider_account, "a balance older than the account's latest was refused");
        }
    }
    if judged != Judged::Refused {
        for day in later_days_past_today(balance.on.min(today), today) {
            let row = tx.balance_on(account, day).await?;
            if row.is_some_and(|row| row.source == BalanceSource::Sync) {
                write_balance(tx, now, account, day, balance.on, minor).await?;
            }
        }
    }
    tx.set_old_balance_refused(provider_account, judged == Judged::Refused)
        .await?;
    Ok(())
}
