//! `/api/v1/household/settings`: the household's settings.

use axum::Json;
use axum::extract::State;

use super::extract::TypedJson;
use super::openapi::tags;
use crate::http::AppState;
use crate::http::extract::ApiActor;
use crate::http::problem::{CommonProblems, Problem};
use crate::ledger::settings::{ForecastSettingsPatch, HouseholdSettings};

/// The household's settings.
#[utoipa::path(
    get,
    path = "/api/v1/household/settings",
    operation_id = "getHouseholdSettings",
    tag = tags::HOUSEHOLD,
    description = "The settings: the safety floor (`null` until one is set, and a forecast has \
                   no number without it), whether planned income counts, the default horizon, \
                   when a balance counts as stale, the display currency net worth is shown \
                   in (`null` until an admin sets it: it is never guessed), and the time zone \
                   that decides which day is today (`timezone`, always a name) with what an \
                   admin chose (`timezone_setting`, `null` while the server's default applies). \
                   Needs `read`. A \
                   token with account grants below `all` at `transactions` reads `floor` as \
                   `null`, the same as none set: a floor is a figure of the whole household's \
                   plan, and that is the access a forecast needs. The other settings read the \
                   same for every token.",
    security(("bearer" = [])),
    responses(
        (status = 200, description = "The settings; the defaults until some are set.",
         body = HouseholdSettings),
        CommonProblems,
    ),
)]
pub(super) async fn get(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
) -> Result<Json<HouseholdSettings>, Problem> {
    Ok(Json(state.ledger.household_settings(&actor).await?))
}

/// Changes the household's settings.
#[utoipa::path(
    patch,
    path = "/api/v1/household/settings",
    operation_id = "updateHouseholdSettings",
    tag = tags::HOUSEHOLD,
    description = "Changes the settings given; a field left out stays. A floor or a display \
                   currency can be replaced but never unset: `floor: null` or \
                   `display_currency: null` leaves it. A patch that changes nothing writes \
                   nothing. A new display currency recomputes all net worth history before this \
                   answers, and is audited with the old and new codes. `timezone` takes an IANA \
                   name, and alone among these `null` clears it, returning to the server's \
                   default. A new zone applies at once, is audited with the old and new names, \
                   and rewrites no day already recorded: only new days use it. If it makes today \
                   earlier than the net worth table's last day, the table ends today from this \
                   change on. A name that isn't a zone is 422, and so is a number out \
                   of range; one outside the field's type (such as 300 months) or an unknown currency \
                   code is 400. Needs `admin`, whatever the token's account grants, but the \
                   answer shows `floor` only as `getHouseholdSettings` would to that token. A \
                   patch naming `display_currency`, whatever it names, also needs `all` at \
                   `balances` from a token with account grants: which codes convert at once \
                   depends on the currencies in use. The refusal is the same whatever exists.",
    security(("bearer" = [])),
    request_body = ForecastSettingsPatch,
    responses(
        (status = 200, description = "The settings as they now are.", body = HouseholdSettings),
        CommonProblems,
    ),
)]
pub(super) async fn update(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedJson(patch): TypedJson<ForecastSettingsPatch>,
) -> Result<Json<HouseholdSettings>, Problem> {
    Ok(Json(
        state.ledger.update_forecast_settings(&actor, patch).await?,
    ))
}
