//! `/api/v1/household/settings`: the household's forecast settings.

use axum::Json;
use axum::extract::State;
use coffer_domain::plan::ForecastSettings;

use super::extract::TypedJson;
use super::openapi::tags;
use crate::http::AppState;
use crate::http::extract::ApiActor;
use crate::http::problem::{CommonProblems, Problem};
use crate::ledger::settings::ForecastSettingsPatch;

/// The household's forecast settings.
#[utoipa::path(
    get,
    path = "/api/v1/household/settings",
    operation_id = "getHouseholdSettings",
    tag = tags::HOUSEHOLD,
    description = "The forecast settings: the safety floor (`null` until one is set, and a \
                   forecast has no number without it), whether planned income counts, the \
                   default horizon and when a balance counts as stale. Needs `read`. Account \
                   grants do not apply: settings describe no account.",
    security(("bearer" = [])),
    responses(
        (status = 200, description = "The settings; the defaults until some are set.",
         body = ForecastSettings),
        CommonProblems,
    ),
)]
pub(super) async fn get(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
) -> Result<Json<ForecastSettings>, Problem> {
    Ok(Json(state.ledger.forecast_settings(&actor).await?))
}

/// Changes the household's forecast settings.
#[utoipa::path(
    patch,
    path = "/api/v1/household/settings",
    operation_id = "updateHouseholdSettings",
    tag = tags::HOUSEHOLD,
    description = "Changes the settings given; a field left out stays. A floor can be \
                   replaced but never unset: `floor: null` leaves it. A patch that changes \
                   nothing writes nothing. A number out of range is 422; one outside the field's type (such \
                   as 300 months) is 400. Needs `admin`. Account grants do not apply.",
    security(("bearer" = [])),
    request_body = ForecastSettingsPatch,
    responses(
        (status = 200, description = "The settings as they now are.", body = ForecastSettings),
        CommonProblems,
    ),
)]
pub(super) async fn update(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedJson(patch): TypedJson<ForecastSettingsPatch>,
) -> Result<Json<ForecastSettings>, Problem> {
    Ok(Json(
        state.ledger.update_forecast_settings(&actor, patch).await?,
    ))
}
