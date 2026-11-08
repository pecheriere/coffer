//! Creating a manual account from the list, with every field `POST /api/v1/accounts` takes, through
//! [`Ledger::create_account`](crate::ledger::Ledger::create_account), which decides whose money it
//! is and audits it. The form is left out for an actor the service would refuse, and the same
//! check comes before the form is read.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use coffer_domain::accounts::{AccountKind, LiquidityClass};
use coffer_domain::identity::Actor;
use coffer_domain::money::Currency;
use coffer_domain::plan::strict_date;
use coffer_domain::policy::{AccountLevel, Action, authorize};
use jiff::civil::Date;
use maud::{Markup, html};
use serde::Deserialize;

use super::balance::{AMOUNT_HINT, minor_units};
use super::fields::{
    BELONGS_TO_UNKNOWN, KIND_UNKNOWN, NOT_THE_HOUSEHOLD_S, SEEN_BY, answer_options, held_for_field,
    kind_field, kind_label, parse_belongs_to, parse_kind,
};
use super::{Notice, account_path, notice_for, page_error, render_list};
use crate::auth::AuthError;
use crate::http::AppState;
use crate::http::pages::layout::csrf_field;
use crate::http::pages::net_worth::liquidity_name;
use crate::http::pages::{PageError, PageForm, PageSession};
use crate::ledger::LedgerError;
use crate::ledger::accounts::{NewAccountInput, OpeningBalance, assert_no_debug};

const CURRENCY_UNKNOWN: &str = "Choose a currency from the list.";
const LIQUIDITY_UNKNOWN: &str = "Choose how spendable it is from the list.";
const NOT_AN_ANSWER: &str = "That isn't an answer the form offers.";
const OPENING_DAY_INVALID: &str = "The opening balance needs a real day.";
/// The longest account name, in characters; the form's `maxlength`.
const NAME_CHARS: usize = 100;
/// The form's own id, for the ids of its fields.
const ROW: &str = "new";

/// The card with the form, for an actor who may create an account.
pub(super) struct CreateCard {
    allowed: bool,
    today: Date,
    display: Option<Currency>,
}

impl CreateCard {
    /// What the card needs, asking the services what `actor` may do.
    pub(super) async fn load(state: &AppState, actor: &Actor) -> Result<Self, PageError> {
        let allowed = authorize(actor, Action::WriteHousehold).is_ok()
            && match state
                .ledger
                .authorize_every_account_access(actor, AccountLevel::Write)
                .await
            {
                Ok(()) => true,
                Err(LedgerError::Denied(_)) => false,
                Err(err) => return Err(page_error(err)),
            };
        let today = state.ledger.today().await.map_err(page_error)?;
        let display = state
            .ledger
            .household_settings(actor)
            .await
            .map_err(page_error)?
            .display_currency;
        Ok(Self {
            allowed,
            today,
            display,
        })
    }

    pub(super) fn allowed(&self) -> bool {
        self.allowed
    }

    pub(super) fn card(&self, csrf: &str) -> Markup {
        if !self.allowed {
            return html! {};
        }
        let kinds = html! {
            option value="" selected disabled { "Choose a kind" }
            @for kind in AccountKind::ALL {
                option value=(kind.as_str()) { (kind_label(kind)) }
            }
        };
        html! {
            section.card id="new-account" {
                h2 { "New account kept by hand" }
                p {
                    "For an account no bank feeds, such as a house, a car, a loan or a 529 you "
                    "update yourself. To have a bank feed an account, connect the bank on "
                    a href="/connections" { "Connections" } "."
                }
                form method="post" action=(super::LIST) {
                    (csrf_field(csrf))
                    label.field {
                        "Name"
                        input .input type="text" name="name" maxlength=(NAME_CHARS) required;
                    }
                    (kind_field(ROW, &kinds, None))
                    div.field {
                        label for="currency-new" { "Currency" }
                        select .input #currency-new name="currency" required
                            aria-describedby="currency-fine-new" {
                            @if self.display.is_none() {
                                option value="" selected disabled { "Choose a currency" }
                            }
                            @for currency in Currency::all() {
                                option value=(currency.as_str())
                                    selected[self.display == Some(currency)] {
                                    (currency.as_str())
                                }
                            }
                        }
                        span.fine #currency-fine-new { "It can't change later." }
                    }
                    (liquidity_field())
                    div.field {
                        label for="whose-new" { "Whose money is it?" }
                        select .input #whose-new name="belongs_to"
                            aria-describedby="whose-fine-new" {
                            option value="" selected { "Follow the kind" }
                            (answer_options())
                        }
                        span.fine #whose-fine-new {
                            "Following the kind, an education or custodial account is held for "
                            "someone else and a charitable one is given away. "
                            (NOT_THE_HOUSEHOLD_S)
                        }
                    }
                    (held_for_field(ROW, None, &format!("{SEEN_BY} Leave it empty if this is the household's money.")))
                    fieldset.choices {
                        legend { "Counting" }
                        label.choice {
                            input type="checkbox" name="leave_out_net_worth" value="yes";
                            "Leave it out of net worth"
                        }
                        label.choice {
                            input type="checkbox" name="leave_out_forecast" value="yes";
                            "Leave it out of the forecast"
                        }
                        p.fine {
                            "Money that isn't the household's is left out of both whatever you "
                            "tick. The forecast counts only cash and debts due soon."
                        }
                    }
                    fieldset.choices {
                        legend { "Opening balance (optional)" }
                        div.field {
                            label for="opening-amount" { "Balance" }
                            input .input #opening-amount type="text" name="opening_amount"
                                inputmode="decimal" autocomplete="off"
                                aria-describedby="opening-fine";
                            span.fine #opening-fine { (AMOUNT_HINT) " Leave it empty to add one later." }
                        }
                        label.field {
                            "At the end of"
                            input .input type="date" name="opening_on" value=(self.today.to_string());
                        }
                    }
                    button.btn.btn-primary type="submit" { "Create account" }
                }
            }
        }
    }
}

/// The select of liquidity classes on the new account's form, following the kind by default.
fn liquidity_field() -> Markup {
    html! {
        div.field {
            label for="liquidity-new" { "How spendable it is" }
            select .input #liquidity-new name="liquidity" aria-describedby="liquidity-fine-new" {
                option value="" selected { "Follow the kind" }
                @for class in LiquidityClass::ALL {
                    option value=(class.as_str()) { (liquidity_name(class)) }
                }
            }
            span.fine #liquidity-fine-new {
                "Net worth groups accounts by it. Following the kind, it changes when the kind "
                "does; one you pick stays."
            }
        }
    }
}

/// Unknown fields are refused: a forged form fails whole. It has no `Debug`, so the label can't
/// reach a log line.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    currency: String,
    #[serde(default)]
    liquidity: String,
    #[serde(default)]
    belongs_to: String,
    #[serde(default)]
    beneficiary: String,
    #[serde(default)]
    leave_out_net_worth: String,
    #[serde(default)]
    leave_out_forecast: String,
    #[serde(default)]
    opening_amount: String,
    #[serde(default)]
    opening_on: String,
}

assert_no_debug!(CreateForm);

/// A box ticked to leave the account out: unticked asks nothing, so whose money it is decides.
fn left_out(raw: &str) -> Result<Option<bool>, &'static str> {
    match raw {
        "" => Ok(None),
        "yes" => Ok(Some(false)),
        _ => Err(NOT_AN_ANSWER),
    }
}

impl CreateForm {
    /// What the form asks, read exactly, or the fixed words of what it got wrong.
    fn input(self) -> Result<NewAccountInput, &'static str> {
        let Ok(Some(kind)) = parse_kind(&self.kind) else {
            return Err(KIND_UNKNOWN);
        };
        let currency = Currency::parse(&self.currency).map_err(|_| CURRENCY_UNKNOWN)?;
        let liquidity = match self.liquidity.as_str() {
            "" => None,
            raw => Some(
                raw.parse::<LiquidityClass>()
                    .map_err(|_| LIQUIDITY_UNKNOWN)?,
            ),
        };
        let belongs_to = parse_belongs_to(&self.belongs_to).map_err(|_| BELONGS_TO_UNKNOWN)?;
        let opening_balance = match self.opening_amount.trim() {
            "" => None,
            amount => Some(OpeningBalance {
                on: strict_date::parse(&self.opening_on).ok_or(OPENING_DAY_INVALID)?,
                minor_units: minor_units(amount, currency)?,
            }),
        };
        Ok(NewAccountInput {
            name: self.name,
            kind,
            currency,
            liquidity,
            include_in_net_worth: left_out(&self.leave_out_net_worth)?,
            include_in_forecast: left_out(&self.leave_out_forecast)?,
            belongs_to,
            beneficiary: Some(self.beneficiary),
            opening_balance,
        })
    }
}

/// The service's own check of who may create comes before the form is read, so an actor it
/// would refuse is refused whatever they send.
pub(super) async fn create(
    State(state): State<AppState>,
    session: PageSession,
    form: Result<PageForm<CreateForm>, PageError>,
) -> Result<Response, PageError> {
    authorize(&session.actor(), Action::WriteHousehold).map_err(AuthError::from)?;
    state
        .ledger
        .authorize_every_account_access(&session.actor(), AccountLevel::Write)
        .await
        .map_err(page_error)?;
    let PageForm(form) = form?;
    let actor = session.verify(&state, &form.csrf)?;
    let input = match form.input() {
        Ok(input) => input,
        Err(message) => {
            let notice = Notice::new(StatusCode::UNPROCESSABLE_ENTITY, message);
            return render_list(&state, &session, Some(notice)).await;
        }
    };
    match state.ledger.create_account(&actor, input, None).await {
        Ok(view) => Ok(Redirect::to(&account_path(&view)).into_response()),
        Err(err) => render_list(&state, &session, Some(notice_for(err)?)).await,
    }
}
