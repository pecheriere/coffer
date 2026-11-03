//! `/api/v1/fx`: the exchange rates stored, what is known of fetching them, and the rates
//! admins enter.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use coffer_domain::fx::{FxRate, ManualRate};
use coffer_domain::money::Currency;
use coffer_domain::plan::strict_date;
use jiff::civil::Date;
use serde::Deserialize;
use utoipa::{IntoParams, ToSchema};

use super::extract::{TypedJson, TypedPath, TypedQuery};
use super::openapi::tags;
use crate::http::AppState;
use crate::http::extract::ApiActor;
use crate::http::problem::{CommonProblems, Problem};
use crate::ledger::manual_rates::ManualRateList;
use crate::ledger::net_worth::{FxCoverageList, FxRateList, FxRatesQuery};

/// One currency's one day.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Path)]
#[serde(deny_unknown_fields)]
pub struct ManualRatePath {
    /// The currency, an ISO 4217 code.
    #[param(value_type = String, pattern = "^[A-Z]{3}$")]
    pub currency: Currency,
    /// The day, written exactly `YYYY-MM-DD`.
    #[serde(deserialize_with = "strict_date::deserialize")]
    pub on: Date,
}

/// A rate to enter.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ManualRateInput {
    pub rate: FxRate,
    /// Keeps a rate over 1.5 times from the published rate nearest its day, within 30 days,
    /// which is otherwise refused as `unusual_rate`. By default `false`.
    #[serde(default)]
    pub confirm_unusual: bool,
}

/// Which manual rates to read.
#[derive(Debug, Clone, Copy, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct ManualRatesQuery {
    /// How many of the newest to skip: by default none.
    pub offset: Option<u32>,
}

/// What is known of fetching each currency's rates.
#[utoipa::path(
    get,
    path = "/api/v1/fx/coverage",
    operation_id = "getFxCoverage",
    tag = tags::FX,
    description = "For each currency a fetch was tried for: `status` (`ready`, `not_published` \
                   or `failing`), the `first_on` and `last_on` of the rates held, when it was last \
                   tried and last succeeded, and `last_error`, a fixed code (`unavailable`, \
                   `malformed` or `network`). A currency never tried has no entry. `manual` lists, \
                   apart from that, each currency holding rates an admin entered, with its \
                   `first_on` and `last_on`: such a currency can have no entry in `items`.\n\n\
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
    description = "A currency's rates in a range of at most 1,000 days, newest first: `rate` is \
                   units of the currency per one euro, as exact decimal text. `manual` is true of \
                   a rate an admin entered, which on its day stands in for any published rate. A \
                   weekend or holiday has no entry, and a report uses the most recent earlier \
                   rate. The \
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

/// The rates admins have entered.
#[utoipa::path(
    get,
    path = "/api/v1/fx/manual-rates",
    operation_id = "listManualRates",
    tag = tags::FX,
    description = "The rates admins have entered, the newest day first, then by currency: at most \
                   1,000 from `offset`. `truncated` says there are older ones: ask again with \
                   `offset` past these. `getFxRates` also shows each, marked. \
                   Needs `read`; a token with account grants also needs `all` at `balances`, \
                   since which rates are stored depends on the currencies in use. The refusal is \
                   the same whatever exists.",
    security(("bearer" = [])),
    params(ManualRatesQuery),
    responses(
        (status = 200, description = "The rates.", body = ManualRateList),
        CommonProblems,
    ),
)]
pub(super) async fn manual_rates(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedQuery(query): TypedQuery<ManualRatesQuery>,
) -> Result<Json<ManualRateList>, Problem> {
    Ok(Json(
        state
            .ledger
            .manual_rates(&actor, query.offset.unwrap_or(0))
            .await?,
    ))
}

/// Enters a rate for a currency and a day.
#[utoipa::path(
    put,
    path = "/api/v1/fx/manual-rates/{currency}/{on}",
    operation_id = "setManualRate",
    tag = tags::FX,
    description = "`rate` is units of the currency per one euro, as exact decimal text: above zero, \
                   at most 10 digits, 6 of them decimals. It is carried forward like a fetched \
                   rate, from its day until a later rate of either kind. A fetch never overwrites \
                   it. On its own day it wins over a published rate, which stays behind it, and a \
                   published rate on a later day takes over as usual. Everything built from it is \
                   marked: `manual_rate` on a report's points, its `manual_rates` and `manual` on \
                   `getFxRates`. It doesn't stop a report being `firm`, until it is old enough for \
                   `rate_stale`. It rewrites the net worth values it moves; with very \
                   many, a report answers `rebuilding` for a moment. Audited with the old and new \
                   rate. The same rate again writes nothing and answers with what is held.\n\n\
                   A rate that is malformed, not above zero or a JSON number is 400, as is an \
                   unknown code or a day not written `YYYY-MM-DD`. The euro, the display currency, \
                   a day after today or before 1970 is 422.\n\n\
                   A rate over 1.5 times from the published rate nearest its day, within 30 days, \
                   as one entered the wrong way round often is, is 422 with the code `unusual_rate` \
                   and both numbers in `detail`, unless `confirm_unusual` is true, and the audit \
                   entry of one kept says what it was against. With no published rate nearby \
                   there is nothing to compare it with, and no such check.\n\n\
                   Needs `admin`; a token with account grants also needs `all` at `balances`: \
                   which codes convert at once depends on the currencies in use. The refusal is \
                   the same whatever exists.",
    security(("bearer" = [])),
    params(ManualRatePath),
    request_body = ManualRateInput,
    responses(
        (status = 200, description = "The rate as it now is.", body = ManualRate),
        CommonProblems,
    ),
)]
pub(super) async fn set_manual_rate(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedPath(path): TypedPath<ManualRatePath>,
    TypedJson(input): TypedJson<ManualRateInput>,
) -> Result<Json<ManualRate>, Problem> {
    Ok(Json(
        state
            .ledger
            .set_manual_rate(
                &actor,
                path.currency,
                path.on,
                input.rate,
                input.confirm_unusual,
            )
            .await?,
    ))
}

/// Removes a rate an admin entered.
#[utoipa::path(
    delete,
    path = "/api/v1/fx/manual-rates/{currency}/{on}",
    operation_id = "removeManualRate",
    tag = tags::FX,
    description = "What it covered falls back on the published rate of that day if one is held, \
                   or else the most recent earlier rate; with neither, amounts in the currency go \
                   back to `null`, with the caution `rate_missing`. Audited with the old rate. With \
                   no such rate it is 404, and writes nothing. Needs what `setManualRate` needs.",
    security(("bearer" = [])),
    params(ManualRatePath),
    responses(
        (status = 204, description = "The rate is removed."),
        CommonProblems,
    ),
)]
pub(super) async fn remove_manual_rate(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedPath(path): TypedPath<ManualRatePath>,
) -> Result<StatusCode, Problem> {
    state
        .ledger
        .remove_manual_rate(&actor, path.currency, path.on)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
