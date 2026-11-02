//! The read-only net worth page (spec §8.10, N13): what `GET /api/v1/reports/net-worth`
//! answers for a range, in words, a chart and a table of the same numbers. It calls the
//! same service and adds nothing up, so the two cannot differ, and it has no form.

mod buckets;
mod chart;
mod words;

use std::collections::BTreeMap;

use axum::Router;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use coffer_domain::ids::AccountId;
use coffer_domain::money::{Currency, Money};
use coffer_domain::net_worth::{
    AccountPoint, AccountSeries, ConversionState, MIN_VALUE_DATE, NetWorthBlocker,
    NetWorthInterval, NetWorthPoint, NetWorthReport, NetWorthStatus, period_ends,
};
use jiff::civil::Date;
use jiff::{Span, ToSpan as _};
use maud::{Markup, html};
use serde::Deserialize;

use super::account::viewer;
use super::amounts::{day, grouped, money};
use super::layout::{Frame, Nav, page};
use super::{PageError, PageSession};
use crate::http::AppState;
use crate::http::problem::{Problem, ProblemCode};
use crate::ledger::net_worth::NetWorthQuery;
use chart::{ChartPoint, ChartSize, line_chart, scale_misses_zero};
use words::{Names, blocker_sentence, caution_sentence, liquidity_name, no_amount_sentence};

const NO_TOTAL: &str = "No total";
const NO_VALUE_THAT_DAY: &str = "No value that day";
const NOTHING_COUNTED: &str = "Nothing counted";
const NOT_COUNTED: &str = "Not counted";
/// A table of this many rows opens for a gap anywhere in it.
const SHORT_TABLE: usize = 30;
/// On a longer one, a gap in this many of the newest rows opens it.
const RECENT_PERIODS: usize = 3;

pub fn routes() -> Router<AppState> {
    Router::new().route("/net-worth", get(net_worth_page))
}

/// The ranges the page offers. There are no free dates.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
enum Range {
    #[serde(rename = "3m")]
    ThreeMonths,
    #[default]
    #[serde(rename = "1y")]
    OneYear,
    #[serde(rename = "5y")]
    FiveYears,
    #[serde(rename = "all")]
    All,
}

/// How many months before this one `All` reaches: with this month, the most
/// periods a report answers, so from May 2053 it shows the latest of them rather
/// than an error.
const ALL_MONTHS: i64 = 999;

impl Range {
    const ALL: [Self; 4] = [Self::ThreeMonths, Self::OneYear, Self::FiveYears, Self::All];

    fn word(self) -> &'static str {
        match self {
            Self::ThreeMonths => "3m",
            Self::OneYear => "1y",
            Self::FiveYears => "5y",
            Self::All => "all",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::ThreeMonths => "3 months",
            Self::OneYear => "1 year",
            Self::FiveYears => "5 years",
            Self::All => "All",
        }
    }

    /// How far back it reaches, and how long its periods are.
    fn reach(self) -> (Span, NetWorthInterval) {
        match self {
            Self::ThreeMonths => (3.months(), NetWorthInterval::Day),
            Self::OneYear => (1.year(), NetWorthInterval::Week),
            Self::FiveYears => (5.years(), NetWorthInterval::Month),
            Self::All => (ALL_MONTHS.months(), NetWorthInterval::Month),
        }
    }

    fn query(self, today: Date) -> NetWorthQuery {
        let (reach, interval) = self.reach();
        let from = today
            .checked_sub(reach)
            .map_or(MIN_VALUE_DATE, |from| from.max(MIN_VALUE_DATE));
        NetWorthQuery {
            from: Some(from),
            to: Some(today),
            interval: Some(interval),
            account_points: Some(true),
        }
    }
}

/// Anything else in the address is refused.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PageQuery {
    range: Option<Range>,
}

fn bad_range() -> PageError {
    PageError(
        Problem::new(
            StatusCode::BAD_REQUEST,
            ProblemCode::Invalid,
            "Invalid request",
        )
        .with_detail("the range must be 3m, 1y, 5y or all".to_owned()),
    )
}

async fn net_worth_page(
    State(state): State<AppState>,
    session: PageSession,
    query: Result<Query<PageQuery>, QueryRejection>,
) -> Result<Response, PageError> {
    let Query(query) = query.map_err(|_| bad_range())?;
    let range = query.range.unwrap_or_default();
    let today = state
        .ledger
        .today()
        .await
        .map_err(|err| PageError(Problem::from(err)))?;
    let report = state
        .ledger
        .net_worth(&session.actor(), range.query(today))
        .await
        .map_err(|err| PageError(Problem::from(err)))?;
    let viewer = viewer(&state, &session).await;
    let frame = Frame {
        look: session.look(&state).await,
        viewer: Some(&viewer),
        current: Some(Nav::NetWorth),
        narrow: false,
    };
    let content = content(&report, range, viewer.can_manage_household);
    Ok(page(&frame, "Net worth", &content).into_response())
}

fn content(report: &NetWorthReport, range: Range, may_choose: bool) -> Markup {
    let names = Names::of_accounts(&report.accounts, report.points.last().map(|point| point.on));
    html! {
        h1 { "Net worth" }
        (range_links(range))
        (answer(report, &names, may_choose))
        @if let Some(currency) = report.display_currency {
            (history(report, currency, &names))
        }
        (latest(report, &names))
        (accounts(report))
        (buckets::section(report))
    }
}

/// One row of plain links above everything they scope.
fn range_links(current: Range) -> Markup {
    html! {
        nav.ranges aria-label="Range" {
            @for range in Range::ALL {
                a href={ "/net-worth?range=" (range.word()) }
                    aria-current=[(range == current).then_some("page")] { (range.name()) }
            }
        }
    }
}

/// The answer, how far to trust it and why, all together.
fn answer(report: &NetWorthReport, names: &Names<'_>, may_choose: bool) -> Markup {
    let blocked = !report.blockers.is_empty();
    html! {
        section.card id="answer" {
            (headline(report))
            (status(report.status, has_a_total(report)))
            @if blocked {
                h3 { "Why there's no number" }
                ul.reasons {
                    @for blocker in &report.blockers {
                        li { (blocker_sentence(*blocker)) }
                    }
                }
                @if report.blockers.contains(&NetWorthBlocker::DisplayCurrencyNotSet) {
                    (way_forward(may_choose))
                }
            }
            @if !report.cautions.is_empty() {
                h3 { (if blocked { "Also worth knowing" } else { "Why it isn't firm" }) }
                ul.reasons id="reasons" {
                    @for caution in &report.cautions {
                        li { (caution_sentence(caution, names)) }
                    }
                }
            }
        }
    }
}

fn way_forward(may_choose: bool) -> Markup {
    html! {
        p.way-forward {
            @if may_choose {
                a href="/admin/household" { "Choose a display currency" }
            } @else {
                "An admin can choose one."
            }
        }
    }
}

/// Whether the latest point has a total to speak of.
fn has_a_total(report: &NetWorthReport) -> bool {
    report
        .points
        .last()
        .is_some_and(|point| point.total.is_some())
}

/// The latest point's total, or plainly that it has none. An earlier total is not offered
/// instead: it would read as the value now.
fn headline(report: &NetWorthReport) -> Markup {
    html! {
        @match report.points.last() {
            Some(NetWorthPoint { on, total: Some(total), .. }) => {
                h2.answer-label { "Net worth on " (day(*on)) }
                (hero(*total))
            }
            Some(NetWorthPoint { on, by_currency, .. }) => {
                h2.answer-label { (NO_TOTAL) " on " (day(*on)) }
                @if report.display_currency.is_some() {
                    p.fine {
                        "Part of it can't be added up, and a total without it would look like \
                         money leaving."
                        @if !by_currency.is_empty() {
                            " The sums per currency are below."
                        }
                    }
                }
            }
            None => {
                h2.answer-label { "No number yet" }
            }
        }
    }
}

/// The figure, kept whole: only the space before its currency can wrap, so a very long
/// number takes a smaller size to fit a phone.
fn hero(total: Money) -> Markup {
    let number = grouped(total.minor_units, total.currency);
    let size = match number.len() {
        ..=10 => "hero-figure",
        11..=16 => "hero-figure hero-long",
        _ => "hero-figure hero-longest",
    };
    let class = if total.minor_units < 0 {
        format!("{size} answer-short")
    } else {
        size.to_owned()
    };
    html! {
        p class=(class) { span.hero-number { (number) } " " (total.currency.as_str()) }
    }
}

fn status(status: NetWorthStatus, has_a_total: bool) -> Markup {
    let (word, class, meaning) = match status {
        NetWorthStatus::Firm => (
            "Firm:",
            "status-firm",
            "coffer has nothing to warn you about.",
        ),
        NetWorthStatus::Provisional if has_a_total => (
            "Provisional:",
            "status-provisional",
            "this is coffer's best number, but the points below could change it.",
        ),
        NetWorthStatus::Provisional => (
            "Provisional:",
            "status-provisional",
            "there's no total on this day, for the reasons below.",
        ),
        NetWorthStatus::Unavailable => (
            "Unavailable:",
            "status-unavailable",
            "coffer can't give a number yet, for the reasons below.",
        ),
    };
    html! {
        p.status { strong class=(class) { (word) } " " (meaning) }
    }
}

/// One period between the first point and the last.
#[derive(Clone, Copy)]
enum Period<'a> {
    Point(&'a NetWorthPoint),
    /// The report leaves out a period with nothing counted. Between two points that's a
    /// stretch with no value, which the line and the table say rather than pass over.
    Nothing {
        on: Date,
    },
}

impl Period<'_> {
    fn on(self) -> Date {
        match self {
            Self::Point(point) => point.on,
            Self::Nothing { on } => on,
        }
    }

    fn partial(self) -> bool {
        match self {
            Self::Point(point) => point.partial,
            Self::Nothing { .. } => false,
        }
    }

    fn total(self) -> Option<Money> {
        match self {
            Self::Point(point) => point.total,
            Self::Nothing { .. } => None,
        }
    }

    /// The period's amount as text, or what stands for it.
    fn written(self) -> String {
        match (self, self.total()) {
            (_, Some(total)) => money(total),
            (Self::Point(_), None) => "no total".to_owned(),
            (Self::Nothing { .. }, None) => "nothing counted".to_owned(),
        }
    }
}

/// The report's points, oldest first, with the periods left out between them.
fn periods(points: &[NetWorthPoint], from: Date, interval: NetWorthInterval) -> Vec<Period<'_>> {
    let (Some(first), Some(last)) = (points.first(), points.last()) else {
        return Vec::new();
    };
    let by_day: BTreeMap<Date, &NetWorthPoint> =
        points.iter().map(|point| (point.on, point)).collect();
    // The report was answered for these dates, so they can't be refused now; if they
    // were, its own points are still all true.
    let Ok(ends) = period_ends(from, last.on, interval) else {
        return points.iter().map(Period::Point).collect();
    };
    ends.into_iter()
        .filter(|end| end.on >= first.on)
        .map(|end| {
            by_day
                .get(&end.on)
                .map_or(Period::Nothing { on: end.on }, |point| Period::Point(point))
        })
        .collect()
}

/// A period's heading in the table and its tooltip.
fn period_name(on: Date, interval: NetWorthInterval) -> String {
    match interval {
        NetWorthInterval::Day => day(on),
        NetWorthInterval::Week => format!("Week to {}", day(on)),
        NetWorthInterval::Month => on.strftime("%B %Y").to_string(),
    }
}

/// What marks a period that ends early, because the range does.
fn partial_mark(on: Date, interval: NetWorthInterval) -> String {
    match interval {
        NetWorthInterval::Week => "part of the week".to_owned(),
        NetWorthInterval::Day | NetWorthInterval::Month => format!("to {}", day(on)),
    }
}

fn chart_points(periods: &[Period<'_>], interval: NetWorthInterval) -> Vec<ChartPoint> {
    periods
        .iter()
        .map(|period| {
            let written = period.written();
            ChartPoint {
                on: period.on(),
                value: period.total().map(|total| total.minor_units),
                title: format!("{}: {written}", period_name(period.on(), interval)),
                label: written,
            }
        })
        .collect()
}

/// The chart and the table of the same numbers.
fn history(report: &NetWorthReport, currency: Currency, names: &Names<'_>) -> Markup {
    let periods = periods(&report.points, report.from, report.interval);
    if periods.is_empty() {
        return html! {};
    }
    let points = chart_points(&periods, report.interval);
    let code = currency.as_str();
    let gaps = periods.iter().any(|period| period.total().is_none());
    html! {
        section.card id="history" {
            h2 { "Net worth over time, in " (code) }
            (line_chart(&points, currency, ChartSize::Wide))
            (line_chart(&points, currency, ChartSize::Narrow))
            p.fine {
                "Each point is the value on the last day of its period. "
                @if gaps {
                    "A break in the line is a period with no total, not a fall to zero. "
                }
                @if scale_misses_zero(&points) {
                    "The scale doesn't start at zero, so a change looks larger than it is."
                }
            }
            (jumps(report, names))
            details.values open[opens_for_a_gap(&periods)] {
                summary { "Every value, as a table" }
                (periods_table(
                    &report.accounts,
                    report.interval,
                    &periods,
                    code,
                    names,
                    "Net worth by period",
                    NOTHING_COUNTED,
                ))
            }
        }
    }
}

/// The table is where a gap is explained, so it opens for one: any on a short table, and
/// only a recent one on a long table, whose rows would stand between the chart and the rest.
fn opens_for_a_gap(periods: &[Period<'_>]) -> bool {
    let no_total = |period: &Period<'_>| period.total().is_none();
    let recent = periods.len().saturating_sub(RECENT_PERIODS);
    if periods.len() <= SHORT_TABLE {
        periods.iter().any(no_total)
    } else {
        periods[recent..].iter().any(no_total)
    }
}

/// Why the line may jump or drop without any money moving.
fn jumps(report: &NetWorthReport, names: &Names<'_>) -> Markup {
    let (Some(first), Some(last)) = (report.points.first(), report.points.last()) else {
        return html! {};
    };
    let inside = |on: Date| first.on < on && on <= last.on;
    html! {
        ul.reasons.jumps {
            @for account in report.accounts.iter().filter(|account| account.counted) {
                @if let Some(on) = account.first_on.filter(|on| inside(*on)) {
                    li {
                        (names.account(account.account)) " first has a value on " (day(on))
                        ", so a jump then is the account arriving, not money."
                    }
                }
                @if let Some(on) = account.closed_on.filter(|on| inside(*on)) {
                    li {
                        (names.account(account.account)) " closed on " (day(on))
                        ", so a drop then is the account closing, not money."
                    }
                }
            }
        }
    }
}

/// Why one account has no amount on a period, to tell a run of periods apart.
type NoAmount = (AccountId, Option<ConversionState>);

/// Newest first, so the numbers nearest now are nearest the chart.
fn periods_table(
    accounts: &[AccountSeries],
    interval: NetWorthInterval,
    periods: &[Period<'_>],
    code: &str,
    names: &Names<'_>,
    label: &str,
    nothing_counted: &str,
) -> Markup {
    let total_heading = format!("Total, {code}");
    let conversions: BTreeMap<(AccountId, Date), ConversionState> = accounts
        .iter()
        .flat_map(|series| {
            series
                .points
                .iter()
                .filter(|point| point.amount.is_none())
                .map(|point| ((series.account, point.on), point.conversion))
        })
        .collect();
    // What the row above, the next period, already said.
    let mut said: Vec<NoAmount> = Vec::new();
    html! {
        table.months.amounts-wrap role="table" aria-label=(label) {
            thead role="rowgroup" {
                tr role="row" {
                    th scope="col" role="columnheader" { "Period" }
                    th scope="col" role="columnheader" { (total_heading) }
                }
            }
            tbody role="rowgroup" {
                @for period in periods.iter().rev() {
                    tr role="row" {
                        th scope="row" role="rowheader" {
                            (period_name(period.on(), interval))
                            @if period.partial() {
                                " " span.chip.chip-quiet { (partial_mark(period.on(), interval)) }
                            }
                        }
                        td role="cell" data-label=(total_heading) {
                            (period_cell(*period, &conversions, &mut said, names, nothing_counted))
                        }
                    }
                }
            }
        }
    }
}

/// A period's total, or why it has none, naming each account once per run of periods.
fn period_cell(
    period: Period<'_>,
    conversions: &BTreeMap<(AccountId, Date), ConversionState>,
    said: &mut Vec<NoAmount>,
    names: &Names<'_>,
    nothing_counted: &str,
) -> Markup {
    let reasons: Vec<NoAmount> = match period {
        Period::Point(point) if point.total.is_none() => point
            .unconverted
            .iter()
            .map(|account| (*account, conversions.get(&(*account, point.on)).copied()))
            .collect(),
        Period::Point(_) | Period::Nothing { .. } => Vec::new(),
    };
    let fresh: Vec<NoAmount> = reasons
        .iter()
        .filter(|reason| !said.contains(reason))
        .copied()
        .collect();
    let as_above = fresh.is_empty() && !reasons.is_empty();
    *said = reasons;
    html! {
        @match period {
            Period::Nothing { .. } => {
                (nothing_counted)
            }
            Period::Point(NetWorthPoint { total: Some(total), .. }) => {
                (grouped(total.minor_units, total.currency))
            }
            Period::Point(_) if as_above => {
                span.no-total { (NO_TOTAL) ", as above." }
            }
            Period::Point(_) => {
                span.no-total {
                    (NO_TOTAL) ":"
                    @for (account, conversion) in &fresh {
                        " " (no_amount_sentence(names.account(*account), *conversion))
                    }
                }
            }
        }
    }
}

fn point_on(series: &AccountSeries, on: Date) -> Option<&AccountPoint> {
    series.points.iter().find(|point| point.on == on)
}

/// The latest point by liquidity class and by currency.
fn latest(report: &NetWorthReport, names: &Names<'_>) -> Markup {
    let Some(point) = report.points.last() else {
        return html! {};
    };
    html! {
        section.card id="latest" {
            h2 { "What it's made of, on " (day(point.on)) }
            @if report.display_currency.is_some() && !point.by_liquidity.is_empty() {
                table.sums.amounts-wrap {
                    tbody {
                        @for line in &point.by_liquidity {
                            tr {
                                th scope="row" { (liquidity_name(line.liquidity)) }
                                td {
                                    @match line.amount {
                                        Some(amount) => { (grouped(amount.minor_units, amount.currency)) }
                                        None => { (NO_TOTAL) }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            @if point.by_currency.is_empty() {
                p.fine { "No account has a value that day." }
            } @else {
                h3 { "By currency, unconverted" }
                (by_currency_table(&point.by_currency))
                (left_out_notes(&report.accounts, point, names))
            }
        }
    }
}

/// An account with no row that day has no balance to sum, which must not pass unsaid.
fn left_out_notes(accounts: &[AccountSeries], point: &NetWorthPoint, names: &Names<'_>) -> Markup {
    let left_out = point.unconverted.iter().filter(|account| {
        accounts
            .iter()
            .find(|series| series.account == **account)
            .is_none_or(|series| point_on(series, point.on).is_none())
    });
    html! {
        @for account in left_out {
            p.fine {
                "These sums leave out " (names.account(*account))
                ", which has no value that day."
            }
        }
    }
}

/// Sums per currency, never added together.
fn by_currency_table(sums: &[Money]) -> Markup {
    html! {
        table.sums.amounts-wrap {
            tbody {
                @for sum in sums {
                    tr {
                        th scope="row" { (sum.currency.as_str()) }
                        td { (money(*sum)) }
                    }
                }
            }
        }
    }
}

/// One cell of the accounts table: its value, and the date it rests on.
struct AccountCell {
    value: String,
    note: Option<String>,
}

impl AccountCell {
    fn plain(value: &str) -> Self {
        Self {
            value: value.to_owned(),
            note: None,
        }
    }
}

/// Each visible account at the latest point, and what its numbers rest on.
fn accounts(report: &NetWorthReport) -> Markup {
    if report.accounts.is_empty() {
        return html! {};
    }
    html! {
        section.card id="accounts" {
            h2 { "Accounts" }
            (accounts_table(
                &report.accounts,
                report.display_currency,
                report.points.last().map(|point| point.on),
                "Accounts at the latest point",
                NO_VALUE_THAT_DAY,
                NOT_COUNTED,
            ))
        }
    }
}

/// One row per account, as it stood on `latest`.
fn accounts_table(
    accounts: &[AccountSeries],
    display_currency: Option<Currency>,
    latest: Option<Date>,
    label: &str,
    no_value: &str,
    not_counted: &str,
) -> Markup {
    let in_display = display_currency.map_or_else(
        || "Converted".to_owned(),
        |currency| format!("In {}", currency.as_str()),
    );
    let columns = ["Balance", in_display.as_str(), "First day"];
    html! {
        table.months.amounts-wrap role="table" aria-label=(label) {
            thead role="rowgroup" {
                tr role="row" {
                    th scope="col" role="columnheader" { "Account" }
                    @for column in columns {
                        th scope="col" role="columnheader" { (column) }
                    }
                }
            }
            tbody role="rowgroup" {
                @for series in accounts {
                    @let at = latest.and_then(|on| point_on(series, on));
                    tr role="row" {
                        th scope="row" role="rowheader" { (series.name) }
                        @for (column, cell) in columns.iter().zip(account_cells(series, at, no_value, not_counted)) {
                            td role="cell" data-label=(column) {
                                span.cell-stack {
                                    (cell.value)
                                    @if let Some(note) = &cell.note {
                                        span.cell-note { (note) }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A shorter date, for a table of several.
fn short_day(date: Date) -> String {
    date.strftime("%-d %b %Y").to_string()
}

/// An account's cells: balance and its date, amount and its rate's date, and first day.
fn account_cells(
    series: &AccountSeries,
    at: Option<&AccountPoint>,
    no_value: &str,
    not_counted: &str,
) -> [AccountCell; 3] {
    const NOT_APPLICABLE: &str = "\u{2013}";
    if !series.counted {
        return [not_counted, not_counted, NOT_APPLICABLE].map(AccountCell::plain);
    }
    let first_day = AccountCell::plain(
        &series
            .first_on
            .map_or_else(|| "None yet".to_owned(), short_day),
    );
    let Some(point) = at else {
        return [
            AccountCell::plain(no_value),
            AccountCell::plain("None"),
            first_day,
        ];
    };
    let amount = match (point.counted, point.amount, point.conversion) {
        (false, _, _) => not_counted.to_owned(),
        (true, Some(amount), _) => grouped(amount.minor_units, amount.currency),
        (true, None, ConversionState::Missing) => "No rate".to_owned(),
        (true, None, ConversionState::TooLarge) => "Too large".to_owned(),
        (true, None, ConversionState::NoDisplayCurrency) => "No display currency".to_owned(),
        (true, None, ConversionState::Same | ConversionState::Converted) => "None".to_owned(),
    };
    [
        AccountCell {
            value: money(point.balance),
            note: Some(format!("from {}", short_day(point.balance_on))),
        },
        AccountCell {
            value: amount,
            note: point
                .rate_on
                .map(|on| format!("at the rate of {}", short_day(on))),
        },
        first_day,
    ]
}
