//! The accounts page (`/accounts`) and each account's own (`/accounts/{id}`): every account the
//! visitor sees with its latest balance, whose money it is and what the forecast does with it, and
//! forms to create a manual account ([`create`]), change one or close it ([`edit`]) and record a
//! balance for a day ([`balance`]). Every read and write goes through the ledger's account service
//! with the session's actor, as `/api/v1/accounts` does, so access, the rule of whose money an
//! account is and the audit stay there. A refusal shows fixed words, never what was sent. No
//! script handles anything here.

mod balance;
mod create;
mod detail;
mod edit;
pub(super) mod fields;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use coffer_domain::accounts::{BelongsTo, LiquidityClass};
use coffer_domain::forecast::{CountingFacts, NotCountedReason, left_out_of_forecast};
use maud::{Markup, html};

use super::account::viewer;
use super::amounts::{day, money};
use super::layout::{Frame, Nav, empty_state, page};
use super::net_worth::liquidity_name;
use super::{PageError, PageSession};
use crate::http::AppState;
use crate::http::problem::Problem;
use crate::ledger::LedgerError;
use crate::ledger::accounts::{AccountSource, AccountView};
use fields::{beneficiaries, form_words, kind_label, whose_words};

const LIST: &str = "/accounts";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(LIST, get(list).post(create::create))
        .route("/accounts/{id}", get(detail::account_page).post(edit::edit))
        .route("/accounts/{id}/closing", post(edit::closing))
        .route("/accounts/{id}/balances", post(balance::record))
}

/// Where one account's page is.
fn account_path(account: &AccountView) -> String {
    format!("{LIST}/{}", account.id)
}

/// A form that didn't go through, said above the page with the response's status. Its words
/// are fixed: they never hold what was sent.
struct Notice {
    status: StatusCode,
    message: String,
}

impl Notice {
    fn new(status: StatusCode, message: &str) -> Self {
        Self {
            status,
            message: message.to_owned(),
        }
    }
}

/// The notice for a refusal of the account service, or the error page it deserves: a denial, a
/// hidden account and a failure aren't the form's fault.
fn notice_for(err: LedgerError) -> Result<Notice, PageError> {
    match err {
        LedgerError::Invalid(message) => Ok(Notice::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            &sentence(form_words(message)),
        )),
        LedgerError::Conflict(_, message) => {
            Ok(Notice::new(StatusCode::CONFLICT, &sentence(message)))
        }
        other => Err(page_error(other)),
    }
}

/// `message` with its first letter upper case and a final full stop.
fn sentence(message: &str) -> String {
    let mut chars = message.chars();
    let mut out: String = chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default();
    if !out.ends_with('.') {
        out.push('.');
    }
    out
}

fn page_error(err: LedgerError) -> PageError {
    PageError(Problem::from(err))
}

/// `markup` as a response, with the notice's status when there is one.
fn answer(markup: Markup, notice: Option<&Notice>) -> Response {
    match notice {
        Some(notice) => (notice.status, markup).into_response(),
        None => markup.into_response(),
    }
}

fn notice_line(notice: Option<&Notice>) -> Markup {
    html! {
        @if let Some(notice) = notice {
            p.notice role="alert" { (notice.message) }
        }
    }
}

async fn list(State(state): State<AppState>, session: PageSession) -> Result<Response, PageError> {
    render_list(&state, &session, None).await
}

/// The list, with `notice` above it when a form didn't go through.
async fn render_list(
    state: &AppState,
    session: &PageSession,
    notice: Option<Notice>,
) -> Result<Response, PageError> {
    let actor = session.actor();
    let accounts = state
        .ledger
        .list_accounts(&actor, true)
        .await
        .map_err(page_error)?;
    let create = create::CreateCard::load(state, &actor).await?;
    let viewer = viewer(state, session).await;
    let look = session.look(state).await;
    let (open, closed): (Vec<&AccountView>, Vec<&AccountView>) =
        accounts.iter().partition(|account| !account.closed);
    let (household, not_household): (Vec<&AccountView>, Vec<&AccountView>) = open
        .into_iter()
        .partition(|account| account.belongs_to == BelongsTo::Household);
    let content = html! {
        h1 { "Accounts" }
        (notice_line(notice.as_ref()))
        @if accounts.is_empty() {
            section.card {
                (empty_state(look, "No accounts yet. Create one below, or bind one on Connections."))
            }
        }
        @if !household.is_empty() {
            section.card {
                h2 { "The household's" }
                @for class in LiquidityClass::ALL {
                    @let in_class: Vec<&AccountView> = household
                        .iter()
                        .copied()
                        .filter(|account| account.liquidity == class)
                        .collect();
                    @if !in_class.is_empty() {
                        h3 { (liquidity_name(class)) }
                        (account_items(&in_class))
                    }
                }
            }
        }
        @if !not_household.is_empty() {
            section.card {
                h2 { "Not the household's" }
                p {
                    "Held for someone else, such as a 529 or a custodial account for a child, or "
                    "given away, such as a donor-advised fund. They stay out of net worth and the "
                    "forecast. Net worth shows them apart, in buckets beside the total."
                }
                (account_items(&not_household))
            }
        }
        @if !closed.is_empty() {
            section.card {
                details {
                    summary { h2 { "Closed (" (closed.len()) ")" } }
                    p.fine {
                        "Net worth counts a closed account up to the day it closed, and after that "
                        "only while it owes money."
                    }
                    (account_items(&closed))
                }
            }
        }
        (create.card(&viewer.csrf))
        @if create.allowed() {
            (beneficiaries(&accounts))
        }
    };
    let frame = Frame {
        look,
        viewer: Some(&viewer),
        current: Some(Nav::More),
        narrow: false,
    };
    Ok(answer(page(&frame, "Accounts", &content), notice.as_ref()))
}

fn account_items(accounts: &[&AccountView]) -> Markup {
    html! {
        ul.items {
            @for account in accounts {
                li.item {
                    div.item-main {
                        a.item-title href=(account_path(account)) { (account.name) }
                        span.item-meta { (summary(account)) }
                        span.item-meta { (forecast_words(account)) }
                    }
                    (latest_balance(account))
                }
            }
        }
    }
}

/// What an account is, in one line: kind, currency, where its data comes from and whose it is.
fn summary(account: &AccountView) -> String {
    let mut parts = vec![
        kind_label(account.kind).to_owned(),
        account.currency.as_str().to_owned(),
        source_words(account.source).to_owned(),
    ];
    parts.extend(whose_words(account));
    if !account.include_in_net_worth && account.belongs_to == BelongsTo::Household {
        parts.push("Left out of net worth".to_owned());
    }
    parts.join(" · ")
}

fn source_words(source: AccountSource) -> &'static str {
    match source {
        AccountSource::Manual => "Kept by hand",
        AccountSource::Linked => "Linked to a bank",
    }
}

fn latest_balance(account: &AccountView) -> Markup {
    html! {
        span.cell-stack {
            @if let Some(latest) = &account.latest_balance {
                (money(latest.balance))
                span.cell-note { "on " (day(latest.on)) }
            } @else {
                span.cell-note { "No balance yet" }
            }
        }
    }
}

/// What the forecast does with an account as it stands, decided by the forecast's own rule.
fn forecast_words(account: &AccountView) -> &'static str {
    let facts = CountingFacts {
        liquidity: account.liquidity,
        belongs_to: account.belongs_to,
        include_in_forecast: account.include_in_forecast,
        closed: account.closed,
        balance_minor: account
            .latest_balance
            .as_ref()
            .map(|latest| latest.balance.minor_units),
    };
    match left_out_of_forecast(facts) {
        None if account.liquidity == LiquidityClass::Cash => "Counts as cash in the forecast",
        None => "Counts against cash in the forecast",
        Some(NotCountedReason::NotHousehold) => {
            "Not in the forecast, since it isn't the household's"
        }
        Some(NotCountedReason::ExcludedByFlag) => "Left out of the forecast by choice",
        Some(NotCountedReason::Closed) => "Not in the forecast, since it is closed",
        Some(NotCountedReason::NotCash) => {
            "Not in the forecast, which counts only cash and debts due soon"
        }
    }
}
