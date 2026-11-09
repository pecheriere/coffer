//! What makes a forecast provisional (spec §8.9): the cautions about accounts, items and
//! loose ends that the number is only as good as.

use std::collections::BTreeSet;

use jiff::Span;
use jiff::civil::Date;

use super::lines::{not_counted, read_by_number};
use super::reduction::Reduction;
use super::settled;
use super::{AccountInput, Caution, ForecastError, ForecastInput, HintEffect, ItemLine, money};
use crate::accounts::LiquidityClass;
use crate::ids::AccountId;
use crate::money::{Currency, Money};
use crate::plan::{PlanDirection, PlanSchedule};
use crate::transfer_rate::mismatches;

pub(super) fn cautions(
    input: &ForecastInput,
    items: &[ItemLine],
    reduction: &Reduction,
    fund_cautions: Vec<Caution>,
    counted: &BTreeSet<AccountId>,
) -> Result<Vec<Caution>, ForecastError> {
    let mut cautions = Vec::new();
    if input.as_of != input.today {
        cautions.push(Caution::AsOfNotToday { today: input.today });
    }
    for account in input.accounts.iter().filter(|a| not_counted(a).is_none()) {
        account_cautions(account, input, &mut cautions);
    }
    cautions.extend(
        input
            .unbound
            .iter()
            .map(|unbound| Caution::UnboundProviderAccount {
                provider_account: unbound.provider_account,
                connection: unbound.connection,
                name: unbound.name.clone(),
                institution: unbound.institution.clone(),
                currency_raw: unbound.currency_raw.clone(),
                currency_supported: Currency::parse(&unbound.currency_raw).is_ok(),
            }),
    );
    // Only a connection that could change the number: one that has no provider account yet
    // (a first sync that failed), one with provider accounts still unbound, or one serving an
    // account the number reads. The rest show as failing where connections are listed.
    let read: BTreeSet<AccountId> = input
        .accounts
        .iter()
        .filter(|account| read_by_number(account))
        .map(|account| account.id)
        .collect();
    cautions.extend(
        input
            .feeds
            .iter()
            .filter(|feed| {
                !feed.has_accounts
                    || feed.serves.iter().any(|id| read.contains(id))
                    || input
                        .unbound
                        .iter()
                        .any(|unbound| unbound.connection == feed.connection)
            })
            .map(|feed| Caution::ConnectionNeedsAttention {
                connection: feed.connection,
                label: feed.label.clone(),
                status: feed.status,
            }),
    );

    // An item with a history has a line per version, and its cautions are about the item.
    let mut about_items = Vec::new();
    for line in items {
        item_cautions(line, input, counted, &mut about_items);
    }
    for caution in about_items {
        if !cautions.contains(&caution) {
            cautions.push(caution);
        }
    }
    // From the items and not their lines: a goal the links have fully paid has no line, which
    // is when they may have overstated it most.
    cautions.extend(
        input
            .items
            .iter()
            .filter(|item| {
                item.goal_linked_both_sides
                    && matches!(
                        item.schedule,
                        PlanSchedule::Goal {
                            completed: false,
                            ..
                        }
                    )
            })
            .map(|item| Caution::GoalLinkedBothSides { goal: item.source }),
    );
    for (&(category, currency), gap) in &reduction.not_seen {
        cautions.push(Caution::BillNotSeen {
            category,
            amount: money(gap.amount, currency)?,
            items: gap.items.clone(),
        });
    }
    cautions.extend(reduction.bill_cautions.iter().cloned());
    cautions.extend(fund_cautions);
    loose_end_cautions(input, reduction, &mut cautions);
    cautions.extend(
        input
            .newer_spending
            .iter()
            .filter(|newer| newer.total_minor > 0 || newer.pending_minor > 0)
            .map(|newer| Caution::SpendingNewerThanBalance {
                currency: newer.currency,
                total: Money {
                    minor_units: newer.total_minor,
                    currency: newer.currency,
                },
                pending: Money {
                    minor_units: newer.pending_minor,
                    currency: newer.currency,
                },
            }),
    );
    Ok(cautions)
}

fn account_cautions(account: &AccountInput, input: &ForecastInput, cautions: &mut Vec<Caution>) {
    if let Some(balance) = account.balance {
        if account.closed {
            cautions.push(Caution::ClosedLiabilityOwing {
                account: account.id,
                balance: Money {
                    minor_units: balance.minor_units,
                    currency: account.currency,
                },
            });
        }
        // At the day the forecast describes, or today if that is yet to come, since a
        // balance can't be fresher than today. A date after it has no age.
        let age_days = balance
            .on
            .until(input.as_of.min(input.today))
            .map_or(0, |age| age.get_days());
        if let Ok(age_days) = u32::try_from(age_days)
            && age_days > u32::from(input.stale_after_days)
        {
            cautions.push(Caution::BalanceStale {
                account: account.id,
                on: balance.on,
                age_days,
            });
        }
        if account.liquidity == LiquidityClass::ShortTermLiability && balance.minor_units > 0 {
            cautions.push(Caution::LiabilityBalancePositive {
                account: account.id,
                balance: Money {
                    minor_units: balance.minor_units,
                    currency: account.currency,
                },
            });
        }
    }
    cautions.extend(
        account
            .feed_problems
            .iter()
            .map(|problem| Caution::AccountFeedProblem {
                account: account.id,
                provider_account: problem.provider_account,
                reason: problem.reason,
            }),
    );
}

/// A hint at an uncounted account matters only if the item could reach the number,
/// which an inflow does not with income off. A goal listed here counts something,
/// so a finished or fully spent one raises nothing.
fn item_cautions(
    line: &ItemLine,
    input: &ForecastInput,
    counted: &BTreeSet<AccountId>,
    cautions: &mut Vec<Caution>,
) {
    let only_shown = line.gross.minor_units == 0 && line.unpaid.minor_units == 0;
    let in_the_number =
        !only_shown && (line.direction == PlanDirection::Outflow || input.include_income);
    if let Some(account) = line.account_hint
        && in_the_number
        && !counted.contains(&account)
    {
        cautions.push(Caution::ItemHintNotCounted {
            item: line.source,
            account,
            effect: if line.direction == PlanDirection::Outflow {
                HintEffect::StillNeeded
            } else {
                HintEffect::LeftOut
            },
        });
    }
    let Some(goal) = line.goal else {
        return;
    };
    if goal.due_on < input.as_of {
        cautions.push(Caution::GoalOverdue {
            goal: line.source,
            due_on: goal.due_on,
        });
    }
    if let Some(category) = line.category {
        let month = Span::new().months(1);
        let earliest = goal.due_on.checked_sub(month).unwrap_or(Date::MIN);
        let latest = goal.due_on.checked_add(month).unwrap_or(Date::MAX);
        for item in &input.items {
            if let PlanSchedule::OneOff { on } = item.schedule
                && item.direction == PlanDirection::Outflow
                && item.category == Some(category)
                && (earliest..=latest).contains(&on)
                && !settled::is_settled(item, on)
            {
                cautions.push(Caution::GoalOneOffOverlap {
                    goal: line.source,
                    one_off: item.source,
                    category,
                });
            }
        }
    }
}

/// This month's unsettled spending only matters where it could still change the
/// number. Uncategorized money is more spending, which only helps a budget or closes
/// a gap. A duplicate is spending that never happened, which pays no bill, but can shrink a
/// budget.
fn loose_end_cautions(input: &ForecastInput, reduction: &Reduction, cautions: &mut Vec<Caution>) {
    let mut possible_duplicates = 0_u32;
    for loose in &input.loose_ends {
        let more_spending_matters = reduction
            .spending_sensitive_currencies
            .contains(&loose.currency);
        if more_spending_matters {
            possible_duplicates = possible_duplicates.saturating_add(loose.possible_duplicates);
        }
        if more_spending_matters && loose.uncategorized_count > 0 {
            cautions.push(Caution::UncategorizedSpending {
                currency: loose.currency,
                count: loose.uncategorized_count,
                total: Money {
                    minor_units: loose.uncategorized_minor,
                    currency: loose.currency,
                },
            });
        }
    }
    if possible_duplicates > 0 {
        cautions.push(Caution::PossibleDuplicates {
            count: possible_duplicates,
        });
    }
    cautions.extend(
        input
            .last_month_loose_ends
            .iter()
            .filter(|loose| {
                reduction
                    .last_month_unpaid_currencies
                    .contains(&loose.currency)
            })
            .map(|loose| Caution::LastMonthLooseEnds {
                currency: loose.currency,
                uncategorized_count: loose.uncategorized_count,
                uncategorized_total: Money {
                    minor_units: loose.uncategorized_minor,
                    currency: loose.currency,
                },
                possible_duplicates: loose.possible_duplicates,
            }),
    );
    cautions.extend(
        input
            .links_with_leg_removed
            .iter()
            .map(|&link| Caution::TransferLegRemoved { link }),
    );
    cautions.extend(
        mismatches(&input.cross_currency_links, &input.rates)
            .into_iter()
            .map(|found| Caution::TransferLinkRateMismatch {
                link: found.link,
                outflow: found.mismatch.outflow,
                inflow: found.mismatch.inflow,
                expected_inflow: found.mismatch.expected_inflow,
                rate_on: found.mismatch.rate_on,
                manual_rate: found.mismatch.manual_rate,
            }),
    );
}
