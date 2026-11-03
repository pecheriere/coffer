//! Net worth history and the exchange rates behind it (spec §8.10): what the store holds,
//! turned into the input of the domain's calculation.
//!
//! A report sums what the actor sees at `balances`, unmarked, and answers from those
//! accounts and the household's settings alone (N2), so the scope is decided before
//! anything is read and filters every query. What is known of fetching rates can depend
//! on a hidden account, so it reaches a report only as the reason for a rate a visible row
//! lacks. The rates themselves are shared: N2 rules on what a rate already held can tell a
//! token about its own currency. The FX endpoints say which currencies are in use, so they
//! need `all` at `balances`. None of this writes.
//!
//! A bucket (§8.11 B6, B7) is made in the domain from these same accounts, so a token's
//! buckets hold what it sees at `balances` and nothing else.

use std::collections::BTreeSet;

use coffer_domain::fx::{FxCoverage, FxFetchStatus, RateObservation};
use coffer_domain::identity::Actor;
use coffer_domain::money::Currency;
use coffer_domain::net_worth::{
    CurrencyState, MIN_VALUE_DATE, NetWorthError, NetWorthInput, NetWorthInterval, NetWorthReport,
    RateMissingReason, ReportAccount, net_worth_report, period_ends,
};
use coffer_domain::plan::{strict_date, validate_as_of};
use coffer_domain::policy::{AccountLevel, Action, authorize};
use coffer_store::{AccountRecord, NetWorthData};
use jiff::ToSpan as _;
use jiff::civil::Date;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::{Ledger, LedgerError, plan_items};

/// The longest range of rates one request reads, both ends counted.
const MAX_RATE_DAYS: i32 = 1000;

const RATE_RANGE_TOO_LONG: &str = "a range covers at most 1000 days";
const RATE_RANGE_INVERTED: &str = "the range starts after it ends";

/// Which periods a net worth report covers.
#[derive(Debug, Clone, Copy, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct NetWorthQuery {
    /// The first day, written exactly `YYYY-MM-DD`, in the years 1970 to 2200: by default
    /// one year before `to` or today, whichever is earlier. A `from` after today, or after
    /// `to`, is refused with 422.
    #[serde(default, deserialize_with = "strict_date::deserialize_option")]
    pub from: Option<Date>,
    /// The last day, written exactly `YYYY-MM-DD`, in the years 1970 to 2200: by default
    /// the household's today. No later day than today or `through` is answered.
    #[serde(default, deserialize_with = "strict_date::deserialize_option")]
    pub to: Option<Date>,
    /// `day`, `week` (ending on Sunday) or `month`, the default.
    pub interval: Option<NetWorthInterval>,
    /// Whether each account's own points are answered too, by default `true`.
    pub account_points: Option<bool>,
}

/// Which rates to read.
#[derive(Debug, Clone, Copy, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct FxRatesQuery {
    /// The currency, an ISO 4217 code.
    #[param(value_type = String, pattern = "^[A-Z]{3}$")]
    pub currency: Currency,
    /// The first day, written exactly `YYYY-MM-DD`.
    #[serde(deserialize_with = "strict_date::deserialize")]
    pub from: Date,
    /// The last day, written exactly `YYYY-MM-DD`. The range covers at most 1,000 days,
    /// both ends counted.
    #[serde(deserialize_with = "strict_date::deserialize")]
    pub to: Date,
}

/// What is known of fetching each currency's rates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct FxCoverageList {
    /// One per currency tried, by currency. A currency never tried has no entry.
    pub items: Vec<FxCoverage>,
}

/// One currency's rates, in units of it per one euro.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct FxRateList {
    pub currency: Currency,
    /// Newest first. A day with no entry had no published rate.
    pub items: Vec<RateObservation>,
}

/// The message for a refused report, which is fixed.
fn refused(error: NetWorthError) -> LedgerError {
    const TOO_MANY: &str = "the range has too many points: at most 1000";
    const INVERTED: &str = "the range starts after it ends";
    const TOO_LARGE: &str = "the report's totals are too large to represent";
    LedgerError::Invalid(match error {
        NetWorthError::TooManyPoints => TOO_MANY,
        NetWorthError::RangeInverted => INVERTED,
        // `NetWorthError` is non-exhaustive; this is the only other refusal.
        _ => TOO_LARGE,
    })
}

/// Why a rate missing for this currency is missing. With a rate held, any later day would
/// find it, so a day lacking one is before the first (`None`), whatever the last fetch did.
fn missing_reason(known: Option<&FxCoverage>, provider_on: bool) -> Option<RateMissingReason> {
    if known.is_some_and(|known| known.first_on.is_some()) {
        return None;
    }
    if !provider_on {
        return Some(RateMissingReason::ProviderOff);
    }
    let Some(known) = known else {
        return Some(RateMissingReason::NotFetchedYet);
    };
    match known.status {
        FxFetchStatus::Ready => None,
        FxFetchStatus::NotPublished => Some(RateMissingReason::NotPublished),
        FxFetchStatus::Failing => Some(RateMissingReason::FetchFailing),
    }
}

impl Ledger {
    /// What the accounts `actor` sees at `balances` come to, period by period. Which of
    /// them it can't see says nothing, and the answer doesn't say there are any.
    ///
    /// # Errors
    ///
    /// [`LedgerError::Denied`] without [`Action::ReadHousehold`]; [`LedgerError::Invalid`]
    /// for a range refused or totals too large to represent; a failure to resolve access;
    /// storage failures.
    pub async fn net_worth(
        &self,
        actor: &Actor,
        query: NetWorthQuery,
    ) -> Result<NetWorthReport, LedgerError> {
        authorize(actor, Action::ReadHousehold)?;
        let scope = self
            .access
            .resolve(actor)
            .await?
            .scope(AccountLevel::Balances);

        let today = self.today();
        let to = query.to.unwrap_or(today);
        validate_as_of(to).map_err(plan_items::invalid)?;
        // No day after today is answered, so that is where a default year ends.
        let answerable = to.min(today);
        let from = match query.from {
            Some(from) => from,
            None => answerable
                .checked_sub(1.year())
                .map_err(|_| refused(NetWorthError::OutOfRange))?
                .max(MIN_VALUE_DATE),
        };
        validate_as_of(from).map_err(plan_items::invalid)?;
        let interval = query.interval.unwrap_or(NetWorthInterval::Month);

        // Refused from the query alone, before any read, so a refusal says nothing of what
        // the table holds. The store is asked up to the same day the report ends on: a
        // table filled past today mustn't move the last period's end.
        period_ends(from, answerable, interval).map_err(refused)?;
        let data = self
            .store
            .net_worth_data(&scope, from, answerable, interval)
            .await?;

        let input = NetWorthInput {
            today,
            from,
            to,
            interval,
            account_points: query.account_points.unwrap_or(true),
            coverage: self.currency_states(&data),
            through: data.state.current_through(),
            rebuild_needed: data.state.rebuild_needed,
            display: data.display,
            stale_after_days: data.stale_after_days,
            first_on: data.first_on,
            rows: data.rows,
            accounts: data
                .accounts
                .iter()
                .map(|record| self.report_account(record))
                .collect(),
        };
        net_worth_report(&input).map_err(refused)
    }

    /// One visible account as a report takes it, its closing in the household's day.
    fn report_account(&self, record: &AccountRecord) -> ReportAccount {
        ReportAccount {
            id: record.id,
            name: record.name.as_str().to_owned(),
            currency: record.currency,
            liquidity: record.liquidity,
            include_in_net_worth: record.include_in_net_worth,
            closed_on: record
                .closed_at
                .map(|at| at.to_zoned(self.timezone.clone()).date()),
            belongs_to: record.belongs_to,
            beneficiary: record
                .beneficiary
                .as_ref()
                .map(|label| label.as_str().to_owned()),
        }
    }

    /// Why rates may be missing, per currency of a visible account or the display
    /// currency. Never answered as such: a report says it only of a visible row.
    fn currency_states(&self, data: &NetWorthData) -> Vec<CurrencyState> {
        let currencies: BTreeSet<Currency> = data
            .accounts
            .iter()
            .map(|record| record.currency)
            .chain(data.display)
            .collect();
        currencies
            .into_iter()
            .map(|currency| CurrencyState {
                currency,
                reason: missing_reason(
                    data.coverage.iter().find(|one| one.currency == currency),
                    self.rate_provider_configured,
                ),
            })
            .collect()
    }

    /// What is known of fetching each currency's rates.
    ///
    /// # Errors
    ///
    /// [`LedgerError::Denied`] without [`Action::ReadHousehold`] and `balances` access to
    /// every account; a failure to resolve access; storage failures.
    pub async fn fx_coverage(&self, actor: &Actor) -> Result<FxCoverageList, LedgerError> {
        self.authorize_fx(actor).await?;
        Ok(FxCoverageList {
            items: self.store.fx_coverage().await?,
        })
    }

    /// The rates `query` asks for.
    ///
    /// # Errors
    ///
    /// As [`Self::fx_coverage`], and [`LedgerError::Invalid`] for a range inverted or too
    /// long.
    pub async fn fx_rates(
        &self,
        actor: &Actor,
        query: FxRatesQuery,
    ) -> Result<FxRateList, LedgerError> {
        self.authorize_fx(actor).await?;
        if query.from > query.to {
            return Err(LedgerError::Invalid(RATE_RANGE_INVERTED));
        }
        let span = query
            .from
            .until(query.to)
            .map_err(|_| LedgerError::Invalid(RATE_RANGE_TOO_LONG))?;
        if span.get_days() >= MAX_RATE_DAYS {
            return Err(LedgerError::Invalid(RATE_RANGE_TOO_LONG));
        }
        Ok(FxRateList {
            currency: query.currency,
            items: self
                .store
                .fx_rates(query.currency, query.from, query.to)
                .await?,
        })
    }

    /// Which rates are stored depends on the currencies in use, so these span every
    /// account (N2); the refusal is the same whatever exists.
    async fn authorize_fx(&self, actor: &Actor) -> Result<(), LedgerError> {
        authorize(actor, Action::ReadHousehold)?;
        self.authorize_every_account_access(actor, AccountLevel::Balances)
            .await
    }
}
