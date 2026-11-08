//! Each currency's emergency fund (money redesign §4.4): set and removed through the ledger
//! calls the API makes, so they are audited and checked the same. Every refusal is fixed
//! text and never repeats what was typed.

use axum::Router;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::response::Redirect;
use axum::routing::post;
use coffer_domain::money::{Currency, DecimalAmount};
use coffer_domain::plan::{EmergencyFund, EmergencyFundTarget, MAX_EMERGENCY_FUND_MONTHS};
use coffer_domain::policy::{Action, authorize};
use maud::{Markup, html};
use serde::Deserialize;

use crate::auth::AuthError;
use crate::http::AppState;
use crate::http::pages::amounts::grouped;
use crate::http::pages::layout::csrf_field;
use crate::http::pages::{PageError, PageForm, PageSession, refused};
use crate::http::problem::Problem;
use crate::ledger::LedgerError;

const SET: &str = "/admin/household/emergency-fund";
const REMOVE: &str = "/admin/household/emergency-fund/{currency}/remove";
/// Where every form here sends the admin back to.
const BACK: &str = "/admin/household#emergency-fund";

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route(SET, post(set))
        .route(REMOVE, post(remove))
}

/// What one target says: "10,000.00" or "3 months of planned spending".
fn described(currency: Currency, target: EmergencyFundTarget) -> String {
    match target {
        EmergencyFundTarget::Fixed { minor_units } => grouped(minor_units, currency),
        EmergencyFundTarget::MonthsOfOutflows { months: 1 } => {
            "1 month of planned spending".to_owned()
        }
        EmergencyFundTarget::MonthsOfOutflows { months } => {
            format!("{months} months of planned spending")
        }
    }
}

/// The targets set, each with its Remove button, and the form that sets one. `funds` is
/// what the household settings hold; `display` is preselected in the form.
pub(super) fn card(csrf: &str, funds: &[EmergencyFund], display: Option<Currency>) -> Markup {
    html! {
        section.card id="emergency-fund" {
            h2 { "Emergency fund" }
            p { "Money kept aside for surprises, per currency. Free money counts what's left after it." }
            @if !funds.is_empty() {
                table.sums {
                    tbody {
                        @for fund in funds {
                            tr {
                                th scope="row" { (fund.currency.as_str()) ":" }
                                td { (described(fund.currency, fund.target)) }
                                td {
                                    form method="post"
                                        action=(format!("{SET}/{}/remove", fund.currency.as_str())) {
                                        (csrf_field(csrf))
                                        button.btn type="submit" { "Remove" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            form method="post" action=(SET) {
                (csrf_field(csrf))
                h3 { "Set an emergency fund" }
                p.fine { "Setting one for a currency that has one replaces it." }
                label.field {
                    "Currency"
                    select.input name="currency" required {
                        @for currency in Currency::all() {
                            option value=(currency.as_str()) selected[display == Some(currency)] {
                                (currency.as_str())
                            }
                        }
                    }
                }
                fieldset.choices {
                    legend { "Kind of target" }
                    label.choice {
                        input type="radio" name="kind" value="fixed" checked;
                        "A fixed amount"
                    }
                    label.choice {
                        input type="radio" name="kind" value="months";
                        "Some months of planned spending"
                    }
                }
                label.field {
                    "Amount, for a fixed amount, such as 10000"
                    input.input type="text" name="amount" inputmode="decimal" autocomplete="off";
                }
                label.field {
                    "Months, for months of planned spending"
                    input.input type="number" name="months" min="1"
                        max=(MAX_EMERGENCY_FUND_MONTHS) step="1";
                }
                button.btn.btn-primary type="submit" { "Save emergency fund" }
            }
        }
    }
}

/// Unknown fields are refused: a forged form fails whole. Both kinds' fields are always
/// sent, and only the chosen kind's is read.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    currency: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    amount: String,
    #[serde(default)]
    months: String,
}

/// Unknown fields are refused: a forged form fails whole.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveForm {
    #[serde(default)]
    csrf: String,
}

fn currency_of(text: &str) -> Result<Currency, PageError> {
    Currency::parse(text).map_err(|_| PageError::invalid("not a currency coffer knows"))
}

/// The target the form says, with the fixed refusal of the chosen kind's field.
fn target_of(form: &SetForm, currency: Currency) -> Result<EmergencyFundTarget, PageError> {
    match form.kind.as_str() {
        "fixed" => DecimalAmount::parse(form.amount.trim())
            .and_then(|amount| amount.to_minor_units(currency))
            .ok()
            .filter(|minor_units| *minor_units >= 0)
            .map(|minor_units| EmergencyFundTarget::Fixed { minor_units })
            .ok_or_else(|| {
                PageError::invalid(
                    "an amount is a number of zero or more such as 10000, with no more \
                     decimals than its currency has",
                )
            }),
        "months" => form
            .months
            .trim()
            .parse::<u8>()
            .ok()
            .filter(|months| (1..=MAX_EMERGENCY_FUND_MONTHS).contains(months))
            .map(|months| EmergencyFundTarget::MonthsOfOutflows { months })
            .ok_or_else(|| PageError::invalid("a number of months is a whole number from 1 to 60")),
        _ => Err(PageError::invalid("not a kind of target the form offers")),
    }
}

/// Sets the chosen currency's target, replacing any it had. The policy is checked before
/// the form is even read, so a member is refused whatever they send.
async fn set(
    State(state): State<AppState>,
    session: PageSession,
    form: Result<PageForm<SetForm>, PageError>,
) -> Result<Redirect, PageError> {
    authorize(&session.actor(), Action::ManageHousehold).map_err(AuthError::from)?;
    let PageForm(form) = form?;
    let actor = session.verify(&state, &form.csrf)?;
    let currency = currency_of(&form.currency)?;
    let target = target_of(&form, currency)?;
    state
        .ledger
        .set_emergency_fund(&actor, currency, target)
        .await
        .map_err(refused)?;
    Ok(Redirect::to(BACK))
}

/// Removes `currency`'s target. A stale form removing one already gone says so, as a page
/// that isn't found.
async fn remove(
    State(state): State<AppState>,
    session: PageSession,
    currency: Result<Path<String>, PathRejection>,
    form: Result<PageForm<RemoveForm>, PageError>,
) -> Result<Redirect, PageError> {
    authorize(&session.actor(), Action::ManageHousehold).map_err(AuthError::from)?;
    let PageForm(form) = form?;
    let actor = session.verify(&state, &form.csrf)?;
    let Path(currency) = currency.map_err(|_| PageError::invalid("not a currency coffer knows"))?;
    let currency = currency_of(&currency)?;
    match state.ledger.remove_emergency_fund(&actor, currency).await {
        Ok(_) => Ok(Redirect::to(BACK)),
        Err(LedgerError::NotFound) => Err(PageError(
            Problem::not_found().with_detail("That emergency fund was already removed.".to_owned()),
        )),
        Err(err) => Err(refused(err)),
    }
}

#[cfg(test)]
mod tests {
    use coffer_domain::money::Currency;
    use coffer_domain::plan::EmergencyFundTarget;

    use super::described;

    #[test]
    fn a_target_reads_in_its_currency_or_in_months() {
        let usd = Currency::parse("USD").unwrap();
        let jpy = Currency::parse("JPY").unwrap();
        assert_eq!(
            described(
                usd,
                EmergencyFundTarget::Fixed {
                    minor_units: 1_000_000
                }
            ),
            "10,000.00"
        );
        assert_eq!(
            described(jpy, EmergencyFundTarget::Fixed { minor_units: 1_000 }),
            "1,000"
        );
        assert_eq!(
            described(usd, EmergencyFundTarget::MonthsOfOutflows { months: 1 }),
            "1 month of planned spending"
        );
        assert_eq!(
            described(usd, EmergencyFundTarget::MonthsOfOutflows { months: 3 }),
            "3 months of planned spending"
        );
    }
}
