//! The exchange rates an admin enters by hand (spec §8.10 N16): through the service the API
//! uses, so they are audited and checked the same. Every refusal is fixed text.

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::post;
use coffer_domain::fx::FxRate;
use coffer_domain::money::Currency;
use coffer_domain::plan::strict_date;
use coffer_domain::policy::{Action, authorize};
use jiff::civil::Date;
use maud::{Markup, html};
use serde::Deserialize;

use super::HOUSEHOLD;
use crate::auth::AuthError;
use crate::http::AppState;
use crate::http::pages::account::viewer;
use crate::http::pages::amounts::day;
use crate::http::pages::layout::csrf_field;
use crate::http::pages::layout::{Frame, Nav, page};
use crate::http::pages::{PageError, PageForm, PageSession};
use crate::http::problem::Problem;
use crate::ledger::LedgerError;
use crate::ledger::manual_rates::ManualRateList;

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
    list: &ManualRateList,
    from: u32,
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
            @if !list.items.is_empty() {
                table.sums {
                    tbody {
                        @for held in &list.items {
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
            @if list.truncated {
                p.fine {
                    "These aren't all of them. "
                    a href=(format!("{HOUSEHOLD}?rates_from={}#manual-rates", next_from(list, from))) {
                        "Older rates"
                    }
                }
            }
            @if from > 0 {
                p.fine {
                    a href=(format!("{HOUSEHOLD}#manual-rates")) { "Back to the newest rates" }
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
                    "How much of it one euro buys, such as 655.957"
                    input.input type="text" name="rate" required inputmode="decimal"
                        autocomplete="off";
                }
                button.btn.btn-primary type="submit" { "Save rate" }
            }
        }
    }
}

/// Where the list goes on after this part of it.
fn next_from(list: &ManualRateList, from: u32) -> u32 {
    let shown = u32::try_from(list.items.len()).unwrap_or(u32::MAX);
    from.saturating_add(shown)
}

/// The rate refused as unusual, the reason in numbers, and the same entry again with the box
/// to tick. It holds nothing the form didn't, once read as valid.
fn confirm_page(csrf: &str, reason: &str, currency: Currency, on: Date, rate: FxRate) -> Markup {
    html! {
        h1 { "Check this rate" }
        section.card id="confirm-rate" {
            p { (reason) }
            form method="post" action=(ENTER) {
                (csrf_field(csrf))
                input type="hidden" name="currency" value=(currency.as_str());
                input type="hidden" name="on" value=(on.to_string());
                input type="hidden" name="rate" value=(rate.to_string());
                label.choice {
                    input type="checkbox" name="confirm_unusual" value="yes" required;
                    "This rate is right, keep it"
                }
                button.btn.btn-primary type="submit" { "Save rate" }
            }
            p { a href=(format!("{HOUSEHOLD}#manual-rates")) { "Go back without saving" } }
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
    #[serde(default)]
    confirm_unusual: String,
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
/// they send. A rate refused as unusual comes back as a page offering to confirm it, not as
/// an error page, with the status of a refusal.
async fn enter(
    State(state): State<AppState>,
    session: PageSession,
    form: Result<PageForm<EnterForm>, PageError>,
) -> Result<Response, PageError> {
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
    let confirm_unusual = match form.confirm_unusual.as_str() {
        "" => false,
        "yes" => true,
        _ => return Err(PageError::invalid("not an answer the form offers")),
    };
    match state
        .ledger
        .set_manual_rate(&actor, currency, on, rate, confirm_unusual)
        .await
    {
        Ok(_) => Ok(Redirect::to(HOUSEHOLD).into_response()),
        Err(err @ LedgerError::UnusualRate { .. }) => {
            let viewer = viewer(&state, &session).await;
            let frame = Frame {
                look: session.look(&state).await,
                viewer: Some(&viewer),
                current: Some(Nav::Household),
                narrow: false,
            };
            let content = confirm_page(&viewer.csrf, &err.to_string(), currency, on, rate);
            let reply = page(&frame, "Check this rate", &content);
            Ok((StatusCode::UNPROCESSABLE_ENTITY, reply).into_response())
        }
        Err(err) => Err(PageError(Problem::from(err))),
    }
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
