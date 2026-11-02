//! Household settings: the theme everyone sees unless they choose their
//! own, and the display currency. Admin-only; a denial renders as a 403 page.

use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use coffer_domain::identity::ThemeName;
use coffer_domain::money::Currency;
use coffer_domain::policy::{Action, authorize};
use maud::html;
use serde::Deserialize;

use super::account::viewer;
use super::layout::{Frame, Nav, csrf_field, page};
use super::{PageError, PageForm, PageSession};
use crate::auth::AuthError;
use crate::http::AppState;
use crate::http::problem::Problem;
use crate::ledger::settings::ForecastSettingsPatch;
use crate::themes::THEMES;

const HOUSEHOLD: &str = "/admin/household";
const DISPLAY_CURRENCY: &str = "/admin/household/display-currency";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(HOUSEHOLD, get(household).post(set_theme))
        .route(DISPLAY_CURRENCY, post(set_display_currency))
}

async fn household(
    State(state): State<AppState>,
    session: PageSession,
) -> Result<Response, PageError> {
    let actor = session.actor();
    authorize(&actor, Action::ManageHousehold).map_err(AuthError::from)?;
    let settings = state
        .ledger
        .household_settings(&actor)
        .await
        .map_err(|err| PageError(Problem::from(err)))?;
    let viewer = viewer(&state, &session).await;
    let look = session.look(&state).await;
    let current = state.appearance.household_theme().await;
    let content = html! {
        h1 { "Household" }
        section.card {
            h2 { "Theme" }
            p { "The theme everyone sees unless they choose their own on their account page." }
            form method="post" action="/admin/household" {
                (csrf_field(&viewer.csrf))
                fieldset.choices {
                    legend { "Household theme" }
                    @for theme in THEMES {
                        label.choice {
                            input type="radio" name="theme" value=(theme.name)
                                checked[theme.name == current.name];
                            (theme.label)
                        }
                    }
                }
                button.btn.btn-primary type="submit" { "Save household theme" }
            }
        }
        section.card {
            h2 { "Display currency" }
            p {
                "What net worth is added up and shown in. Changing it recomputes all history at "
                "once, at the rates of each day, and changes no balance."
            }
            form method="post" action=(DISPLAY_CURRENCY) {
                (csrf_field(&viewer.csrf))
                label.field {
                    "Display currency"
                    select.input name="display_currency" required {
                        @if settings.display_currency.is_none() {
                            option value="" selected { "Choose a currency" }
                        }
                        @for currency in Currency::all() {
                            option value=(currency.as_str())
                                selected[settings.display_currency == Some(currency)] {
                                (currency.as_str())
                            }
                        }
                    }
                }
                button.btn.btn-primary type="submit" { "Save display currency" }
            }
        }
    };
    let frame = Frame {
        look,
        viewer: Some(&viewer),
        current: Some(Nav::Household),
        narrow: false,
    };
    Ok(page(&frame, "Household", &content).into_response())
}

/// Unknown fields are refused: a forged form fails whole.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ThemeForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    theme: String,
}

/// Sets the household theme. The policy is checked before the theme is
/// parsed, so a member is refused whatever theme they send.
async fn set_theme(
    State(state): State<AppState>,
    session: PageSession,
    PageForm(form): PageForm<ThemeForm>,
) -> Result<Redirect, PageError> {
    let actor = session.verify(&state, &form.csrf)?;
    authorize(&actor, Action::ManageHousehold).map_err(AuthError::from)?;
    let theme = ThemeName::parse(&form.theme).map_err(AuthError::Invalid)?;
    state.appearance.set_household_theme(&actor, theme).await?;
    Ok(Redirect::to(HOUSEHOLD))
}

/// Unknown fields are refused: a forged form fails whole.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DisplayCurrencyForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    display_currency: String,
}

/// Sets the display currency through the service the API uses, so it's audited the same.
/// The policy is checked before the form is even read, so a member is refused whatever
/// they send, and every refusal after that is fixed text.
async fn set_display_currency(
    State(state): State<AppState>,
    session: PageSession,
    form: Result<PageForm<DisplayCurrencyForm>, PageError>,
) -> Result<Redirect, PageError> {
    authorize(&session.actor(), Action::ManageHousehold).map_err(AuthError::from)?;
    let PageForm(form) = form?;
    let actor = session.verify(&state, &form.csrf)?;
    let currency = Currency::parse(&form.display_currency)
        .map_err(|_| PageError::invalid("not a currency coffer knows"))?;
    state
        .ledger
        .update_forecast_settings(
            &actor,
            ForecastSettingsPatch {
                display_currency: Some(currency),
                ..ForecastSettingsPatch::default()
            },
        )
        .await
        .map_err(|err| PageError(Problem::from(err)))?;
    Ok(Redirect::to(HOUSEHOLD))
}
