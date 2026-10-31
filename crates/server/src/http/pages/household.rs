//! Household settings: the theme everyone sees unless they choose their
//! own. Admin-only; a denial renders as a 403 page.

use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use coffer_domain::identity::ThemeName;
use coffer_domain::policy::{Action, authorize};
use maud::html;
use serde::Deserialize;

use super::account::viewer;
use super::layout::{Frame, Nav, csrf_field, page};
use super::{PageError, PageForm, PageSession};
use crate::auth::AuthError;
use crate::http::AppState;
use crate::themes::THEMES;

pub fn routes() -> Router<AppState> {
    Router::new().route("/admin/household", get(household).post(set_theme))
}

async fn household(
    State(state): State<AppState>,
    session: PageSession,
) -> Result<Response, PageError> {
    authorize(&session.actor(), Action::ManageHousehold).map_err(AuthError::from)?;
    let viewer = viewer(&state, &session);
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
    Ok(Redirect::to("/admin/household"))
}
