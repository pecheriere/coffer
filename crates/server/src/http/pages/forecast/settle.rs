//! Settling a bill from the forecast page (spec §8.9 R9a, R14): the buttons beside a caution that
//! asks a person, the one that undoes it, and the handlers behind them. A settlement is a whole
//! calendar month, occurrences still to come included, so the buttons say so. The ledger decides
//! and audits, as for the API; the page only leaves out the buttons a role could not use.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use coffer_domain::forecast::{Caution, LineSource, SettledLine};
use coffer_domain::ids::PlannedItemId;
use coffer_domain::plan::{CalendarMonth, SettlementKind};
use jiff::civil::Date;
use maud::{Markup, html};
use serde::Deserialize;

use super::refused;
use super::words::Names;
use crate::http::AppState;
use crate::http::pages::layout::csrf_field;
use crate::http::pages::{PageError, PageForm, PageSession};

/// The occurrences a caution asks about, each with the day that names its month.
fn asked_about(caution: &Caution, as_of: Date) -> Vec<(LineSource, Date)> {
    match caution {
        Caution::BillUnpaidLastMonth { item, on, .. }
        | Caution::BillPaymentUnclear { item, on, .. } => vec![(*item, *on)],
        Caution::BillNotSeen { items, .. } => items.iter().map(|item| (*item, as_of)).collect(),
        _ => Vec::new(),
    }
}

/// The buttons that settle what `caution` asks about.
pub(super) fn forms(caution: &Caution, as_of: Date, names: &Names<'_>, csrf: &str) -> Markup {
    html! {
        @for (item, on) in asked_about(caution, as_of) {
            @if let LineSource::PlannedItem { id } = item {
                @let name = names.item(item).unwrap_or("this bill");
                @let month = on.strftime("%B %Y");
                form method="post" action={ "/forecast/items/" (id) "/settlements/" (on.strftime("%Y-%m")) } {
                    (csrf_field(csrf))
                    button.btn type="submit" name="kind" value="paid"
                        { "Mark " (name) " paid for all of " (month) }
                    " "
                    button.btn type="submit" name="kind" value="skipped"
                        { "Skip " (name) " for all of " (month) }
                }
            }
        }
    }
}

/// The button that undoes what `line` settled.
pub(super) fn undo_form(line: &SettledLine, csrf: &str) -> Markup {
    let LineSource::PlannedItem { id } = line.source else {
        return html! {};
    };
    let day = line.month.first_day();
    html! {
        form method="post" action={ "/forecast/items/" (id) "/settlements/" (day.strftime("%Y-%m")) "/undo" } {
            (csrf_field(csrf))
            button.btn type="submit" { "Undo " (line.name) " for " (day.strftime("%B %Y")) }
        }
    }
}

/// Unknown fields are refused: a forged form fails whole.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SettleForm {
    #[serde(default)]
    csrf: String,
    kind: SettlementKind,
}

pub(super) async fn settle(
    State(state): State<AppState>,
    session: PageSession,
    Path((id, month)): Path<(String, String)>,
    PageForm(form): PageForm<SettleForm>,
) -> Result<Response, PageError> {
    let actor = session.verify(&state, &form.csrf)?;
    let id: PlannedItemId = id.parse().map_err(|_| PageError::not_found())?;
    let month: CalendarMonth = month.parse().map_err(|_| PageError::not_found())?;
    state
        .ledger
        .settle_planned_item(&actor, id, month, form.kind)
        .await
        .map_err(refused)?;
    Ok(Redirect::to("/forecast").into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct UndoForm {
    #[serde(default)]
    csrf: String,
}

pub(super) async fn undo(
    State(state): State<AppState>,
    session: PageSession,
    Path((id, month)): Path<(String, String)>,
    PageForm(form): PageForm<UndoForm>,
) -> Result<Response, PageError> {
    let actor = session.verify(&state, &form.csrf)?;
    let id: PlannedItemId = id.parse().map_err(|_| PageError::not_found())?;
    let month: CalendarMonth = month.parse().map_err(|_| PageError::not_found())?;
    state
        .ledger
        .unsettle_planned_item(&actor, id, month)
        .await
        .map_err(refused)?;
    Ok(Redirect::to("/forecast").into_response())
}
