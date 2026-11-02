//! The read-only forecast page (spec §8.9, R14): what `GET /api/v1/forecast`
//! answers on the household's defaults, in words. It calls the same service and
//! computes nothing, so the two cannot differ, and it has no form of its own.

mod words;

use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use coffer_domain::forecast::{CurrencyTotals, Forecast, ForecastStatus, MonthBucket};
use coffer_domain::identity::Actor;
use coffer_domain::money::{Currency, Money};
use maud::{Markup, html};

use super::account::viewer;
use super::layout::{Frame, Nav, page};
use super::{PageError, PageSession};
use crate::http::AppState;
use crate::http::problem::Problem;
use crate::ledger::LedgerError;
use crate::ledger::forecast::ForecastOptions;
use words::{
    Names, blocker_sentence, caution_sentence, day, grouped, magnitude, money, month_name, months,
    settled_sentence,
};

pub fn routes() -> Router<AppState> {
    Router::new().route("/forecast", get(forecast_page))
}

async fn forecast_page(
    State(state): State<AppState>,
    session: PageSession,
) -> Result<Response, PageError> {
    let actor = session.actor();
    let forecast = state
        .ledger
        .forecast(&actor, ForecastOptions::default())
        .await
        .map_err(refused)?;
    let names = names(&state, &actor, &forecast).await;
    let viewer = viewer(&state, &session).await;
    let frame = Frame {
        look: session.look(&state).await,
        viewer: Some(&viewer),
        current: Some(Nav::Forecast),
        narrow: false,
    };
    Ok(page(&frame, "Forecast", &content(&forecast, &names)).into_response())
}

fn refused(err: LedgerError) -> PageError {
    PageError(Problem::from(err))
}

/// The names the cautions speak of. Only they need more than the forecast holds, which
/// speaks of a category or a planned item by its id. A name is a courtesy: the numbers
/// are already read, so a lookup that fails costs its names, and the sentences read
/// without them.
async fn names<'a>(state: &AppState, actor: &Actor, forecast: &'a Forecast) -> Names<'a> {
    let mut names = Names::of_accounts(&forecast.accounts);
    if forecast.cautions.is_empty() {
        return names;
    }
    match state.ledger.list_categories(actor).await {
        Ok(tree) => {
            let categories = tree.items.into_iter().flat_map(|group| group.categories);
            names
                .categories
                .extend(categories.map(|category| (category.id, category.name)));
        }
        Err(err) => tracing::warn!(error = %err, "could not read the forecast's category names"),
    }
    match state.ledger.list_planned_items(actor, false).await {
        Ok(items) => names
            .items
            .extend(items.into_iter().map(|item| (item.id, item.name))),
        Err(err) => tracing::warn!(error = %err, "could not read the forecast's item names"),
    }
    names
}

fn content(forecast: &Forecast, names: &Names<'_>) -> Markup {
    html! {
        h1 { "Forecast" }
        p {
            "From " (day(forecast.as_of)) " up to " (day(forecast.end_exclusive)) ", "
            (months(forecast.horizon_months)) ", on the household's settings."
        }
        (answer(forecast, names))
        // A row without a surplus has a blocker leaving something out of its sums,
        // and a sum that looks whole is what a blocker exists to prevent.
        @for totals in forecast.totals.iter().filter(|totals| totals.surplus.is_some()) {
            (currency_section(forecast, totals))
        }
    }
}

/// The answer, how far to trust it and why, all together.
fn answer(forecast: &Forecast, names: &Names<'_>) -> Markup {
    let blocked = !forecast.blockers.is_empty();
    html! {
        section.card id="answer" {
            (headline(forecast))
            (status(forecast.status))
            @if blocked {
                h3 { "Why there's no number" }
                ul.reasons {
                    @for blocker in &forecast.blockers {
                        li { (blocker_sentence(blocker)) }
                    }
                }
            }
            @if !forecast.cautions.is_empty() {
                h3 { (if blocked { "Also worth knowing" } else { "Why it isn't firm" }) }
                ul.reasons id="reasons" {
                    @for caution in &forecast.cautions {
                        li { (caution_sentence(caution, forecast.as_of, names)) }
                    }
                }
            }
            @if !forecast.settled.is_empty() {
                h3 { "Settled, so not counted" }
                ul.reasons id="settled" {
                    @for line in &forecast.settled {
                        li { (settled_sentence(line)) }
                    }
                }
            }
        }
    }
}

fn headline(forecast: &Forecast) -> Markup {
    html! {
        @match forecast.surplus {
            Some(surplus) if surplus.minor_units < 0 => {
                h2.answer-label { "Shortfall" }
                p.answer-number.answer-short { (magnitude(surplus)) }
                p.fine { "What's missing to cover the planned outflows, goals and safety floor." }
            }
            Some(surplus) => {
                h2.answer-label { "Surplus" }
                p.answer-number { (money(surplus)) }
                p.fine { "What could be invested once the planned outflows, goals and safety floor are covered." }
            }
            None => {
                h2.answer-label { "No number yet" }
            }
        }
    }
}

fn status(status: ForecastStatus) -> Markup {
    let (word, class, meaning) = match status {
        ForecastStatus::Firm => (
            "Firm:",
            "status-firm",
            "coffer has nothing to warn you about.",
        ),
        ForecastStatus::Provisional => (
            "Provisional:",
            "status-provisional",
            "this is coffer's best number, but the points below could change it.",
        ),
        ForecastStatus::Unavailable => (
            "Unavailable:",
            "status-unavailable",
            "coffer can't give a number yet, for the reasons below.",
        ),
    };
    html! {
        p.status { strong class=(class) { (word) } " " (meaning) }
    }
}

/// One currency's sums and months, never netted with another's.
fn currency_section(forecast: &Forecast, totals: &CurrencyTotals) -> Markup {
    let code = totals.currency.as_str();
    let several = forecast.totals.len() > 1;
    let bare = |amount: Money| cell(amount, several);
    html! {
        section.card id={ "currency-" (code) } {
            h2 { "How it adds up, in " (code) }
            table.sums {
                tbody {
                    (sum_row("", "Cash available", &bare(totals.cash_available)))
                    (sum_row("", "Cash needed", &bare(totals.cash_needed)))
                    (sum_row("sum-part", "Planned outflows", &bare(totals.planned_outflows)))
                    (sum_row("sum-part", "Goals", &bare(totals.goals)))
                    (sum_row("sum-part", "Safety floor", &bare(totals.floor)))
                    (sum_row("sum-part", "Income counted, taken off", &bare(totals.planned_inflows)))
                    @if let Some(surplus) = totals.surplus {
                        @if surplus.minor_units < 0 {
                            (sum_row("sum-total", "Shortfall", bare(surplus).trim_start_matches('-')))
                        } @else {
                            (sum_row("sum-total", "Surplus", &bare(surplus)))
                        }
                    }
                    @if !forecast.include_income && totals.excluded_inflows.minor_units != 0 {
                        (sum_row("sum-aside", "Expected income, not counted", &bare(totals.excluded_inflows)))
                    }
                }
            }
            @if !forecast.cautions.is_empty() {
                p.fine { "Not firm: " a href="#reasons" { "see why" } "." }
            }
            h3 { "Month by month" }
            (months_table(forecast.include_income, &forecast.months, totals.currency, several))
            p.fine {
                "Projected cash is what's left at the end of the month, or of the forecast, after "
                "planned outflows and any income counted. It doesn't set aside goals or the safety floor. "
                "Goal accrual is what goals add to the need that month, and the first month holds "
                "all they had built up before it. The first month also holds what a budget's "
                "days earlier that month have left unspent, since it may still go."
            }
        }
    }
}

/// An amount in a table. With one currency the section's heading says which; with
/// several, a phone's stacked rows can sit a long way from a heading that scrolled off, and
/// another currency's look just the same, so each cell says.
fn cell(amount: Money, several: bool) -> String {
    if several {
        money(amount)
    } else {
        grouped(amount.minor_units, amount.currency)
    }
}

fn sum_row(class: &str, label: &str, amount: &str) -> Markup {
    html! {
        tr class=[(!class.is_empty()).then_some(class)] {
            th scope="row" { (label) }
            td { (amount) }
        }
    }
}

/// The months of `currency`. Below the phone width each row stands alone as a small
/// block, so `data-label` repeats its column's heading and the roles keep it a table
/// for assistive technology, which `display: block` would otherwise undo.
fn months_table(
    include_income: bool,
    buckets: &[MonthBucket],
    currency: Currency,
    several: bool,
) -> Markup {
    let income = if include_income {
        "Expected income"
    } else {
        "Expected income (not counted)"
    };
    let columns = ["Outflows", "Goal accrual", income, "Projected cash"];
    html! {
        table.months role="table" {
            thead role="rowgroup" {
                tr role="row" {
                    th scope="col" role="columnheader" { "Month" }
                    @for column in columns {
                        th scope="col" role="columnheader" { (column) }
                    }
                }
            }
            tbody role="rowgroup" {
                @for bucket in buckets {
                    @for line in bucket.lines.iter().filter(|line| line.currency == currency) {
                        tr role="row" {
                            th scope="row" role="rowheader" {
                                (month_name(bucket.month))
                                @if bucket.partial {
                                    " " span.chip.chip-quiet { "part of the month" }
                                }
                            }
                            @for (column, amount) in columns.iter().zip([
                                line.outflows,
                                line.goal_accrual,
                                line.inflows,
                                line.projected_cash,
                            ]) {
                                td role="cell" data-label=(column) {
                                    (cell(amount, several))
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
