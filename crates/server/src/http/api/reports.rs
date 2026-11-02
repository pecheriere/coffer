//! `/api/v1/reports`: what happened against what was planned.

use axum::Json;
use axum::extract::State;
use coffer_domain::plan_report::PlanReport;

use super::extract::TypedQuery;
use super::openapi::tags;
use crate::http::AppState;
use crate::http::extract::ApiActor;
use crate::http::problem::{CommonProblems, Problem};
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
                   them, without invested and illiquid accounts.\n\n\
                   An occurrence a person settled (`settlePlanItem`) stays in `planned`, since it \
                   is what was planned: a row's `settled` says how much of `planned` it is, and \
                   the list `settled` says which item, for which month, and whether `paid` or \
                   `skipped`.\n\n\
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
