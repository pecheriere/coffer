//! Home's first card: free money, what is spoken for and what is left, with the forecast's
//! cautions after the answer. It shows what `GET /api/v1/free-money` answers and adds
//! nothing up.

use coffer_domain::forecast::{Blocker, Caution, FreeMoney, FreeMoneyCurrency};
use coffer_domain::money::{Currency, Money};
use jiff::civil::Date;
use maud::{Markup, html};

use super::bar::{Part, bar};
use super::words::{held_note, hero_sentence};
use crate::http::pages::amounts::{day, money};
use crate::http::pages::forecast::emergency_fund_next_step;
use crate::http::pages::forecast::words::{Names, blocker_sentence, caution_sentence};

/// One line of the breakdown: what it is, how much, and the class of its colour mark.
pub(super) struct LegendRow {
    pub label: String,
    pub amount: Money,
    /// A colour class for the mark before the label; empty for cash, which is the whole.
    pub class: &'static str,
    /// What the amount is measured against, when it needs saying.
    pub note: Option<String>,
}

const FREE_CLASS: &str = "free-fill";

/// The breakdown in the order of §4.3 for a currency with a number. A "coming up" row and a
/// goals row of nothing are left out.
pub(super) fn legend_rows(row: &FreeMoneyCurrency, through: Date) -> Vec<LegendRow> {
    let zero = Money {
        minor_units: 0,
        currency: row.currency,
    };
    let need = &row.still_need;
    let mut rows = vec![
        LegendRow {
            label: "Cash available".to_owned(),
            amount: row.cash_available,
            class: "",
            note: None,
        },
        LegendRow {
            label: "Must pay this month".to_owned(),
            amount: need.must_pay_this_month,
            class: "tier-must-pay",
            note: None,
        },
    ];
    if need.must_pay_coming.minor_units != 0 {
        rows.push(LegendRow {
            label: format!("Must pay, coming up by {}", day(through)),
            amount: need.must_pay_coming,
            class: "tier-must-pay",
            note: None,
        });
    }
    rows.push(LegendRow {
        label: "Essentials".to_owned(),
        amount: need.essentials,
        class: "tier-essentials",
        note: None,
    });
    rows.push(LegendRow {
        label: "Flexible".to_owned(),
        amount: need.flexible,
        class: "tier-flexible",
        note: None,
    });
    if row.set_aside.minor_units != 0 {
        rows.push(LegendRow {
            label: "Set aside for goals".to_owned(),
            amount: row.set_aside,
            class: "tier-saving",
            note: None,
        });
    }
    rows.push(LegendRow {
        label: "Emergency fund".to_owned(),
        amount: row.emergency_fund_held.unwrap_or(zero),
        class: "tier-emergency",
        note: Some(held_note(row)),
    });
    rows.push(LegendRow {
        label: "Free money".to_owned(),
        amount: row.free_money.unwrap_or(zero),
        class: FREE_CLASS,
        note: None,
    });
    rows
}

/// The bar's parts: everything spoken for, then what is free.
fn bar_parts(row: &FreeMoneyCurrency) -> Vec<Part> {
    let need = &row.still_need;
    let part = |amount: Money, class| Part {
        minor: amount.minor_units,
        class,
    };
    let held = row.emergency_fund_held.map_or(0, |held| held.minor_units);
    vec![
        part(need.must_pay_this_month, "tier-must-pay"),
        part(need.must_pay_coming, "tier-must-pay"),
        part(need.essentials, "tier-essentials"),
        part(need.flexible, "tier-flexible"),
        part(row.set_aside, "tier-saving"),
        Part {
            minor: held,
            class: "tier-emergency",
        },
        Part {
            minor: row.free_money.map_or(0, |free| free.minor_units),
            class: FREE_CLASS,
        },
    ]
}

/// The currencies with the display currency first, then the rest by code. Without a display
/// currency, or one nothing is held in, the first by code leads.
fn in_order(answer: &FreeMoney, display: Option<Currency>) -> Vec<&FreeMoneyCurrency> {
    let mut rows: Vec<&FreeMoneyCurrency> = answer.currencies.iter().collect();
    rows.sort_by_key(|row| (Some(row.currency) != display, row.currency));
    rows
}

/// Why there is no number, after "No number yet:". A currency's line gives its own accounts'
/// blockers; one that is not per currency, such as no account counting as cash, shows under
/// every blocked line. With no currency (nothing is held), every blocker is given.
fn why(blockers: &[Blocker], currency: Option<Currency>) -> String {
    blockers
        .iter()
        .filter(|blocker| match (blocker, currency) {
            (Blocker::BalanceMissing { currency: own, .. }, Some(row)) => *own == row,
            _ => true,
        })
        .map(blocker_sentence)
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn card(
    answer: &FreeMoney,
    names: &Names<'_>,
    display: Option<Currency>,
    can_manage: bool,
) -> Markup {
    let rows = in_order(answer, display);
    html! {
        section.card.home-free id="free-money" {
            h2 { "Free money" }
            @if let Some((lead, others)) = rows.split_first() {
                (lead_currency(lead, answer))
                @if !others.is_empty() {
                    ul.other-currencies {
                        @for row in others {
                            li { (row.currency.as_str()) ": " (one_line(row, &answer.blockers)) }
                        }
                    }
                }
                @if display.is_none() && can_manage && rows.len() > 1 {
                    p.fine { "Choose a display currency on the Household page to pick which one leads." }
                }
            } @else {
                p { "No number yet: " (why(&answer.blockers, None)) }
            }
            (worth_knowing(answer, names, can_manage))
            p { a href="/forecast" { "How this is worked out" } }
        }
    }
}

fn one_line(row: &FreeMoneyCurrency, blockers: &[Blocker]) -> String {
    hero_sentence(row)
        .unwrap_or_else(|| format!("No number yet: {}", why(blockers, Some(row.currency))))
}

/// The leading currency: the answer large, the bar, and the legend that says every part.
fn lead_currency(row: &FreeMoneyCurrency, answer: &FreeMoney) -> Markup {
    let Some(sentence) = hero_sentence(row) else {
        return html! {
            p { (row.currency.as_str()) ": No number yet: " (why(&answer.blockers, Some(row.currency))) }
        };
    };
    let short = row.shortfall.is_some_and(|short| short.minor_units > 0);
    html! {
        p class={ "hero-number" @if short { " answer-short" } } { (sentence) }
        (bar(&bar_parts(row), row.cash_available.minor_units))
        dl.legend {
            @for part in legend_rows(row, answer.must_pay_through) {
                div {
                    dt {
                        @if !part.class.is_empty() {
                            span class={ "tier-mark " (part.class) } {}
                            " "
                        }
                        (part.label)
                    }
                    dd {
                        (money(part.amount))
                        @if let Some(note) = &part.note {
                            " " span.fine { (note) }
                        }
                    }
                }
            }
        }
    }
}

/// Cautions after the answer, never above it.
fn worth_knowing(answer: &FreeMoney, names: &Names<'_>, can_manage: bool) -> Markup {
    html! {
        @if !answer.cautions.is_empty() {
            h3 { "Worth knowing" }
            ul.reasons {
                @for caution in &answer.cautions {
                    li {
                        (caution_sentence(caution, answer.as_of, names))
                        @if matches!(caution, Caution::EmergencyFundNotSet { .. }) {
                            " "
                            (emergency_fund_next_step(can_manage))
                        }
                    }
                }
            }
        }
    }
}
