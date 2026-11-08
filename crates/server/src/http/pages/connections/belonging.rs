//! What a bound account is and whose money it is, from its row on the page: what the row says,
//! the form that changes it and the labels the forms offer. The form asks
//! [`Ledger::update_account`](crate::ledger::Ledger::update_account), which decides and audits
//! (spec §8.11 B2, B3, B4, B5). A label may be a child's name: it goes only to the service and
//! to who sees its account, in the row, the form and the list of labels. A refusal holds fixed
//! words, and nothing here logs a field. The form carries what it showed, so a form older than the
//! account changes nothing.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use coffer_domain::accounts::AccountKind;
use coffer_domain::ids::AccountId;
use maud::{Markup, html};
use serde::Deserialize;

use super::bind::unprocessable;
use super::{FormKind, Notice, Shared, back, dismiss, refused, render};
use crate::http::AppState;
use crate::http::pages::accounts::fields::{
    BELONGS_TO_UNKNOWN, KIND_UNKNOWN, Shown, answer_label, asked_beneficiary, form_words,
    held_for_field, held_for_fine, kind_field, kind_label, parse_belongs_to, parse_kind,
    shown_fields, whose_field, whose_words,
};
use crate::http::pages::layout::csrf_field;
use crate::http::pages::{PageError, PageForm, PageSession};
use crate::ledger::accounts::{AccountPatch, AccountView, assert_no_debug};
use crate::sync::SyncError;
use crate::sync::connections::{ConnectionView, ProviderAccountView};

const CHANGED: &str =
    "This account changed after the page opened, so this save wasn't applied. Look at it again.";

pub(super) fn account_item(
    connection: &ConnectionView,
    account: &ProviderAccountView,
    shared: &Shared<'_>,
    open: bool,
) -> Markup {
    let view = account
        .account
        .and_then(|bound| shared.accounts.get(&bound).copied());
    html! {
        li.item {
            div.item-main {
                span.item-title { (account.name) }
                span.item-meta {
                    @if let Some(institution) = &account.institution {
                        (institution) " · "
                    }
                    (account.currency)
                    @if let Some(words) = view.and_then(whose_words) {
                        " · " (words)
                    }
                }
                @if let Some(error) = &account.error {
                    span.item-meta { "Error: " (error) }
                }
                @if let Some(note) = &account.old_balance_note {
                    span.item-meta { (note) }
                }
            }
            @if account.account.is_some() {
                span.chip {
                    "Bound to "
                    @if let Some(view) = view {
                        a href={ "/accounts/" (view.id) } { (view.name) }
                    } @else {
                        "an account"
                    }
                }
            } @else if account.dismissed_at.is_some() {
                span.chip.chip-quiet { "Dismissed" }
            } @else {
                span.chip.chip-quiet { "Not bound" }
            }
            (dismiss::forms(connection, account, shared.csrf, shared.binding.may_dismiss()))
            (shared.binding.forms(connection, account, shared.csrf, open, shared.refused_row == Some(account.id)))
            @if let Some(view) = view.filter(|view| shared.binding.may_change(view.id)) {
                (row_form(account, view, shared.csrf))
            }
        }
    }
}

/// The form changing what `view` is and whose money it is.
fn row_form(account: &ProviderAccountView, view: &AccountView, csrf: &str) -> Markup {
    let row = account.id;
    html! {
        details.item-detail {
            summary { "Kind and whose money it is" }
            p.fine {
                "Now: " (kind_label(view.kind)) " · " (answer_label(view.belongs_to))
            }
            form method="post" action={ "/connections/accounts/" (view.id) "/belonging" } {
                (csrf_field(csrf))
                (shown_fields(view))
                (kind_field(row, &html! {
                    @for kind in AccountKind::ALL {
                        option value=(kind.as_str()) selected[view.kind == kind] {
                            (kind_label(kind))
                        }
                    }
                }, None))
                (whose_field(row, view))
                (held_for_field(row, view.beneficiary.as_deref(), &held_for_fine(view)))
                button.btn type="submit" { "Save" }
            }
        }
    }
}

/// `belongs_to` empty leaves it, unlike the bind form's "Follow the kind": a patch can't return
/// a chosen answer to following the kind. `shown_*` is what the page showed when it was drawn.
/// Unknown fields are refused: a forged form fails whole.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BelongingForm {
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
    kind: String,
    #[serde(default)]
    belongs_to: String,
    #[serde(default)]
    beneficiary: Option<String>,
}

assert_no_debug!(BelongingForm);

impl BelongingForm {
    fn shown(&self) -> Shown<'_> {
        Shown {
            kind: &self.shown_kind,
            belongs_to: &self.shown_belongs_to,
            chosen: &self.shown_chosen,
            beneficiary: &self.shown_beneficiary,
        }
    }
}

/// `err` in the form's words, for the refusals of the account service.
pub(super) fn in_form_words(err: SyncError) -> SyncError {
    match err {
        SyncError::Invalid(message) => SyncError::Invalid(form_words(message)),
        other => other,
    }
}

pub(super) async fn update(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    PageForm(form): PageForm<BelongingForm>,
) -> Result<Response, PageError> {
    let actor = session.verify(&state, &form.csrf)?;
    let id: AccountId = id.parse().map_err(|_| PageError::not_found())?;
    // Hidden or unknown is decided here, before the form is read.
    let current = match state.ledger.get_account(&actor, id).await {
        Ok(current) => current,
        Err(err) => return refused(&state, &session, err.into(), FormKind::Action).await,
    };
    if !form.shown().matches(&current) {
        let notice = Notice::new(StatusCode::CONFLICT, CHANGED);
        return render(&state, &session, Some(notice)).await;
    }
    let Ok(kind) = parse_kind(&form.kind) else {
        return unprocessable(&state, &session, KIND_UNKNOWN).await;
    };
    let Ok(belongs_to) = parse_belongs_to(&form.belongs_to) else {
        return unprocessable(&state, &session, BELONGS_TO_UNKNOWN).await;
    };
    let beneficiary = asked_beneficiary(form.beneficiary, &form.shown_beneficiary);
    let patch = AccountPatch {
        kind,
        belongs_to,
        beneficiary,
        ..AccountPatch::default()
    };
    match state.ledger.update_account(&actor, id, patch).await {
        Ok(_) => Ok(back()),
        Err(err) => {
            let err = in_form_words(SyncError::from(err));
            refused(&state, &session, err, FormKind::Action).await
        }
    }
}
