//! `/api/v1/fx`: the exchange rates stored and what is known of fetching them.

use axum::Json;
use axum::extract::State;

use super::extract::TypedQuery;
use super::openapi::tags;
use crate::http::AppState;
use crate::http::extract::ApiActor;
use crate::http::problem::{CommonProblems, Problem};
use crate::ledger::net_worth::{FxCoverageList, FxRateList, FxRatesQuery};

/// What is known of fetching each currency's rates.
#[utoipa::path(
    get,
    path = "/api/v1/fx/coverage",
    operation_id = "getFxCoverage",
    tag = tags::FX,
    description = "For each currency a fetch was tried for: `status` (`ready`, `not_published` \
                   or `failing`), the `first_on` and `last_on` of the rates held, when it was last \
                   tried and last succeeded, and `last_error`, a fixed code (`unavailable`, \
                   `malformed` or `network`). A currency never tried has no entry.\n\n\
                   Needs `read`; a token with account grants also needs `all` at `balances`, since \
                   which rates are stored depends on the currencies in use. The refusal is the \
                   same whatever exists.",
    security(("bearer" = [])),
    responses(
        (status = 200, description = "What is known, by currency.", body = FxCoverageList),
        CommonProblems,
    ),
)]
pub(super) async fn coverage(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
) -> Result<Json<FxCoverageList>, Problem> {
    Ok(Json(state.ledger.fx_coverage(&actor).await?))
}

/// One currency's stored rates.
#[utoipa::path(
    get,
    path = "/api/v1/fx/rates",
    operation_id = "getFxRates",
    tag = tags::FX,
    description = "A currency's published rates in a range of at most 1,000 days, newest first: \
                   `rate` is units of the currency per one euro, as exact decimal text. A weekend \
                   or holiday has no entry, and a report uses the most recent earlier rate. The \
                   euro has no entries, being 1 on every day. A range too long or inverted is 422; \
                   an unknown code or a date not written `YYYY-MM-DD` is 400.\n\n\
                   Needs `read`; a token with account grants also needs `all` at `balances`, since \
                   which rates are stored depends on the currencies in use. The refusal is the \
                   same whatever exists.",
    security(("bearer" = [])),
    params(FxRatesQuery),
    responses(
        (status = 200, description = "The rates.", body = FxRateList),
        CommonProblems,
    ),
)]
pub(super) async fn rates(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedQuery(query): TypedQuery<FxRatesQuery>,
) -> Result<Json<FxRateList>, Problem> {
    Ok(Json(state.ledger.fx_rates(&actor, query).await?))
}
