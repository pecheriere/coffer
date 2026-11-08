//! The cash forecast's settings (money redesign §4.4): saved through the ledger call the API
//! makes, so they are audited and checked the same. Every refusal is fixed text.

use axum::Router;
use axum::extract::State;
use axum::response::Redirect;
use axum::routing::post;
use coffer_domain::plan::{MAX_HORIZON_MONTHS, MAX_STALE_AFTER_DAYS};
use coffer_domain::policy::{Action, authorize};
use maud::{Markup, html};
use serde::Deserialize;

use crate::auth::AuthError;
use crate::http::AppState;
use crate::http::pages::layout::csrf_field;
use crate::http::pages::{PageError, PageForm, PageSession, refused};
use crate::ledger::settings::{ForecastSettingsPatch, HouseholdSettings};

const SAVE: &str = "/admin/household/forecast";
/// Where the form sends the admin back to.
const BACK: &str = "/admin/household#forecast";

pub(super) fn routes() -> Router<AppState> {
    Router::new().route(SAVE, post(save))
}

/// The form, filled with what the settings hold now.
pub(super) fn card(csrf: &str, settings: &HouseholdSettings) -> Markup {
    html! {
        section.card id="forecast" {
            h2 { "Cash forecast" }
            p { "How the forecast counts and how far it looks, unless someone asks it otherwise." }
            form method="post" action=(SAVE) {
                (csrf_field(csrf))
                label.choice {
                    input type="checkbox" name="include_income" checked[settings.include_income];
                    "Count planned income in the forecast's totals"
                }
                label.field {
                    "How many months the forecast looks ahead"
                    input.input type="number" name="horizon"
                        value=(settings.default_horizon_months) required min="1"
                        max=(MAX_HORIZON_MONTHS) step="1";
                }
                label.field {
                    "Flag a balance older than this many days"
                    input.input type="number" name="stale_after_days"
                        value=(settings.stale_after_days) required min="1"
                        max=(MAX_STALE_AFTER_DAYS) step="1";
                }
                button.btn.btn-primary type="submit" { "Save forecast settings" }
            }
        }
    }
}

/// Unknown fields are refused: a forged form fails whole. A browser leaves an unticked box
/// out, so a missing `include_income` means no.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    include_income: String,
    #[serde(default)]
    horizon: String,
    #[serde(default)]
    stale_after_days: String,
}

/// What the form says, with the fixed refusal of the first field that can't be read. The
/// ledger checks the ranges again; checking them here keeps the refusal in the form's words.
fn patch_of(form: &SaveForm) -> Result<ForecastSettingsPatch, PageError> {
    let include_income = match form.include_income.as_str() {
        "" => false,
        "on" => true,
        _ => return Err(PageError::invalid("not an answer the form offers")),
    };
    let horizon = form
        .horizon
        .trim()
        .parse::<u8>()
        .ok()
        .filter(|months| (1..=MAX_HORIZON_MONTHS).contains(months))
        .ok_or_else(|| {
            PageError::invalid("how far the forecast looks is a whole number of months, 1 to 60")
        })?;
    let stale_after_days = form
        .stale_after_days
        .trim()
        .parse::<u16>()
        .ok()
        .filter(|days| (1..=MAX_STALE_AFTER_DAYS).contains(days))
        .ok_or_else(|| {
            PageError::invalid("how old a balance may be is a whole number of days, 1 to 365")
        })?;
    Ok(ForecastSettingsPatch {
        include_income: Some(include_income),
        default_horizon_months: Some(horizon),
        stale_after_days: Some(stale_after_days),
        ..ForecastSettingsPatch::default()
    })
}

/// Saves all three settings at once. The policy is checked before the form is even read,
/// so a member is refused whatever they send.
async fn save(
    State(state): State<AppState>,
    session: PageSession,
    form: Result<PageForm<SaveForm>, PageError>,
) -> Result<Redirect, PageError> {
    authorize(&session.actor(), Action::ManageHousehold).map_err(AuthError::from)?;
    let PageForm(form) = form?;
    let actor = session.verify(&state, &form.csrf)?;
    let patch = patch_of(&form)?;
    state
        .ledger
        .update_forecast_settings(&actor, patch)
        .await
        .map_err(refused)?;
    Ok(Redirect::to(BACK))
}
