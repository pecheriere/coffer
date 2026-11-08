//! `/api/v1/household`: the household's settings and its emergency funds.

use axum::Json;
use axum::extract::State;
use coffer_domain::money::Currency;
use coffer_domain::plan::EmergencyFundTarget;
use serde::Deserialize;
use utoipa::IntoParams;

use super::extract::{TypedJson, TypedPath};
use super::openapi::tags;
use crate::http::AppState;
use crate::http::extract::ApiActor;
use crate::http::problem::{CommonProblems, Problem};
use crate::ledger::settings::{ForecastSettingsPatch, HouseholdSettings};

/// One currency's emergency fund.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Path)]
#[serde(deny_unknown_fields)]
pub struct EmergencyFundPath {
    /// The currency, an ISO 4217 code.
    #[param(value_type = String, pattern = "^[A-Z]{3}$")]
    pub currency: Currency,
}

/// The household's settings.
#[utoipa::path(
    get,
    path = "/api/v1/household/settings",
    operation_id = "getHouseholdSettings",
    tag = tags::HOUSEHOLD,
    description = "The settings: each currency's emergency fund (`setEmergencyFund`; a \
                   forecast has no number while a currency in use has none), whether planned \
                   income counts, the default horizon, \
                   when a balance counts as stale, the display currency net worth is shown \
                   in (`null` until an admin sets it: it is never guessed), and the time zone \
                   that decides which day is today (`timezone`, always a name) with what an \
                   admin chose (`timezone_setting`, `null` while the server's default applies). \
                   Needs `read`. A \
                   token with account grants below `all` at `transactions` reads \
                   `emergency_funds` as `null`, whether or not any is set: an emergency fund is \
                   a figure of the whole household's plan, and that is the access a forecast \
                   needs. The other settings read the same for every token.",
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
    description = "Changes the settings given; a field left out stays. A display currency \
                   can be replaced but never unset: `display_currency: null` leaves it. \
                   Emergency funds change through `setEmergencyFund` and `removeEmergencyFund`. \
                   A patch that changes nothing writes \
                   nothing. A new display currency recomputes all net worth history before this \
                   answers, and is audited with the old and new codes. `timezone` takes an IANA \
                   name, and alone among these `null` clears it, returning to the server's \
                   default. A new zone applies at once, is audited with the old and new names, \
                   and rewrites no day already recorded: only new days use it. If it makes today \
                   earlier than the net worth table's last day, the table ends today from this \
                   change on. A name that isn't a zone is 422, and so is a number out \
                   of range; one outside the field's type (such as 300 months) or an unknown currency \
                   code is 400. Needs `admin`, whatever the token's account grants, but the \
                   answer shows `emergency_funds` only as `getHouseholdSettings` would to that \
                   token. A \
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

/// Sets one currency's emergency fund.
#[utoipa::path(
    put,
    path = "/api/v1/household/emergency-funds/{currency}",
    operation_id = "setEmergencyFund",
    tag = tags::HOUSEHOLD,
    description = "Sets how much cash to keep untouched in this currency, replacing any target \
                   it had: a fixed amount in its minor units (`fixed`, 0 to 10^15), or some months \
                   (`months_of_outflows`, 1 to 60) of that currency's planned recurring outflows, \
                   counted from a forecast's `as_of`. A forecast applies it to that currency's \
                   totals only, and has no number while a currency in use has none. A currency \
                   the household doesn't use yet may have one; it changes no answer until it is \
                   in use. Audited with the old and new target; the same target again writes \
                   nothing. A target out of range is 422; an unknown code, or a code not written \
                   as three capital letters, is 400.\n\n\
                   Needs `admin`, whatever the token's account grants. The answer is the \
                   settings as `getHouseholdSettings` shows them to that token, so a token below \
                   `all` at `transactions` reads `emergency_funds` as `null`.",
    security(("bearer" = [])),
    params(EmergencyFundPath),
    request_body = EmergencyFundTarget,
    responses(
        (status = 200, description = "The settings as they now are.", body = HouseholdSettings),
        CommonProblems,
    ),
)]
pub(super) async fn set_emergency_fund(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedPath(path): TypedPath<EmergencyFundPath>,
    TypedJson(target): TypedJson<EmergencyFundTarget>,
) -> Result<Json<HouseholdSettings>, Problem> {
    Ok(Json(
        state
            .ledger
            .set_emergency_fund(&actor, path.currency, target)
            .await?,
    ))
}

/// Removes one currency's emergency fund.
#[utoipa::path(
    delete,
    path = "/api/v1/household/emergency-funds/{currency}",
    operation_id = "removeEmergencyFund",
    tag = tags::HOUSEHOLD,
    description = "Removes this currency's emergency fund target: while the currency is in \
                   use, a forecast has no number until one is set again. Audited with the old \
                   target. With none set it is 404, and writes nothing.\n\n\
                   Needs `admin`; a token with account grants also needs `all` at \
                   `transactions`, since whether a target is set is a figure of the whole \
                   household's plan. The refusal is the same whatever is set.",
    security(("bearer" = [])),
    params(EmergencyFundPath),
    responses(
        (status = 200, description = "The settings as they now are.", body = HouseholdSettings),
        CommonProblems,
    ),
)]
pub(super) async fn remove_emergency_fund(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedPath(path): TypedPath<EmergencyFundPath>,
) -> Result<Json<HouseholdSettings>, Problem> {
    Ok(Json(
        state
            .ledger
            .remove_emergency_fund(&actor, path.currency)
            .await?,
    ))
}
