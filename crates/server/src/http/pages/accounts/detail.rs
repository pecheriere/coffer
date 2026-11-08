//! One account's page: what it is, its balance history a page at a time, newest day first, as
//! `GET /api/v1/accounts/{id}/balances` answers it, and the forms of an actor who may change it.
//! An account the actor can't see answers as one that doesn't exist.

use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use coffer_domain::accounts::{BalanceSource, BelongsTo};
use coffer_domain::ids::AccountId;
use coffer_domain::policy::AccountLevel;
use coffer_store::PageRequest;
use maud::{Markup, html};
use serde::Deserialize;

use super::fields::{answer_label, beneficiaries, kind_label, whose_words};
use super::{
    LIST, Notice, account_path, answer, forecast_words, notice_line, page_error, source_words,
};
use crate::http::AppState;
use crate::http::pages::account::viewer;
use crate::http::pages::amounts::{day, money};
use crate::http::pages::layout::{Frame, Nav, page};
use crate::http::pages::net_worth::liquidity_name;
use crate::http::pages::{PageError, PageSession};
use crate::ledger::LedgerError;
use crate::ledger::accounts::{AccountSource, AccountView, BalancePage};

/// Where in the balances to start. Anything else in the address is refused.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BalancesQuery {
    after: Option<String>,
}

pub(super) async fn account_page(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    query: Result<Query<BalancesQuery>, QueryRejection>,
) -> Result<axum::response::Response, PageError> {
    let id: AccountId = id.parse().map_err(|_| PageError::not_found())?;
    let Query(query) = query.map_err(|_| PageError::invalid("not a place in the list"))?;
    render_account(&state, &session, id, query.after, None).await
}

/// The page of the account `id`, its balances from `after`, with `notice` above it when a form
/// didn't go through.
pub(super) async fn render_account(
    state: &AppState,
    session: &PageSession,
    id: AccountId,
    after: Option<String>,
    notice: Option<Notice>,
) -> Result<axum::response::Response, PageError> {
    let actor = session.actor();
    let view = state
        .ledger
        .get_account(&actor, id)
        .await
        .map_err(page_error)?;
    let paged = after.is_some();
    let balances = state
        .ledger
        .list_balances(
            &actor,
            id,
            None,
            None,
            &PageRequest {
                after,
                ..PageRequest::default()
            },
        )
        .await
        .map_err(page_error)?;
    // The ledger decides, as for the API: a refusal only leaves the forms out.
    let may_change = match state
        .ledger
        .authorize_account_access(&actor, id, AccountLevel::Write)
        .await
    {
        Ok(()) => true,
        Err(LedgerError::Denied(_)) => false,
        Err(err) => return Err(page_error(err)),
    };
    // The forms' day and the labels their `Held for` offers, only for an actor who has them.
    let forms = if may_change {
        let today = state.ledger.today().await.map_err(page_error)?;
        let accounts = state
            .ledger
            .list_accounts(&actor, true)
            .await
            .map_err(page_error)?;
        Some((today, accounts))
    } else {
        None
    };
    let viewer = viewer(state, session).await;
    let look = session.look(state).await;
    let content = html! {
        p { a href=(LIST) { "All accounts" } }
        h1 { (view.name) }
        (notice_line(notice.as_ref()))
        (facts(&view))
        (history(&view, &balances, paged))
        @if let Some((today, accounts)) = &forms {
            (super::balance::card(&view, &viewer.csrf, *today))
            (super::edit::card(&view, &viewer.csrf))
            (super::edit::closing_card(&view, &viewer.csrf))
            (beneficiaries(accounts))
        }
    };
    let frame = Frame {
        look,
        viewer: Some(&viewer),
        current: Some(Nav::Accounts),
        narrow: false,
    };
    Ok(answer(page(&frame, &view.name, &content), notice.as_ref()))
}

/// What the account is, a row for each thing the API says of it.
fn facts(view: &AccountView) -> Markup {
    let whose = whose_words(view).unwrap_or_else(|| answer_label(view.belongs_to).to_owned());
    let net_worth = match (view.belongs_to, view.include_in_net_worth) {
        (BelongsTo::Household, true) => "Counted",
        (BelongsTo::Household, false) => "Left out by choice",
        _ => {
            "Left out, since it isn't the household's: net worth shows it in a bucket beside the total"
        }
    };
    html! {
        section.card {
            table.sums {
                tbody {
                    tr { th scope="row" { "Kind" } td { (kind_label(view.kind)) } }
                    tr { th scope="row" { "Currency" } td { (view.currency.as_str()) } }
                    tr {
                        th scope="row" { "Balances from" }
                        td {
                            (source_words(view.source))
                            @if view.source == AccountSource::Linked {
                                ", on " a href="/connections" { "Connections" }
                            }
                        }
                    }
                    tr {
                        th scope="row" { "How spendable" }
                        td {
                            (liquidity_name(view.liquidity))
                            @if view.liquidity_overridden { " (picked)" }
                        }
                    }
                    tr { th scope="row" { "Whose money" } td { (whose) } }
                    tr { th scope="row" { "Net worth" } td { (net_worth) } }
                    tr { th scope="row" { "Forecast" } td { (forecast_words(view)) } }
                    tr {
                        th scope="row" { "State" }
                        td { @if view.closed { "Closed" } @else { "Open" } }
                    }
                }
            }
        }
    }
}

fn source_label(source: BalanceSource) -> &'static str {
    match source {
        BalanceSource::Manual => "Entered by hand",
        BalanceSource::Sync => "From a sync",
    }
}

/// The balances, one a day, newest first, a page at a time.
fn history(view: &AccountView, balances: &BalancePage, paged: bool) -> Markup {
    let here = account_path(view);
    html! {
        section.card id="balances" {
            h2 { "Balances" }
            @if balances.items.is_empty() {
                p { "No balances here yet." }
            } @else {
                table.months aria-label="Balances, newest day first" {
                    thead {
                        tr {
                            th scope="col" { "Day" }
                            th scope="col" { "Balance" }
                            th scope="col" { "From" }
                        }
                    }
                    tbody {
                        @for balance in &balances.items {
                            tr {
                                th scope="row" { (day(balance.on)) }
                                td { (money(balance.balance)) }
                                td { (source_label(balance.source)) }
                            }
                        }
                    }
                }
            }
            @if let Some(next) = &balances.next {
                p.fine {
                    a href=(format!("{here}?after={}#balances", encoded(next))) { "Older balances" }
                }
            }
            @if paged {
                p.fine { a href=(format!("{here}#balances")) { "Back to the newest balances" } }
            }
        }
    }
}

/// `cursor` as a query value.
fn encoded(cursor: &str) -> String {
    url::form_urlencoded::byte_serialize(cursor.as_bytes()).collect()
}
