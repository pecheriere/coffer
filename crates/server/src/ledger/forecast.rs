//! The cash forecast and its what-if (spec §6.8, §8.9): what the store holds, turned
//! into the input of the domain's calculation.
//!
//! Both need `all` at `transactions` before anything is read (R2), so there is no
//! partial answer, and nothing about a hidden account or item can reach a token that
//! can't see it. They write nothing.

use std::collections::{BTreeMap, BTreeSet};

use coffer_domain::forecast::{
    AccountInput, BalanceInput, CategorySpent, FeedAlert, FeedProblem, FeedProblemReason,
    FeedStatus, Forecast, ForecastInput, ItemInput, LineSource, LooseEnds, NewerSpending,
    UnboundAccount, compute_forecast,
};
use coffer_domain::identity::Actor;
use coffer_domain::ids::{AccountId, CategoryId, ConnectionId, PlannedItemId};
use coffer_domain::money::{Currency, Money};
use coffer_domain::plan::{
    CalendarMonth, ForecastSettings, MAX_HYPOTHETICAL_ITEMS, PlanDirection, PlanSchedule,
    PlanSpending, PlannedItemName, SafetyFloor, SettlementKind, strict_date, validate_amount,
    validate_as_of,
};
use coffer_domain::policy::{AccountLevel, Action, authorize};
use coffer_store::{
    CategoryGroupRecord, ConnectionHealth, ConnectionStatus, ForecastData, PlannedItemRecord,
    ProviderAccountRecord, StoreError,
};
use jiff::civil::Date;
use serde::Deserialize;
use utoipa::ToSchema;

use super::plan_items::{self, check_hint, check_item_category, resolved_spending};
use super::settlements::settled_months;
use super::{Ledger, LedgerError, settings};

const TOO_MANY_ITEMS: &str = "a what-if takes at most 50 hypothetical items";
const TOO_LARGE: &str = "the forecast's totals are too large to represent";

/// What a forecast may say differently from the household's settings.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ForecastOptions {
    /// The first day of the window: by default the household's today.
    pub as_of: Option<Date>,
    pub horizon_months: Option<u8>,
    pub include_income: Option<bool>,
    pub floor: Option<SafetyFloor>,
    pub stale_after_days: Option<u16>,
}

/// A planned item to imagine, checked like a stored one but for the optional category.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HypotheticalItem {
    /// 1 to 100 characters, without control characters.
    pub name: String,
    /// An `expense` category for an outflow, an `income` one for an inflow; not
    /// archived, nor in an archived group.
    pub category: Option<CategoryId>,
    /// A positive magnitude, 1 to 10^15 minor units: `direction` says which way the
    /// money goes. For a goal, its target.
    pub amount: Money,
    /// A goal must be an `outflow`.
    pub direction: PlanDirection,
    /// An open account it is expected on.
    pub account_hint: Option<AccountId>,
    pub schedule: PlanSchedule,
    /// Only on a recurring outflow, where it is `bill` by default. This month's spending
    /// reduces no hypothetical item, whichever it is.
    pub spending: Option<PlanSpending>,
}

/// A forecast with other settings, and with items that are not in the plan.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WhatIfRequest {
    /// The first day of the window, written exactly `YYYY-MM-DD`, in the years 1970 to
    /// 2200: by default the household's today.
    #[serde(default, deserialize_with = "strict_date::deserialize_option")]
    pub as_of: Option<Date>,
    /// 1 to 60 months: by default the household's.
    #[serde(default)]
    #[schema(minimum = 1, maximum = 60)]
    pub horizon_months: Option<u8>,
    /// Whether planned inflows reduce the cash needed: by default the household's
    /// setting.
    #[serde(default)]
    pub include_income: Option<bool>,
    /// The safety floor to use: by default the household's.
    #[serde(default)]
    pub floor: Option<SafetyFloor>,
    /// 1 to 365 days: by default the household's.
    #[serde(default)]
    #[schema(minimum = 1, maximum = 365)]
    pub stale_after_days: Option<u16>,
    /// At most 50 items to add, each answering in the breakdown by its position here.
    #[serde(default)]
    #[schema(max_items = 50)]
    pub items: Vec<HypotheticalItem>,
}

impl Ledger {
    /// Whether `actor` may have a forecast: it sums every account (R2), so an `all`
    /// grant at `transactions`, decided before anything is read.
    pub(super) async fn authorize_forecast(&self, actor: &Actor) -> Result<(), LedgerError> {
        authorize(actor, Action::ReadHousehold)?;
        self.authorize_every_account_access(actor, AccountLevel::Transactions)
            .await
    }

    /// The forecast, from the stored plan and the household's settings unless `options`
    /// says otherwise.
    ///
    /// # Errors
    ///
    /// [`LedgerError::Denied`] without [`Action::ReadHousehold`] and `transactions`
    /// access to every account; [`LedgerError::Invalid`] for an option out of range or
    /// totals too large to represent; storage failures.
    pub async fn forecast(
        &self,
        actor: &Actor,
        options: ForecastOptions,
    ) -> Result<Forecast, LedgerError> {
        self.authorize_forecast(actor).await?;
        self.answer(options, Vec::new()).await
    }

    /// The forecast with what `request` imagines. Writes nothing.
    ///
    /// # Errors
    ///
    /// As [`Self::forecast`], and [`LedgerError::Invalid`] for too many items or an
    /// item that could not be stored.
    pub async fn what_if(
        &self,
        actor: &Actor,
        request: WhatIfRequest,
    ) -> Result<Forecast, LedgerError> {
        self.authorize_forecast(actor).await?;
        if request.items.len() > MAX_HYPOTHETICAL_ITEMS {
            return Err(LedgerError::Invalid(TOO_MANY_ITEMS));
        }
        let items = self.checked_hypothetical_items(request.items).await?;
        let options = ForecastOptions {
            as_of: request.as_of,
            horizon_months: request.horizon_months,
            include_income: request.include_income,
            floor: request.floor,
            stale_after_days: request.stale_after_days,
        };
        self.answer(options, items).await
    }

    /// `items`, each checked as a stored item would be.
    async fn checked_hypothetical_items(
        &self,
        items: Vec<HypotheticalItem>,
    ) -> Result<Vec<ItemInput>, LedgerError> {
        let groups = if items.iter().any(|item| item.category.is_some()) {
            self.store.list_categories().await?
        } else {
            Vec::new()
        };
        let mut checked = Vec::new();
        for (index, item) in (0_u32..).zip(items) {
            let name = PlannedItemName::parse(&item.name).map_err(plan_items::invalid)?;
            validate_amount(item.amount).map_err(plan_items::invalid)?;
            item.schedule
                .validate(item.direction)
                .map_err(plan_items::invalid)?;
            let spending = resolved_spending(
                item.spending,
                PlanSpending::Bill,
                item.direction,
                &item.schedule,
            )?;
            if let Some(category) = item.category {
                check_hypothetical_category(&groups, category, item.direction)?;
            }
            if let Some(account) = item.account_hint {
                check_hint(self.store.get_account(account).await)?;
            }
            checked.push(ItemInput {
                source: LineSource::Hypothetical { index },
                name: name.as_str().to_owned(),
                category: item.category,
                amount: item.amount,
                direction: item.direction,
                account_hint: item.account_hint,
                schedule: item.schedule,
                spending,
                goal_spent_minor: 0,
                goal_linked_both_sides: false,
                settled: BTreeMap::new(),
            });
        }
        Ok(checked)
    }

    /// The forecast of the stored data and `hypothetical`, for an actor already
    /// authorized.
    async fn answer(
        &self,
        options: ForecastOptions,
        hypothetical: Vec<ItemInput>,
    ) -> Result<Forecast, LedgerError> {
        let today = self.today();
        let as_of = options.as_of.unwrap_or(today);
        validate_as_of(as_of).map_err(plan_items::invalid)?;
        let data = self
            .store
            .forecast_data(as_of)
            .await
            .map_err(|err| match err {
                StoreError::SumTooLarge => LedgerError::Invalid(TOO_LARGE),
                other => other.into(),
            })?;
        let chosen = ForecastSettings {
            floor: options.floor.or(data.settings.floor),
            include_income: options
                .include_income
                .unwrap_or(data.settings.include_income),
            default_horizon_months: options
                .horizon_months
                .unwrap_or(data.settings.default_horizon_months),
            stale_after_days: options
                .stale_after_days
                .unwrap_or(data.settings.stale_after_days),
        };
        chosen.validate().map_err(settings::invalid)?;
        compute_forecast(&input(today, as_of, &chosen, data, hypothetical))
            .map_err(|_| LedgerError::Invalid(TOO_LARGE))
    }
}

/// `Ok` when `category` is one an item going in `direction` may use.
fn check_hypothetical_category(
    groups: &[CategoryGroupRecord],
    category: CategoryId,
    direction: PlanDirection,
) -> Result<(), LedgerError> {
    for group in groups {
        if let Some(found) = group.categories.iter().find(|one| one.id == category) {
            return check_item_category(found, group.archived_at.is_some(), direction);
        }
    }
    Err(LedgerError::Invalid(plan_items::UNKNOWN_CATEGORY))
}

/// The calculation's input: only what it needs, so provider error text and sealed
/// data can't reach an answer.
fn input(
    today: Date,
    as_of: Date,
    chosen: &ForecastSettings,
    data: ForecastData,
    hypothetical: Vec<ItemInput>,
) -> ForecastInput {
    let ForecastData {
        accounts,
        items,
        settlements,
        goal_spent,
        goals_linked_both_sides,
        goal_spending_accounts,
        category_spent,
        loose_ends,
        newer_spending,
        provider_accounts,
        connections,
        ..
    } = data;
    let currencies: BTreeMap<AccountId, Currency> = accounts
        .iter()
        .map(|(account, _)| (account.id, account.currency))
        .collect();
    let removed: BTreeSet<ConnectionId> = connections
        .iter()
        .filter(|connection| connection.status == ConnectionStatus::Removed)
        .map(|connection| connection.id)
        .collect();
    let feeds = feed_alerts(connections, &provider_accounts);
    let (mut problems, unbound) = provider_view(provider_accounts, &currencies, &removed);
    let goal_spent: BTreeMap<PlannedItemId, i64> = goal_spent.into_iter().collect();
    let goal_spending: BTreeSet<AccountId> = goal_spending_accounts.into_iter().collect();
    let both_sides: BTreeSet<PlannedItemId> = goals_linked_both_sides.into_iter().collect();
    let mut settled = settled_months(&settlements);
    ForecastInput {
        today,
        as_of,
        horizon_months: chosen.default_horizon_months,
        include_income: chosen.include_income,
        floor: chosen.floor,
        stale_after_days: chosen.stale_after_days,
        accounts: accounts
            .into_iter()
            .map(|(account, balance)| AccountInput {
                id: account.id,
                name: account.name.as_str().to_owned(),
                currency: account.currency,
                liquidity: account.liquidity,
                belongs_to: account.belongs_to,
                include_in_forecast: account.include_in_forecast,
                closed: account.closed_at.is_some(),
                balance: balance.map(|snapshot| BalanceInput {
                    on: snapshot.on,
                    minor_units: snapshot.balance_minor,
                }),
                feed_problems: problems.remove(&account.id).unwrap_or_default(),
                goal_spending: goal_spending.contains(&account.id),
            })
            .collect(),
        items: items
            .iter()
            .map(|item| {
                let months = settled.remove(&item.id).unwrap_or_default();
                stored_item(item, &goal_spent, &both_sides, months)
            })
            .chain(hypothetical)
            .collect(),
        category_spent: category_spent
            .into_iter()
            .map(|row| CategorySpent {
                category: row.category,
                currency: row.currency,
                spent_minor: row.spent_minor,
            })
            .collect(),
        loose_ends: loose_ends
            .into_iter()
            .map(|row| LooseEnds {
                currency: row.currency,
                uncategorized_count: row.uncategorized_count,
                uncategorized_minor: row.uncategorized_minor,
                possible_duplicates: row.possible_duplicates,
            })
            .collect(),
        newer_spending: newer_spending
            .into_iter()
            .map(|row| NewerSpending {
                currency: row.currency,
                total_minor: row.total_minor,
                pending_minor: row.pending_minor,
            })
            .collect(),
        unbound,
        feeds,
    }
}

/// What the provider accounts say: the problems of those bound to an account, and
/// those bound to none. Those of a removed connection are skipped everywhere.
fn provider_view(
    provider_accounts: Vec<ProviderAccountRecord>,
    currencies: &BTreeMap<AccountId, Currency>,
    removed: &BTreeSet<ConnectionId>,
) -> (BTreeMap<AccountId, Vec<FeedProblem>>, Vec<UnboundAccount>) {
    let mut problems: BTreeMap<AccountId, Vec<FeedProblem>> = BTreeMap::new();
    let mut unbound = Vec::new();
    for record in provider_accounts
        .into_iter()
        .filter(|record| !removed.contains(&record.connection))
    {
        match record.account {
            Some(account) => {
                let expected = currencies.get(&account).copied();
                problems
                    .entry(account)
                    .or_default()
                    .extend(feed_problems(&record, expected));
            }
            // A dismissed account is left unbound on purpose: no caution.
            None if record.dismissed_at.is_some() => {}
            None => unbound.push(UnboundAccount {
                provider_account: record.id,
                connection: record.connection,
                name: record.name,
                institution: record.institution,
                currency_raw: record.currency_raw,
            }),
        }
    }
    (problems, unbound)
}

/// The alerts of the connections that aren't syncing, each with the accounts it feeds.
fn feed_alerts(
    connections: Vec<ConnectionHealth>,
    provider_accounts: &[ProviderAccountRecord],
) -> Vec<FeedAlert> {
    let mut serves: BTreeMap<ConnectionId, Vec<AccountId>> = BTreeMap::new();
    let mut with_accounts = BTreeSet::new();
    for record in provider_accounts {
        with_accounts.insert(record.connection);
        if let Some(account) = record.account {
            serves.entry(record.connection).or_default().push(account);
        }
    }
    connections
        .into_iter()
        .filter_map(|connection| {
            let serves = serves.remove(&connection.id).unwrap_or_default();
            let has_accounts = with_accounts.contains(&connection.id);
            feed_alert(connection, serves, has_accounts)
        })
        .collect()
}

/// The alert of a connection that isn't syncing.
fn feed_alert(
    connection: ConnectionHealth,
    serves: Vec<AccountId>,
    has_accounts: bool,
) -> Option<FeedAlert> {
    let status = match connection.status {
        ConnectionStatus::Error => FeedStatus::Error,
        ConnectionStatus::ReauthRequired => FeedStatus::ReauthRequired,
        ConnectionStatus::Active | ConnectionStatus::Removed => return None,
    };
    Some(FeedAlert {
        connection: connection.id,
        label: connection.label,
        status,
        serves,
        has_accounts,
    })
}

/// What may be wrong with a bound provider account: only the reason, never the
/// provider's own text. `expected` is the currency of the account it is bound to.
fn feed_problems(record: &ProviderAccountRecord, expected: Option<Currency>) -> Vec<FeedProblem> {
    let provider_account = record.id;
    let mut found = Vec::new();
    if record.error.is_some() {
        found.push(FeedProblem {
            provider_account,
            reason: FeedProblemReason::ProviderError,
        });
    }
    match Currency::parse(&record.currency_raw) {
        Err(_) => found.push(FeedProblem {
            provider_account,
            reason: FeedProblemReason::CurrencyUnsupported,
        }),
        Ok(currency) if Some(currency) != expected => found.push(FeedProblem {
            provider_account,
            reason: FeedProblemReason::CurrencyDiffers,
        }),
        Ok(_) => {}
    }
    found
}

fn stored_item(
    item: &PlannedItemRecord,
    goal_spent: &BTreeMap<PlannedItemId, i64>,
    both_sides: &BTreeSet<PlannedItemId>,
    settled: BTreeMap<CalendarMonth, SettlementKind>,
) -> ItemInput {
    ItemInput {
        source: LineSource::PlannedItem { id: item.id },
        name: item.name.as_str().to_owned(),
        category: Some(item.category),
        amount: item.amount,
        direction: item.direction,
        account_hint: item.account_hint,
        schedule: item.schedule,
        spending: item.spending,
        goal_spent_minor: goal_spent.get(&item.id).copied().unwrap_or(0),
        goal_linked_both_sides: both_sides.contains(&item.id),
        settled,
    }
}
