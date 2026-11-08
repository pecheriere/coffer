//! Home at `/` (spec §5): what to know today. Net worth with how it moved this month, and
//! how many transactions wait to be checked. Read-only: it calls the ledger as
//! `GET /api/v1/reports/net-worth` and `GET /api/v1/transactions/review-count` do and adds
//! nothing up, and a part the viewer may not read is left out.

mod sparkline;

use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use coffer_domain::money::Money;
use coffer_domain::net_worth::{
    MIN_VALUE_DATE, NetWorthBlocker, NetWorthInterval, NetWorthReport, NetWorthStatus,
};
use jiff::ToSpan as _;
use jiff::civil::Date;
use maud::{Markup, html};

use super::account::viewer;
use super::amounts::{magnitude, money};
use super::layout::{Frame, Nav, page};
use super::{PageError, PageSession};
use crate::http::AppState;
use crate::http::problem::Problem;
use crate::ledger::LedgerError;
use crate::ledger::net_worth::NetWorthQuery;
use crate::ledger::review_count::ReviewCount;
use sparkline::sparkline;

/// Where the review queue is.
const QUEUE: &str = "/transactions?review=unreviewed";

pub fn routes() -> Router<AppState> {
    Router::new().route("/", get(home_page))
}

/// A year of month ends, this month's so far last: from the first day of the month twelve
/// months back.
fn last_twelve_months(today: Date) -> NetWorthQuery {
    let from = today
        .first_of_month()
        .checked_sub(12.months())
        .map_or(MIN_VALUE_DATE, |from| from.max(MIN_VALUE_DATE));
    NetWorthQuery {
        from: Some(from),
        to: None,
        interval: Some(NetWorthInterval::Month),
        account_points: Some(false),
    }
}

async fn home_page(
    State(state): State<AppState>,
    session: PageSession,
) -> Result<Response, PageError> {
    let actor = session.actor();
    let net_worth = match state
        .ledger
        .net_worth_by_day(&actor, last_twelve_months)
        .await
    {
        Ok(report) => Some(report),
        Err(LedgerError::Denied(_)) => None,
        Err(err) => return Err(PageError(Problem::from(err))),
    };
    let to_check = state
        .ledger
        .count_transactions_to_review(&actor)
        .await
        .map_err(|err| PageError(Problem::from(err)))?;
    let viewer = viewer(&state, &session).await;
    let frame = Frame {
        look: session.look(&state).await,
        viewer: Some(&viewer),
        current: Some(Nav::Home),
        narrow: false,
    };
    let content = html! {
        h1 { "Home" }
        @if let Some(report) = &net_worth {
            (net_worth_card(report, viewer.can_manage_household))
        }
        (to_check_card(to_check))
    };
    Ok(page(&frame, "Home", &content).into_response())
}

fn net_worth_card(report: &NetWorthReport, may_choose: bool) -> Markup {
    html! {
        section.card.home-card id="net-worth" {
            h2 { "Net worth" }
            (net_worth_body(report, may_choose))
        }
    }
}

fn net_worth_body(report: &NetWorthReport, may_choose: bool) -> Markup {
    if report
        .blockers
        .contains(&NetWorthBlocker::NoCountedAccounts)
    {
        return html! {
            p { "No accounts count towards net worth yet." }
            p { a href="/accounts" { "See your accounts" } }
        };
    }
    let latest = report.points.last().and_then(|point| point.total);
    let Some(total) = latest else {
        return no_total(report, may_choose);
    };
    let totals: Option<Vec<i64>> = report
        .points
        .iter()
        .map(|point| point.total.map(|total| total.minor_units))
        .collect();
    html! {
        p.answer-number { (money(total)) }
        @if let Some(change) = change_this_month(report) {
            p { (change_sentence(change)) }
        }
        @if let Some(totals) = totals {
            (sparkline(&totals, &format!(
                "Net worth at the end of each of the last {} months",
                totals.len()
            )))
        }
        @if report.status == NetWorthStatus::Provisional {
            p.fine { "This number could still change." }
        }
        p { a href="/net-worth" { "See its history" } }
    }
}

/// Why there is no number, and where to go about it. Never a zero in its place.
fn no_total(report: &NetWorthReport, may_choose: bool) -> Markup {
    if report
        .blockers
        .contains(&NetWorthBlocker::DisplayCurrencyNotSet)
    {
        let (href, words) = if may_choose {
            ("/admin/household", "Choose a display currency")
        } else {
            ("/net-worth", "See it by currency")
        };
        return html! {
            p { "Choose a display currency to see net worth in one number." }
            p { a href=(href) { (words) } }
        };
    }
    html! {
        p { "Net worth can't be added up right now." }
        p { a href="/net-worth" { "See why" } }
    }
}

/// The latest total less the one before it, when both exist in one currency.
fn change_this_month(report: &NetWorthReport) -> Option<Money> {
    let [.., before, latest] = report.points.as_slice() else {
        return None;
    };
    let (before, latest) = (before.total?, latest.total?);
    if before.currency != latest.currency {
        return None;
    }
    let minor_units = latest.minor_units.checked_sub(before.minor_units)?;
    Some(Money {
        minor_units,
        currency: latest.currency,
    })
}

fn change_sentence(change: Money) -> String {
    match change.minor_units.signum() {
        1 => format!("Up {} this month", magnitude(change)),
        -1 => format!("Down {} this month", magnitude(change)),
        _ => "No change this month".to_owned(),
    }
}

fn to_check_card(count: ReviewCount) -> Markup {
    html! {
        section.card.home-card id="to-check" {
            h2 { "To check" }
            @match count.unreviewed {
                0 => p { "Nothing to check." },
                1 => p { a href=(QUEUE) { "1 transaction to check" } },
                many => p { a href=(QUEUE) { (many) " transactions to check" } },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use coffer_domain::money::{Currency, Money};
    use jiff::civil::date;

    use super::{change_sentence, last_twelve_months};

    fn usd(minor_units: i64) -> Money {
        Money {
            minor_units,
            currency: Currency::parse("USD").unwrap(),
        }
    }

    #[test]
    fn the_year_starts_on_the_first_of_a_month() {
        let query = last_twelve_months(date(2026, 11, 2));
        assert_eq!(query.from, Some(date(2025, 11, 1)));
        assert_eq!(query.to, None, "up to today, the report's day");
    }

    #[test]
    fn a_change_says_which_way_without_a_sign() {
        assert_eq!(change_sentence(usd(10_000)), "Up 100.00 USD this month");
        assert_eq!(change_sentence(usd(-2_550)), "Down 25.50 USD this month");
        assert_eq!(change_sentence(usd(0)), "No change this month");
    }
}
