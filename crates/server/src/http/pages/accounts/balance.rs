//! Recording an account's balance for a day from its page, through
//! [`Ledger::set_manual_balance`](crate::ledger::Ledger::set_manual_balance), as
//! `PUT /api/v1/accounts/{id}/balances/{on}` does. The amount is typed as a decimal and read
//! exactly in the account's currency; nothing here is a float.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use coffer_domain::ids::AccountId;
use coffer_domain::money::{Currency, DecimalAmount, MoneyError};
use coffer_domain::plan::strict_date;
use jiff::civil::Date;
use maud::{Markup, html};
use serde::Deserialize;

use super::detail::render_account;
use super::{Notice, account_path, notice_for, page_error};
use crate::http::AppState;
use crate::http::pages::layout::csrf_field;
use crate::http::pages::{PageError, PageForm, PageSession};
use crate::ledger::accounts::{AccountSource, AccountView};

/// Under every box taking an amount.
pub(super) const AMOUNT_HINT: &str =
    "Such as 1234.56, in the account's currency. Negative is money owed.";
const AMOUNT_MALFORMED: &str =
    "A balance is digits with an optional minus sign and decimal point, such as -1234.56.";
const AMOUNT_TOO_PRECISE: &str = "The balance has more decimal places than its currency allows.";
const AMOUNT_TOO_LARGE: &str = "The balance is too large.";
const DAY_INVALID: &str = "Choose a real day for the balance.";

/// `text` in minor units of `currency`, read exactly, or the fixed words of what is wrong.
pub(super) fn minor_units(text: &str, currency: Currency) -> Result<i64, &'static str> {
    DecimalAmount::parse(text)
        .and_then(|amount| amount.to_minor_units(currency))
        .map_err(|err| match err {
            MoneyError::TooPrecise => AMOUNT_TOO_PRECISE,
            MoneyError::Overflow => AMOUNT_TOO_LARGE,
            MoneyError::Malformed | MoneyError::UnknownCurrency => AMOUNT_MALFORMED,
        })
}

/// The card with the form, on the page of an account the actor may change.
pub(super) fn card(view: &AccountView, csrf: &str, today: Date) -> Markup {
    let currency = view.currency.as_str();
    html! {
        section.card id="record-balance" {
            h2 { "Record a balance" }
            p {
                "The balance at the end of a day. It replaces any balance already on that day, "
                "and the audit log keeps both."
                @if view.source == AccountSource::Linked {
                    " This account is linked to a bank: its next sync replaces the balance of the "
                    "day it reports."
                }
            }
            form method="post" action={ (account_path(view)) "/balances" } {
                (csrf_field(csrf))
                label.field {
                    "Day"
                    input .input type="date" name="on" required value=(today.to_string());
                }
                div.field {
                    label for="balance-amount" { "Balance in " (currency) }
                    input .input #balance-amount type="text" name="amount" required
                        inputmode="decimal" autocomplete="off" aria-describedby="balance-fine";
                    span.fine #balance-fine { (AMOUNT_HINT) }
                }
                button.btn.btn-primary type="submit" { "Save balance" }
            }
        }
    }
}

/// Unknown fields are refused: a forged form fails whole.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BalanceForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    on: String,
    #[serde(default)]
    amount: String,
}

/// A hidden or unknown account is decided before the form is read, and the amount is read in
/// the account's own currency.
pub(super) async fn record(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    PageForm(form): PageForm<BalanceForm>,
) -> Result<Response, PageError> {
    let actor = session.verify(&state, &form.csrf)?;
    let id: AccountId = id.parse().map_err(|_| PageError::not_found())?;
    let account = state
        .ledger
        .get_account(&actor, id)
        .await
        .map_err(page_error)?;
    let asked = strict_date::parse(&form.on)
        .ok_or(DAY_INVALID)
        .and_then(|on| Ok((on, minor_units(form.amount.trim(), account.currency)?)));
    let (on, minor) = match asked {
        Ok(asked) => asked,
        Err(message) => {
            let notice = Notice::new(StatusCode::UNPROCESSABLE_ENTITY, message);
            return render_account(&state, &session, id, None, Some(notice)).await;
        }
    };
    match state.ledger.set_manual_balance(&actor, id, on, minor).await {
        Ok(_) => Ok(Redirect::to(&account_path(&account)).into_response()),
        Err(err) => render_account(&state, &session, id, None, Some(notice_for(err)?)).await,
    }
}

#[cfg(test)]
mod tests {
    use coffer_domain::money::Currency;

    use super::{AMOUNT_MALFORMED, AMOUNT_TOO_LARGE, AMOUNT_TOO_PRECISE, minor_units};

    #[test]
    fn a_balance_is_read_exactly_in_its_currency() {
        let usd = Currency::parse("USD").unwrap();
        let jpy = Currency::parse("JPY").unwrap();
        assert_eq!(minor_units("1234.56", usd), Ok(123_456));
        assert_eq!(minor_units("-0.10", usd), Ok(-10));
        assert_eq!(minor_units("0.1", usd), Ok(10));
        assert_eq!(minor_units("1234", jpy), Ok(1234));
        for (text, currency, refused) in [
            ("12.345", usd, AMOUNT_TOO_PRECISE),
            ("1.5", jpy, AMOUNT_TOO_PRECISE),
            ("99999999999999999.99", usd, AMOUNT_TOO_LARGE),
            ("1,234.56", usd, AMOUNT_MALFORMED),
            ("1e3", usd, AMOUNT_MALFORMED),
            ("+5", usd, AMOUNT_MALFORMED),
            ("", usd, AMOUNT_MALFORMED),
        ] {
            assert_eq!(minor_units(text, currency), Err(refused), "{text:?}");
        }
    }
}
