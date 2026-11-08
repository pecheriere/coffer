//! Changing an account from its page, and closing or reopening it, through
//! [`Ledger::update_account`](crate::ledger::Ledger::update_account) as
//! `PATCH /api/v1/accounts/{id}` does: the service decides what a change of kind or of whose
//! money it is does to the rest, and audits it. A field sent back as the page showed it asks for
//! nothing, and the form carries what it showed, so a form older than the account changes
//! nothing.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use coffer_domain::accounts::{AccountKind, BelongsTo, LiquidityClass};
use coffer_domain::ids::AccountId;
use maud::{Markup, html};
use serde::Deserialize;

use super::detail::render_account;
use super::fields::{
    BELONGS_TO_UNKNOWN, KIND_UNKNOWN, Shown, asked_beneficiary, held_for_field, held_for_fine,
    kind_field, kind_label, parse_belongs_to, parse_kind, shown_fields, whose_field,
};
use super::{Notice, account_path, notice_for, page_error};
use crate::http::AppState;
use crate::http::pages::layout::csrf_field;
use crate::http::pages::net_worth::liquidity_name;
use crate::http::pages::{PageError, PageForm, PageSession};
use crate::ledger::accounts::{AccountPatch, AccountView, assert_no_debug};

const CHANGED: &str =
    "This account changed after the page opened, so this save wasn't applied. Look at it again.";
const LIQUIDITY_UNKNOWN: &str = "Choose how spendable it is from the list.";
const NOT_AN_ANSWER: &str = "That isn't an answer the form offers.";
/// The longest account name, in characters; the form's `maxlength`.
const NAME_CHARS: usize = 100;
/// The form's own id, for the ids of its fields.
const ROW: &str = "edit";

/// The form changing `view`.
pub(super) fn card(view: &AccountView, csrf: &str) -> Markup {
    let kinds = html! {
        @for kind in AccountKind::ALL {
            option value=(kind.as_str()) selected[view.kind == kind] { (kind_label(kind)) }
        }
    };
    let household = view.belongs_to == BelongsTo::Household;
    html! {
        section.card id="change-account" {
            h2 { "Change this account" }
            form method="post" action=(account_path(view)) {
                (csrf_field(csrf))
                (shown_fields(view))
                input type="hidden" name="shown_name" value=(view.name);
                input type="hidden" name="shown_liquidity" value=(view.liquidity.as_str());
                input type="hidden" name="shown_liquidity_chosen" value=(view.liquidity_overridden);
                input type="hidden" name="shown_net_worth" value=(view.include_in_net_worth);
                input type="hidden" name="shown_forecast" value=(view.include_in_forecast);
                label.field {
                    "Name"
                    input .input type="text" name="name" value=(view.name)
                        maxlength=(NAME_CHARS) required;
                }
                (kind_field(ROW, &kinds, None))
                (liquidity_field(view))
                (whose_field(ROW, view))
                (held_for_field(ROW, view.beneficiary.as_deref(), &held_for_fine(view)))
                fieldset.choices {
                    legend { "Counting" }
                    @if household {
                        label.choice {
                            input type="checkbox" name="net_worth" value="yes"
                                checked[view.include_in_net_worth];
                            "Count it in net worth"
                        }
                        label.choice {
                            input type="checkbox" name="forecast" value="yes"
                                checked[view.include_in_forecast];
                            "Count it in the forecast"
                        }
                        p.fine {
                            "The forecast counts only cash and debts due soon, so its box changes "
                            "nothing for other accounts. Saying the money isn't the household's "
                            "leaves it out of both, whatever is ticked."
                        }
                    } @else {
                        p.fine {
                            "It isn't the household's money, so it is left out of net worth and "
                            "the forecast. Say it's the household's to count it."
                        }
                    }
                }
                button.btn.btn-primary type="submit" { "Save changes" }
            }
        }
    }
}

/// The select of liquidity classes. One already picked stays picked, since a change can't make
/// it follow the kind again; one following the kind is the empty answer.
fn liquidity_field(view: &AccountView) -> Markup {
    let fine = if view.liquidity_overridden {
        "You picked this, so it stays whatever the kind. Net worth groups accounts by it, and the \
         forecast counts only cash and debts due soon."
    } else {
        "It follows the kind now. Picking one, even the one it has, keeps it whatever the kind. \
         Net worth groups accounts by it, and the forecast counts only cash and debts due soon."
    };
    html! {
        div.field {
            label for="liquidity-edit" { "How spendable it is" }
            select .input #liquidity-edit name="liquidity" aria-describedby="liquidity-fine-edit" {
                @if !view.liquidity_overridden {
                    option value="" selected {
                        "Follow the kind (now " (liquidity_name(view.liquidity)) ")"
                    }
                }
                @for class in LiquidityClass::ALL {
                    option value=(class.as_str())
                        selected[view.liquidity_overridden && view.liquidity == class] {
                        (liquidity_name(class))
                    }
                }
            }
            span.fine #liquidity-fine-edit { (fine) }
        }
    }
}

/// Closing or reopening `view`.
pub(super) fn closing_card(view: &AccountView, csrf: &str) -> Markup {
    html! {
        section.card id="closing" {
            @if view.closed {
                h2 { "Reopen this account" }
                p { "Reopening puts it back in the lists, and net worth counts the days since it closed again." }
            } @else {
                h2 { "Close this account" }
                p {
                    "Closing moves it to the closed accounts. Net worth counts it up to the day it "
                    "closes, and after that only while it owes money. The forecast leaves it out, "
                    "unless it is a card still owing. Its balances and transactions stay, and you "
                    "can reopen it."
                }
            }
            form method="post" action={ (account_path(view)) "/closing" } {
                (csrf_field(csrf))
                input type="hidden" name="closed" value=(!view.closed);
                button.btn type="submit" {
                    @if view.closed { "Reopen account" } @else { "Close account" }
                }
            }
        }
    }
}

/// `shown_*` is what the page showed when it was drawn. Unknown fields are refused: a forged
/// form fails whole. It has no `Debug`, so the label can't reach a log line.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EditForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    shown_kind: String,
    #[serde(default)]
    shown_belongs_to: String,
    #[serde(default)]
    shown_chosen: String,
    #[serde(default)]
    shown_beneficiary: String,
    #[serde(default)]
    shown_name: String,
    #[serde(default)]
    shown_liquidity: String,
    #[serde(default)]
    shown_liquidity_chosen: String,
    #[serde(default)]
    shown_net_worth: String,
    #[serde(default)]
    shown_forecast: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    liquidity: String,
    #[serde(default)]
    belongs_to: String,
    #[serde(default)]
    beneficiary: Option<String>,
    #[serde(default)]
    net_worth: String,
    #[serde(default)]
    forecast: String,
}

assert_no_debug!(EditForm);

/// A ticked box, or the fixed words of an answer the form doesn't offer.
fn ticked(raw: &str) -> Result<bool, &'static str> {
    match raw {
        "" => Ok(false),
        "yes" => Ok(true),
        _ => Err(NOT_AN_ANSWER),
    }
}

impl EditForm {
    /// Whether the page this came from showed the account as it is now: what the form's words
    /// said a change would do depends on all of it.
    fn showed(&self, current: &AccountView) -> bool {
        let shown = Shown {
            kind: &self.shown_kind,
            belongs_to: &self.shown_belongs_to,
            chosen: &self.shown_chosen,
            beneficiary: &self.shown_beneficiary,
        };
        shown.matches(current)
            && self.shown_name == current.name
            && self.shown_liquidity == current.liquidity.as_str()
            && self.shown_liquidity_chosen == current.liquidity_overridden.to_string()
            && self.shown_net_worth == current.include_in_net_worth.to_string()
            && self.shown_forecast == current.include_in_forecast.to_string()
    }

    /// The change asked of `current`, which the page showed: only what differs from that. The
    /// name is asked only when it differs from the one the page showed.
    fn patch(self, current: &AccountView) -> Result<AccountPatch, &'static str> {
        let kind = parse_kind(&self.kind).map_err(|_| KIND_UNKNOWN)?;
        let liquidity = match self.liquidity.as_str() {
            "" => None,
            raw => Some(
                raw.parse::<LiquidityClass>()
                    .map_err(|_| LIQUIDITY_UNKNOWN)?,
            ),
        };
        let belongs_to = parse_belongs_to(&self.belongs_to).map_err(|_| BELONGS_TO_UNKNOWN)?;
        let changed = |ticked: bool, shown: bool| (ticked != shown).then_some(ticked);
        Ok(AccountPatch {
            name: Some(self.name).filter(|name| name.trim() != self.shown_name),
            kind: kind.filter(|kind| *kind != current.kind),
            liquidity: liquidity
                .filter(|class| !(current.liquidity_overridden && *class == current.liquidity)),
            include_in_net_worth: changed(ticked(&self.net_worth)?, current.include_in_net_worth),
            include_in_forecast: changed(ticked(&self.forecast)?, current.include_in_forecast),
            closed: None,
            belongs_to,
            beneficiary: asked_beneficiary(self.beneficiary, &self.shown_beneficiary),
        })
    }
}

fn account_id(raw: &str) -> Result<AccountId, PageError> {
    raw.parse().map_err(|_| PageError::not_found())
}

/// A hidden or unknown account is decided before the form is read.
pub(super) async fn edit(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    PageForm(form): PageForm<EditForm>,
) -> Result<Response, PageError> {
    let actor = session.verify(&state, &form.csrf)?;
    let id = account_id(&id)?;
    let current = state
        .ledger
        .get_account(&actor, id)
        .await
        .map_err(page_error)?;
    if !form.showed(&current) {
        let notice = Notice::new(StatusCode::CONFLICT, CHANGED);
        return render_account(&state, &session, id, None, Some(notice)).await;
    }
    let patch = match form.patch(&current) {
        Ok(patch) => patch,
        Err(message) => {
            let notice = Notice::new(StatusCode::UNPROCESSABLE_ENTITY, message);
            return render_account(&state, &session, id, None, Some(notice)).await;
        }
    };
    match state.ledger.update_account(&actor, id, patch).await {
        Ok(view) => Ok(Redirect::to(&account_path(&view)).into_response()),
        Err(err) => render_account(&state, &session, id, None, Some(notice_for(err)?)).await,
    }
}

/// Unknown fields are refused: a forged form fails whole.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ClosingForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    closed: String,
}

pub(super) async fn closing(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    PageForm(form): PageForm<ClosingForm>,
) -> Result<Response, PageError> {
    let actor = session.verify(&state, &form.csrf)?;
    let id = account_id(&id)?;
    let closed = match form.closed.as_str() {
        "true" => true,
        "false" => false,
        _ => return Err(PageError::invalid("not an answer the form offers")),
    };
    let patch = AccountPatch {
        closed: Some(closed),
        ..AccountPatch::default()
    };
    match state.ledger.update_account(&actor, id, patch).await {
        Ok(view) => Ok(Redirect::to(&account_path(&view)).into_response()),
        Err(err) => render_account(&state, &session, id, None, Some(notice_for(err)?)).await,
    }
}
