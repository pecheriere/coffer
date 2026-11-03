//! `/api/v1/reports`: what happened against what was planned, and net worth over time.

use axum::Json;
use axum::extract::State;
use coffer_domain::net_worth::NetWorthReport;
use coffer_domain::plan_report::PlanReport;

use super::extract::TypedQuery;
use super::openapi::tags;
use crate::http::AppState;
use crate::http::extract::ApiActor;
use crate::http::problem::{CommonProblems, Problem};
use crate::ledger::net_worth::NetWorthQuery;
use crate::ledger::plan_report::PlanReportQuery;

/// Plan versus actual.
#[utoipa::path(
    get,
    path = "/api/v1/reports/plan-vs-actual",
    operation_id = "getPlanVsActual",
    tag = tags::REPORTS,
    description = "Per category and month, what the plan says against what happened. `planned` \
                   is the occurrences of today's active items falling in the month, not spread \
                   across months, and the plan has no history, so changing an item rewrites past \
                   months. `actual` is posted splits on any account: positive for spending in an \
                   `expense` category and for income in an `income` one, net of refunds, so it \
                   can be negative. `transfer` categories don't count. `difference` is `planned` \
                   minus `actual`, and `rolling_average` the mean of `actual` over the \
                   `average_months` calendar months ending at that month, zeros included.\n\n\
                   Money that isn't in `actual` is still reported, at what its splits say, not the \
                   transaction's revised amount. A charge, or a deposit in an \
                   `income` category, whose provider removed it or revised it away from its \
                   splits stays out of `actual`, with the amount in `left_out`; a refund on such \
                   a transaction keeps counting. A possible duplicate stays in `actual`, with the \
                   amount in `possible_duplicate`. A split linked to a goal counts only on that \
                   goal's line in `goals`, as `spent_against` beside what its schedule `accrued`, \
                   and a removed, revised or possibly duplicate charge there is in that line's \
                   `left_out`. Outflows with no splits are never spending: `uncategorized` lists \
                   them, without invested and illiquid accounts. Both legs of a transfer link \
                   between two of the household's accounts are out of all of these, whatever they \
                   are filed as: `moved_within_household` says what left and what arrived, per \
                   month and currency, so a fee shows as the two differing. A link out of the \
                   household changes nothing.\n\n\
                   An occurrence a person settled (`settlePlanItem`) stays in `planned`, since it \
                   is what was planned: a row's `settled` says how much of `planned` it is, and \
                   the list `settled` says which item, for which month, and whether `paid` or \
                   `skipped`. A settled occurrence takes no share of the spending in `rollovers`, \
                   since that spending wasn't for it.\n\n\
                   `rollovers` has a line for each category and currency with a budget: what was \
                   `budgeted` over the reported months up to today's, what they `spent` on it while \
                   a budget was running, once bills and one-offs dated up to today have taken \
                   their share, and the `rollover` between them, positive for unspent and negative \
                   for overspent. It is a window over those months, not a running balance, so \
                   moving `from` changes it, and today's month counts all its budget occurrences. \
                   A bill has none, and the forecast doesn't carry it over. `partial_months` names \
                   the reported month holding today, whose figures aren't final. Amounts are \
                   integer minor units, and currencies are never netted.\n\n\
                   Needs `read`; a token with account grants also needs `all` at `transactions`, \
                   since the answer sums every account. The refusal is the same whatever exists.",
    security(("bearer" = [])),
    params(PlanReportQuery),
    responses(
        (status = 200, description = "The report.", body = PlanReport),
        CommonProblems,
    ),
)]
pub(super) async fn plan_vs_actual(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedQuery(query): TypedQuery<PlanReportQuery>,
) -> Result<Json<PlanReport>, Problem> {
    Ok(Json(state.ledger.plan_vs_actual(&actor, query).await?))
}

/// Net worth over time.
#[utoipa::path(
    get,
    path = "/api/v1/reports/net-worth",
    operation_id = "getNetWorth",
    tag = tags::REPORTS,
    description = "What the household is worth, period by period, in the display currency.\n\n\
                   A point is the value on the last day of its period, or on the last day \
                   available when it says `partial`: `on` is that day and `period_start` the first \
                   day of the period inside the range. A week ends on Sunday. Each account's \
                   latest balance on or before `on` counts, so a balance carries forward until a \
                   newer one. A period where nothing counts is left out, and at most 1,000 periods \
                   are answered: a longer range is 422, whatever is stored. `through` is the last \
                   day with values: no later day, and no day after today, is a point. It is \
                   `null` while the values are being rebuilt.\n\n\
                   `total` is the sum of the counted accounts' amounts, each converted and rounded \
                   on its own, so every breakdown adds up to it. It is `null`, never a partial sum, \
                   when one of them has no amount: `unconverted` names the accounts, and a caution \
                   says why. A `by_liquidity` line is `null` the same way. `by_currency` is the \
                   native sum per currency, with no rate involved, so nothing is omitted; only an \
                   account with no value that day can't be in it.\n\n\
                   In `accounts`, each account that is the household's says whether it counts, its \
                   `first_on` (a jump that \
                   day is an account arriving, not money) and its `closed_on` (a closed account \
                   stops counting that day unless it still owes). Its `points` say the `balance` in \
                   its own currency, `balance_on`, the date of the snapshot that balance comes \
                   from, `balance_source`, the `amount` in the display currency and `rate_on`, the \
                   older of the dates of the rates used: a weekend reports Friday's.\n\n\
                   `status` says how far to trust it. `firm` is numbers with nothing to warn about. \
                   `provisional` is numbers with `cautions`. `unavailable` is `blockers`: \
                   `display_currency_not_set` (an admin has to choose one, and points still hold \
                   `by_currency`), `rebuilding` (no points until the values are rebuilt) or \
                   `no_counted_accounts` (not a net worth of zero).\n\n\
                   The cautions: `rate_missing`, with a reason of `not_fetched_yet`, \
                   `not_published`, `fetch_failing`, `provider_off` or `none_that_early`; \
                   `rate_stale`, a rate more than a week older than its day, as for a discontinued \
                   currency; `amount_too_large`, a conversion that doesn't fit; `balance_stale`, \
                   an old balance on the latest point; `to_date_not_yet_filled`, later days not \
                   answered yet; `daily_value_missing`, an account with no value on a day after \
                   its first, which is a hole and not money leaving; \
                   `nothing_counted_after_last_point`, so the last point isn't the value now; \
                   `liability_balance_positive`, perhaps a wrong sign, added as it stands; and \
                   `closed_account_owing`, a debt still counting.\n\n\
                   `buckets` holds the money that isn't the household's, in no number above: **nothing \
                   adds a bucket to `total`, or one bucket to another**, and nothing about one \
                   touches the household's `status`, `points`, `accounts`, `blockers` or `cautions`. \
                   There is one per `belongs_to` and exact `beneficiary`, so two spellings of a name \
                   are two buckets: `someone_else` before `given_away`, and within one no label \
                   (`beneficiary` is `null`) first, then labels by their bytes. Each is the same \
                   calculation over its own accounts, listed only there, all of them `counted` in \
                   that bucket, with its own `status`, `points`, `accounts`, `blockers` and \
                   `cautions`. There, `no_counted_accounts` means nothing in it has a value in this \
                   range. It answers from balances as they stand and whose money an account is now: \
                   moving an account moves its whole history. While `rebuilding`, `buckets` is \
                   empty.\n\n\
                   Needs `read`. **A token with account grants gets the sum of the accounts it can \
                   see at `balances`, and nothing marks it as partial**, since that would reveal \
                   that there are others. An agent holding such a token must not report the sum as \
                   the whole household's. A bucket is made from those same accounts and sums them, \
                   unmarked: holding one of a child's two accounts, it sees a bucket of one. Nothing \
                   in the answer depends on an account the token \
                   can't see, except that rates are stored once for the household: a currency the \
                   token sees has its rates as soon as anyone's use fetched them. A sum too large to \
                   represent, in the household's money or a bucket's the token sees, refuses the \
                   whole report with 422. Amounts are integer minor units. It writes nothing.",
    security(("bearer" = [])),
    params(NetWorthQuery),
    responses(
        (status = 200, description = "The report.", body = NetWorthReport),
        CommonProblems,
    ),
)]
pub(super) async fn net_worth(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedQuery(query): TypedQuery<NetWorthQuery>,
) -> Result<Json<NetWorthReport>, Problem> {
    Ok(Json(state.ledger.net_worth(&actor, query).await?))
}
