//! Transactions entered by hand, on an open account no provider feeds: the form that enters one,
//! as `POST /api/v1/transactions` does, and the one that deletes it, as
//! `DELETE /api/v1/transactions/{id}` does. The service decides which accounts take one.

use axum::extract::{Path, State};
use axum::response::Redirect;
use coffer_domain::ids::AccountId;
use coffer_domain::money::DecimalAmount;
use coffer_domain::plan::strict_date;
use maud::{Markup, html};
use serde::Deserialize;

use super::{LIST, Names, detail_path, refused, transaction_id};
use crate::http::AppState;
use crate::http::pages::layout::csrf_field;
use crate::http::pages::{PageError, PageForm, PageSession};
use crate::ledger::accounts::AccountSource;
use crate::ledger::transactions::NewManualTransactionInput;

/// The form, on the list, offering the open accounts kept by hand; nothing when there are none.
pub(super) fn form(names: &Names, csrf: &str) -> Markup {
    let accounts: Vec<_> = names
        .accounts
        .iter()
        .filter(|account| account.source == AccountSource::Manual && !account.closed)
        .collect();
    if accounts.is_empty() {
        return html! {};
    }
    html! {
        section.card id="enter" {
            h2 { "Enter a transaction by hand" }
            p.fine {
                "For an account kept by hand. It starts unreviewed and uncategorized, in the "
                "account's currency, and the audit log records it."
            }
            form method="post" action=(LIST) {
                (csrf_field(csrf))
                label.field {
                    "Account"
                    select.input name="account" required {
                        option value="" selected { "Choose an account" }
                        @for account in &accounts {
                            option value=(account.id) {
                                (account.name) ", " (account.currency.as_str())
                            }
                        }
                    }
                }
                label.field {
                    "Day"
                    input.input type="date" name="posted_on" required;
                }
                fieldset.choices {
                    legend { "Which way" }
                    label.choice {
                        input type="radio" name="direction" value="out" required checked;
                        "Money out"
                    }
                    label.choice {
                        input type="radio" name="direction" value="in";
                        "Money in"
                    }
                }
                label.field {
                    "Amount, such as 12.50"
                    input.input type="text" name="amount" required inputmode="decimal"
                        autocomplete="off";
                }
                label.field {
                    "Description"
                    input.input type="text" name="description" required maxlength="500"
                        autocomplete="off";
                }
                label.field {
                    "Payee, if you like"
                    input.input type="text" name="payee" maxlength="200" autocomplete="off";
                }
                label.field {
                    "Notes, if you like"
                    textarea.input name="notes" maxlength="2000" rows="2" {}
                }
                button.btn.btn-primary type="submit" { "Enter it" }
            }
        }
    }
}

/// Unknown fields are refused: a forged form fails whole.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    account: String,
    #[serde(default)]
    posted_on: String,
    #[serde(default)]
    direction: String,
    #[serde(default)]
    amount: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    payee: String,
    #[serde(default)]
    notes: String,
}

/// A text a form may leave empty: empty is none.
fn optional(text: &str) -> Option<String> {
    (!text.is_empty()).then(|| text.to_owned())
}

/// Enters the transaction, then shows it. The amount is read exactly in the account's
/// currency, which the account the actor sees says; the service checks it again.
pub(super) async fn create(
    State(state): State<AppState>,
    session: PageSession,
    PageForm(form): PageForm<CreateForm>,
) -> Result<Redirect, PageError> {
    let actor = session.verify(&state, &form.csrf)?;
    let account: AccountId = form
        .account
        .parse()
        .map_err(|_| PageError::invalid("choose an account"))?;
    let posted_on =
        strict_date::parse(&form.posted_on).ok_or_else(|| PageError::invalid("not a real day"))?;
    let out = match form.direction.as_str() {
        "out" => true,
        "in" => false,
        _ => return Err(PageError::invalid("not an answer the form offers")),
    };
    let currency = state
        .ledger
        .get_account(&actor, account)
        .await
        .map_err(refused)?
        .currency;
    let amount = DecimalAmount::parse(form.amount.trim())
        .ok()
        .filter(|amount| !amount.as_str().starts_with('-'))
        .ok_or_else(|| PageError::invalid("an amount is a number such as 12.50, without a sign"))?;
    let amount = if out { amount.negated() } else { amount };
    let minor_units = amount.to_minor_units(currency).map_err(|_| {
        PageError::invalid("an amount has no more decimals than its currency, and isn't too large")
    })?;
    let created = state
        .ledger
        .create_manual_transaction(
            &actor,
            NewManualTransactionInput {
                account,
                posted_on,
                minor_units,
                description: form.description,
                payee: optional(&form.payee),
                memo: None,
                notes: optional(&form.notes.replace("\r\n", "\n")),
            },
            None,
        )
        .await
        .map_err(refused)?;
    Ok(Redirect::to(&detail_path(created.id)))
}

/// Unknown fields are refused: a forged form fails whole.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DeleteForm {
    #[serde(default)]
    csrf: String,
}

pub(super) async fn delete(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    PageForm(form): PageForm<DeleteForm>,
) -> Result<Redirect, PageError> {
    let actor = session.verify(&state, &form.csrf)?;
    let id = transaction_id(&id)?;
    state
        .ledger
        .delete_transaction(&actor, id)
        .await
        .map_err(refused)?;
    Ok(Redirect::to(LIST))
}
