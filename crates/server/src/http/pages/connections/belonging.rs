//! What a bound account is and whose money it is, from its row on the page: what the row says,
//! the form that changes it and the labels the forms offer. The form asks
//! [`Ledger::update_account`](crate::ledger::Ledger::update_account), which decides and audits
//! (spec §8.11 B2, B3, B4, B5). A label may be a child's name: it goes only to the service and
//! to who sees its account, in the row, the form and the list of labels. A refusal holds fixed
//! words, and nothing here logs a field. The form carries what it showed, so a form older than the
//! account changes nothing.

use std::collections::BTreeSet;
use std::fmt::Display;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use coffer_domain::accounts::{AccountError, AccountKind, BelongsTo};
use coffer_domain::ids::AccountId;
use maud::{Markup, html};
use serde::Deserialize;

use super::bind::{KIND_UNKNOWN, kind_label, unprocessable};
use super::{FormKind, Notice, Shared, back, refused, render};
use crate::http::AppState;
use crate::http::pages::layout::csrf_field;
use crate::http::pages::{PageError, PageForm, PageSession};
use crate::ledger::accounts::{AccountPatch, AccountView, assert_no_debug};
use crate::sync::SyncError;
use crate::sync::connections::{ConnectionView, ProviderAccountView};

pub(super) const BELONGS_TO_UNKNOWN: &str =
    "Choose one of the answers in the list for whose money it is.";
const KIND_FINE: &str = "Checking and savings count as cash in the forecast. The forecast counts \
    only cash and debts due soon. Education and custodial accounts are held for someone else, and \
    charitable ones are given away: they stay out of net worth unless you say they're the \
    household's.";
/// Who sees a label, for the hints of both forms.
pub(super) const SEEN_BY: &str = "A name or a purpose. Anyone who can see this account sees it, \
    on this page and in net worth, and it stays in the audit log.";
const HELD_FOR_INVALID: &str =
    "Held for takes a name or a purpose of up to 64 plain characters, on one line.";
const HELD_FOR_ON_HOUSEHOLD: &str =
    "Held for is only for money that isn't the household's: empty it, or choose another answer.";
const CHANGED: &str =
    "This account changed after the page opened, so this save wasn't applied. Look at it again.";
const STOPS_FOLLOWING: &str =
    "Picking an answer stops it following the kind for good, even the one it has now.";
const ANSWERS: [BelongsTo; 3] = [
    BelongsTo::Household,
    BelongsTo::SomeoneElse,
    BelongsTo::GivenAway,
];
const AUDITED: &str = "Every change goes in the audit log.";

/// How an answer to "Whose money is it?" reads.
fn answer_label(answer: BelongsTo) -> &'static str {
    match answer {
        BelongsTo::Household => "The household's",
        BelongsTo::SomeoneElse => "Someone else's, held for them",
        BelongsTo::GivenAway => "Given away",
    }
}

/// The answers every form offers. What its empty answer means differs, so each form adds that.
pub(super) fn answer_options() -> Markup {
    html! {
        @for answer in ANSWERS {
            option value=(answer.as_str()) { (answer_label(answer)) }
        }
    }
}

/// The form's answer: `""` asks nothing.
pub(super) fn parse_belongs_to(raw: &str) -> Result<Option<BelongsTo>, AccountError> {
    match raw {
        "" => Ok(None),
        raw => raw.parse().map(Some),
    }
}

/// The select of kinds, for the form of the row `row`, offering `options`. `guess` is the sentence
/// to put right under it when its choice is only a guess.
pub(super) fn kind_field(row: impl Display, options: &Markup, guess: Option<&str>) -> Markup {
    let field = format!("kind-{row}");
    let hint = format!("kind-fine-{row}");
    let guess_hint = format!("kind-guess-{row}");
    let described_by = if guess.is_some() {
        format!("{guess_hint} {hint}")
    } else {
        hint.clone()
    };
    html! {
        div.field {
            label for=(field) { "Kind" }
            select .input #(field) name="kind" required aria-describedby=(described_by) { (options) }
            @if let Some(guess) = guess {
                span.fine #(guess_hint) { (guess) }
            }
            span.fine #(hint) { (KIND_FINE) }
        }
    }
}

/// The field for the label, for the form of the row `row`. The hint is the input's description
/// and stays out of its name, which a screen reader repeats.
pub(super) fn held_for_field(row: impl Display, value: Option<&str>, fine: &str) -> Markup {
    let field = format!("held-for-{row}");
    let hint = format!("held-for-fine-{row}");
    html! {
        div.field {
            label for=(field) { "Held for (optional)" }
            input .input #(field) type="text" name="beneficiary" value=[value]
                maxlength="64" list="beneficiaries" aria-describedby=(hint);
            span.fine #(hint) { (fine) }
        }
    }
}

/// The labels of `accounts`, which is what the actor sees, once each and in a fixed order. The
/// forms' `Held for` offers them, so that one name stays one bucket.
pub(super) fn beneficiaries(accounts: &[AccountView]) -> Markup {
    let labels: BTreeSet<&str> = accounts
        .iter()
        .filter_map(|account| account.beneficiary.as_deref())
        .collect();
    html! {
        datalist #beneficiaries {
            @for label in labels {
                option value=(label) {}
            }
        }
    }
}

/// What a row says of an account that isn't the household's.
fn whose_words(view: &AccountView) -> Option<String> {
    let label = view.beneficiary.as_deref();
    match (view.belongs_to, label) {
        (BelongsTo::Household, _) => None,
        (BelongsTo::SomeoneElse, Some(label)) => Some(format!("Held for {label}")),
        (BelongsTo::SomeoneElse, None) => Some("Held for someone else".to_owned()),
        (BelongsTo::GivenAway, Some(label)) => Some(format!("Given away: {label}")),
        (BelongsTo::GivenAway, None) => Some("Given away".to_owned()),
    }
}

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
                    (view.map_or("an account", |view| view.name.as_str()))
                }
            } @else {
                span.chip.chip-quiet { "Not bound" }
            }
            (shared.binding.forms(connection, account, shared.csrf, open, shared.refused_row == Some(account.id)))
            @if let Some(view) = view.filter(|view| shared.binding.may_change(view.id)) {
                (row_form(account, view, shared.csrf))
            }
        }
    }
}

/// What the form's empty answer does for this account, and what the others do: said before it
/// happens, since the page has no script to say it after a choice.
fn row_form(account: &ProviderAccountView, view: &AccountView, csrf: &str) -> Markup {
    let row = account.id;
    let answer = format!("whose-{row}");
    let hint = format!("whose-fine-{row}");
    let leave_it = if view.belongs_to_overridden {
        "Leave it as you chose"
    } else {
        "Leave it, following the kind"
    };
    let named = view.beneficiary.is_some();
    let following = match (view.belongs_to_overridden, view.belongs_to, named) {
        (true, ..) => {
            "You chose this, so it stays whatever the kind. It can't go back to following the kind."
        }
        (false, BelongsTo::Household, _) => {
            "You haven't chosen, so this follows the kind. An education, custodial or charitable \
             kind takes it out of net worth, and out of the forecast if it's cash or a debt due \
             soon."
        }
        (false, _, true) => {
            "You haven't chosen, so this follows the kind. An education or custodial kind makes it \
             held for someone else and charitable makes it given away: it stays out of net worth \
             and keeps the name. Any other kind makes it the household's and counts it in net \
             worth, and removes the name."
        }
        (false, _, false) => {
            "You haven't chosen, so this follows the kind. An education or custodial kind makes it \
             held for someone else and charitable makes it given away: it stays out of net worth. \
             Any other kind makes it the household's and counts it in net worth."
        }
    };
    let moving = match (view.belongs_to, named) {
        (BelongsTo::Household, _) => {
            "Saying it's someone else's or given away takes it out of net worth, over its whole \
             history, and out of the forecast if it's cash or a debt due soon. If you say it's \
             the household's again later, it counts again, even where you had turned that off."
        }
        (_, true) => {
            "Saying it's the household's counts it in net worth, over its whole history, and \
             removes the name. The forecast counts it only if it's cash or a debt due soon."
        }
        (_, false) => {
            "Saying it's the household's counts it in net worth, over its whole history. The \
             forecast counts it only if it's cash or a debt due soon."
        }
    };
    let held_for_fine = match (view.belongs_to, named) {
        (BelongsTo::Household, _) => {
            "A name or a purpose, only for money that isn't the household's.".to_owned()
        }
        (_, true) => format!("{SEEN_BY} Empty it to remove the name."),
        (_, false) => SEEN_BY.to_owned(),
    };
    let shown_name = view.beneficiary.as_deref().unwrap_or_default();
    html! {
        details.item-detail {
            summary { "Kind and whose money it is" }
            p.fine {
                "Now: " (kind_label(view.kind)) " · " (answer_label(view.belongs_to))
            }
            form method="post" action={ "/connections/accounts/" (view.id) "/belonging" } {
                (csrf_field(csrf))
                input type="hidden" name="shown_kind" value=(view.kind.as_str());
                input type="hidden" name="shown_belongs_to" value=(view.belongs_to.as_str());
                input type="hidden" name="shown_chosen" value=(view.belongs_to_overridden);
                input type="hidden" name="shown_beneficiary" value=(shown_name);
                (kind_field(row, &html! {
                    @for kind in AccountKind::ALL {
                        option value=(kind.as_str()) selected[view.kind == kind] {
                            (kind_label(kind))
                        }
                    }
                }, None))
                div.field {
                    label for=(answer) { "Whose money is it?" }
                    select .input #(answer) name="belongs_to" aria-describedby=(hint) {
                        option value="" selected { (leave_it) }
                        (answer_options())
                    }
                    span.fine #(hint) {
                        (following) " "
                        @if !view.belongs_to_overridden {
                            (STOPS_FOLLOWING) " "
                        }
                        (moving) " " (AUDITED)
                    }
                }
                (held_for_field(row, view.beneficiary.as_deref(), &held_for_fine))
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
    /// Whether the page this came from showed the account as it is now. Any answer sent from an
    /// older one was chosen by someone who hadn't seen what has happened since, and whether the
    /// answer was chosen decides what the form's words said a change of kind would do.
    fn showed(&self, current: &AccountView) -> bool {
        self.shown_kind == current.kind.as_str()
            && self.shown_belongs_to == current.belongs_to.as_str()
            && self.shown_chosen == current.belongs_to_overridden.to_string()
            && self.shown_beneficiary == current.beneficiary.as_deref().unwrap_or_default()
    }
}

/// `err` in the form's words, for the two refusals that are about `Held for`: the service's speak
/// to the API, which names the field `beneficiary`.
pub(super) fn in_form_words(err: SyncError) -> SyncError {
    match err {
        SyncError::Invalid(message) if message == AccountError::InvalidBeneficiary.message() => {
            SyncError::Invalid(HELD_FOR_INVALID)
        }
        SyncError::Invalid(message)
            if message == AccountError::BeneficiaryOnTheHousehold.message() =>
        {
            SyncError::Invalid(HELD_FOR_ON_HOUSEHOLD)
        }
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
    if !form.showed(&current) {
        let notice = Notice::new(StatusCode::CONFLICT, CHANGED);
        return render(&state, &session, Some(notice)).await;
    }
    let kind = match form.kind.as_str() {
        "" => None,
        raw => match raw.parse::<AccountKind>() {
            Ok(kind) => Some(kind),
            Err(_) => return unprocessable(&state, &session, KIND_UNKNOWN).await,
        },
    };
    let Ok(belongs_to) = parse_belongs_to(&form.belongs_to) else {
        return unprocessable(&state, &session, BELONGS_TO_UNKNOWN).await;
    };
    // The box starts with the name the page showed, which is the account's. Sent back as it was,
    // that asks for nothing, so a move to the household's removes the name as the form said.
    let beneficiary = form
        .beneficiary
        .filter(|asked| asked.trim() != form.shown_beneficiary);
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
