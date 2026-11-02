//! `/api/v1/forecast`: how much cash the household needs and what is free above it.

use axum::Json;
use axum::extract::State;
use coffer_domain::forecast::Forecast;
use coffer_domain::plan::strict_date;
use jiff::civil::Date;
use serde::Deserialize;
use utoipa::IntoParams;

use super::extract::{TypedJson, TypedQuery};
use super::openapi::tags;
use crate::http::AppState;
use crate::http::extract::ApiActor;
use crate::http::problem::{CommonProblems, Problem};
use crate::ledger::forecast::{ForecastOptions, WhatIfRequest};

/// What a forecast may say differently from the household's settings.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct ForecastQuery {
    /// The first day of the window, written exactly `YYYY-MM-DD`, in the years 1970 to
    /// 2200: by default the household's today. A balance is the latest on or before
    /// it. Any other day than today raises the caution `as_of_not_today`, since bills
    /// between today and that day fall outside the window.
    #[serde(default, deserialize_with = "strict_date::deserialize_option")]
    pub as_of: Option<Date>,
    /// How many months the window covers, by default the household's setting.
    #[param(minimum = 1, maximum = 60)]
    pub horizon_months: Option<u8>,
    /// Whether planned inflows reduce the cash needed, by default the household's
    /// setting.
    pub include_income: Option<bool>,
}

/// The cash forecast.
#[utoipa::path(
    get,
    path = "/api/v1/forecast",
    operation_id = "getForecast",
    tag = tags::FORECAST,
    description = "How much cash the household needs over the window, and how much above that \
                   could be invested: `surplus` is `cash_available` minus `cash_needed`, and \
                   `cash_needed` is the planned outflows, plus the goals, plus the safety floor, \
                   minus any income counted.\n\n\
                   `status` says how far to trust it. `firm` is a number with nothing to warn \
                   about. `provisional` is a number with `cautions`, each saying what might \
                   change it. `unavailable` is no number: `surplus` is `null` and `blockers` say \
                   why, such as a safety floor not yet set or an account without a balance. \
                   Several currencies are never netted: `surplus` is `null`, with the blocker \
                   `mixed_currencies`, and each row of `totals` has its own.\n\n\
                   A planned amount is a positive magnitude with a `direction`. Balances, \
                   `cash_available`, `projected_cash` and `surplus` carry a sign: a card owing \
                   money is negative, and a negative `surplus` is a shortfall. Amounts are \
                   integer minor units.\n\n\
                   Every number traces to its lines: `accounts` says which balances counted, \
                   `items` what each planned item added, `months` the same by calendar month. \
                   This month's spending counts only where a balance already shows it: posted, \
                   not pending, on or before its account's balance date.\n\n\
                   Needs `read`; a token with account grants also needs `all` at `transactions`, \
                   since the answer sums every account. The refusal is the same whatever exists.",
    security(("bearer" = [])),
    params(ForecastQuery),
    responses(
        (status = 200, description = "The forecast, even when it has no number: see `status`.",
         body = Forecast),
        CommonProblems,
    ),
)]
pub(super) async fn get(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedQuery(query): TypedQuery<ForecastQuery>,
) -> Result<Json<Forecast>, Problem> {
    let options = ForecastOptions {
        as_of: query.as_of,
        horizon_months: query.horizon_months,
        include_income: query.include_income,
        ..ForecastOptions::default()
    };
    Ok(Json(state.ledger.forecast(&actor, options).await?))
}

/// The cash forecast with imagined changes.
#[utoipa::path(
    post,
    path = "/api/v1/forecast/what-if",
    operation_id = "whatIfForecast",
    tag = tags::FORECAST,
    description = "The answer of `getForecast` with other settings and with items that are not \
                   in the plan, such as a new roof: `items` are checked like planned items, but \
                   a category is optional, and each answers in the breakdown as `hypothetical` \
                   with its position in the list. This month's spending neither reduces nor pays \
                   them.\n\n\
                   This persists nothing: no item, no setting, no audit entry, no idempotency \
                   key. So it needs only `read`, though it is a `POST`, and the same access as \
                   `getForecast`.",
    security(("bearer" = [])),
    request_body = WhatIfRequest,
    responses(
        (status = 200, description = "The forecast, even when it has no number: see `status`.",
         body = Forecast),
        CommonProblems,
    ),
)]
pub(super) async fn what_if(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedJson(request): TypedJson<WhatIfRequest>,
) -> Result<Json<Forecast>, Problem> {
    Ok(Json(state.ledger.what_if(&actor, request).await?))
}
