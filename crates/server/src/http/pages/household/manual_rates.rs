//! The exchange rates an admin enters by hand (spec §8.10 N16): through the service the API
//! uses, so they are audited and checked the same. Every refusal is fixed text.

use axum::Router;
use axum::extract::State;
use axum::response::Redirect;
use axum::routing::post;
use coffer_domain::fx::{FxRate, ManualRate};
use coffer_domain::money::Currency;
use coffer_domain::plan::strict_date;
use coffer_domain::policy::{Action, authorize};
use jiff::civil::Date;
use maud::{Markup, html};
use serde::Deserialize;

use super::HOUSEHOLD;
use crate::auth::AuthError;
use crate::http::AppState;
use crate::http::pages::amounts::day;
use crate::http::pages::layout::csrf_field;
use crate::http::pages::{PageError, PageForm, PageSession};
use crate::http::problem::Problem;

const ENTER: &str = "/admin/household/manual-rates";
const REMOVE: &str = "/admin/household/manual-rates/remove";

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route(ENTER, post(enter))
        .route(REMOVE, post(remove))
}

/// What the household page says of them, and its forms.
pub(super) fn card(
    csrf: &str,
    rates: &[ManualRate],
    display: Option<Currency>,
    today: Date,
) -> Markup {
    html! {
        section.card id="manual-rates" {
            h2 { "Exchange rates entered by hand" }
            p {
                "For a currency the source of rates doesn't publish, or a day it has wrong. A rate "
                "counts from its day until a later rate, and on its own day it wins over a "
                "published one, which no fetch changes. To cover an old balance, date the rate on "
                "or before it. Every figure using one says so."
            }
            @if !rates.is_empty() {
                table.sums {
                    tbody {
                        @for held in rates {
                            tr {
                                th scope="row" { (held.currency.as_str()) ", " (day(held.on)) }
                                td { (held.rate.to_string()) " per euro" }
                                td {
                                    form method="post" action=(REMOVE) {
                                        (csrf_field(csrf))
                                        input type="hidden" name="currency"
                                            value=(held.currency.as_str());
                                        input type="hidden" name="on" value=(held.on.to_string());
                                        button.btn type="submit" { "Remove" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            form method="post" action=(ENTER) {
                (csrf_field(csrf))
                label.field {
                    "Currency"
                    select.input name="currency" required {
                        option value="" selected { "Choose a currency" }
                        @for currency in Currency::all() {
                            @if currency.as_str() != "EUR" && Some(currency) != display {
                                option value=(currency.as_str()) { (currency.as_str()) }
                            }
                        }
                    }
                }
                label.field {
                    "Day"
                    input.input type="date" name="on" required max=(today.to_string());
                }
                label.field {
                    "Units of it per one euro, such as 655.957"
                    input.input type="text" name="rate" required inputmode="decimal"
                        autocomplete="off";
                }
                button.btn.btn-primary type="submit" { "Save rate" }
            }
        }
    }
}

/// Unknown fields are refused: a forged form fails whole.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnterForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    currency: String,
    #[serde(default)]
    on: String,
    #[serde(default)]
    rate: String,
}

/// Unknown fields are refused: a forged form fails whole.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    currency: String,
    #[serde(default)]
    on: String,
}

fn currency_of(text: &str) -> Result<Currency, PageError> {
    Currency::parse(text).map_err(|_| PageError::invalid("not a currency coffer knows"))
}

fn day_of(text: &str) -> Result<Date, PageError> {
    strict_date::parse(text).ok_or_else(|| PageError::invalid("not a real day"))
}

/// The policy is checked before the form is even read, so a member is refused whatever
/// they send.
async fn enter(
    State(state): State<AppState>,
    session: PageSession,
    form: Result<PageForm<EnterForm>, PageError>,
) -> Result<Redirect, PageError> {
    authorize(&session.actor(), Action::ManageHousehold).map_err(AuthError::from)?;
    let PageForm(form) = form?;
    let actor = session.verify(&state, &form.csrf)?;
    let currency = currency_of(&form.currency)?;
    let on = day_of(&form.on)?;
    let rate: FxRate = form.rate.parse().map_err(|_| {
        PageError::invalid(
            "a rate is a number above zero, with at most 10 digits, 6 of them decimals",
        )
    })?;
    state
        .ledger
        .set_manual_rate(&actor, currency, on, rate)
        .await
        .map_err(|err| PageError(Problem::from(err)))?;
    Ok(Redirect::to(HOUSEHOLD))
}

/// As [`enter`].
async fn remove(
    State(state): State<AppState>,
    session: PageSession,
    form: Result<PageForm<RemoveForm>, PageError>,
) -> Result<Redirect, PageError> {
    authorize(&session.actor(), Action::ManageHousehold).map_err(AuthError::from)?;
    let PageForm(form) = form?;
    let actor = session.verify(&state, &form.csrf)?;
    let currency = currency_of(&form.currency)?;
    let on = day_of(&form.on)?;
    state
        .ledger
        .remove_manual_rate(&actor, currency, on)
        .await
        .map_err(|err| PageError(Problem::from(err)))?;
    Ok(Redirect::to(HOUSEHOLD))
}
