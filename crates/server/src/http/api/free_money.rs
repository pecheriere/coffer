//! `/api/v1/free-money`: the cash nobody has claimed yet, in each currency.

use axum::Json;
use axum::extract::State;
use coffer_domain::forecast::FreeMoney;

use super::openapi::tags;
use crate::http::AppState;
use crate::http::extract::ApiActor;
use crate::http::problem::{CommonProblems, Problem};

/// Free money.
#[utoipa::path(
    get,
    path = "/api/v1/free-money",
    operation_id = "getFreeMoney",
    tag = tags::FORECAST,
    description = "How much cash is free today in each currency: what nothing in the plan, no \
                   goal and no emergency fund has claimed. For each currency, `free_money` is \
                   `cash_available`, less what the plan still needs (`still_need`), less what \
                   goals have set aside so far (`set_aside`), less the emergency fund target \
                   (`emergency_fund_target`), and never below 0. What is left before the target, \
                   between 0 and the target, is `emergency_fund_held`; when the cash doesn't \
                   cover what the plan still needs and goals have set aside, `shortfall` says by \
                   how much.\n\n\
                   `still_need` is what each planned outflow plans less what was spent on it, \
                   never below 0, so an overspent item takes nothing from another. Must pay \
                   items count every occurrence not yet paid through `must_pay_through`, 30 days \
                   on, so a bill due on the 1st is never free on the 30th; essentials and \
                   flexible items count this calendar month only. A bill paid, matched up to 7 \
                   days early or settled (`settlePlanItem`) needs nothing; one dated earlier and \
                   not paid is still needed. Planned income never counts, whatever the forecast \
                   settings say: free money is cash that exists. Investments never count: only \
                   what the forecast counts as cash.\n\n\
                   A currency without an emergency fund target holds nothing back: everything \
                   left counts as free, with the caution `emergency_fund_not_set`. Currencies \
                   are never added together or converted: each has its own row, sorted by \
                   currency. A blocker leaves a currency without a number (`free_money`, \
                   `emergency_fund_held` and `shortfall` are `null`): an account without a \
                   balance blocks its own currency, and no account counting as cash blocks all \
                   of them. `status` is `unavailable` when no currency has a number, \
                   `provisional` with any blocker or caution, and `firm` otherwise; the \
                   cautions are the forecast's, as `getForecast` describes them.\n\n\
                   Needs `read`; a token with account grants also needs `all` at `transactions`, \
                   since the answer sums every account. The refusal is the same whatever exists.",
    security(("bearer" = [])),
    responses(
        (status = 200, description = "Free money, even when a currency has no number: see \
                                      `status`.",
         body = FreeMoney),
        CommonProblems,
    ),
)]
pub(super) async fn get(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
) -> Result<Json<FreeMoney>, Problem> {
    Ok(Json(state.ledger.free_money(&actor).await?))
}
