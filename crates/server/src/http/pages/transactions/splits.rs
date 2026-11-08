//! Filing a transaction: in one category, or split across several with amounts that must add up
//! to it, each part perhaps counting towards a goal. The page reads the form exactly and sends
//! the splits as the API takes them; the service checks they add up, and its refusal is shown.
//!
//! Amounts are typed in the transaction's direction, so a purchase of 12.50 splits into 10.00
//! and 2.50, not -10.00 and -2.50; a minus sign is a part going the other way.

use std::collections::BTreeMap;

use coffer_domain::money::{Currency, DecimalAmount, format_minor_units};
use coffer_domain::transactions::{ReviewStatus, SplitInput};
use maud::{Markup, html};

use super::{Names, detail_path};
use crate::http::pages::PageError;
use crate::http::pages::layout::csrf_field;
use crate::ledger::transactions::TransactionView;

/// The most parts a transaction takes, as the API says.
const MAX_SPLITS: usize = 50;
/// Empty rows the form offers beyond the parts a transaction has.
const SPARE_ROWS: usize = 2;
/// Rows the form offers at least.
const MIN_ROWS: usize = 3;

/// The field a quick filing names to uncategorize.
pub(super) const NO_CATEGORY: &str = "none";

/// `1` for a transaction of money in, `-1` for money out: what a typed amount is multiplied
/// by to become a split's signed amount.
fn direction(transaction: &TransactionView) -> i64 {
    if transaction.amount.minor_units < 0 {
        -1
    } else {
        1
    }
}

/// `minor` as typed in the direction `sign` gives: as it is for money in, its sign turned for
/// money out. Written from the exact text, so no amount overflows.
fn typed(minor: i64, sign: i64, currency: Currency) -> String {
    let plain = format_minor_units(minor, currency);
    if sign > 0 || minor == 0 {
        return plain;
    }
    plain
        .strip_prefix('-')
        .map_or_else(|| format!("-{plain}"), str::to_owned)
}

/// The form that files the whole amount in one category, and marks it reviewed with it.
pub(super) fn category_form(transaction: &TransactionView, names: &Names, csrf: &str) -> Markup {
    let current = match transaction.splits.as_slice() {
        [only] => Some(only.category),
        _ => None,
    };
    let unreviewed = transaction.review == ReviewStatus::Unreviewed;
    html! {
        form method="post" action={ (detail_path(transaction.id)) "/category" } {
            (csrf_field(csrf))
            label.field {
                "Category"
                select.input name="category" required {
                    option value="" selected[current.is_none() && !transaction.splits.is_empty()] {
                        "Choose a category"
                    }
                    option value=(NO_CATEGORY) selected[transaction.splits.is_empty()] {
                        "Uncategorized"
                    }
                    @for category in names.offered_categories() {
                        option value=(category.id) selected[current == Some(category.id)] {
                            (category.label)
                        }
                    }
                }
            }
            @if unreviewed {
                label.choice {
                    input type="checkbox" name="reviewed" value="yes" checked;
                    "Mark it reviewed too"
                }
            }
            p.fine {
                "This files the whole amount in one category, in place of any parts it has, and "
                "a part's goal goes with them. Rules never change it after you do."
            }
            button.btn.btn-primary type="submit" { "Save category" }
        }
    }
}

/// The form that splits the amount across categories, one row a part.
pub(super) fn splits_form(transaction: &TransactionView, names: &Names, csrf: &str) -> Markup {
    let sign = direction(transaction);
    let currency = transaction.amount.currency;
    let rows = (transaction.splits.len() + SPARE_ROWS).clamp(MIN_ROWS, MAX_SPLITS);
    let goals: Vec<_> = names
        .goals
        .iter()
        .filter(|goal| goal.currency == currency)
        .collect();
    html! {
        details.item-detail open[transaction.splits.len() > 1] {
            summary { "Split it across categories" }
            form method="post" action={ (detail_path(transaction.id)) "/splits" } {
                (csrf_field(csrf))
                p.fine {
                    "The parts must add up to " (typed(transaction.amount.minor_units, sign, currency))
                    " " (currency.as_str()) ". Type each in the same direction as the transaction; "
                    "put a minus sign before a part that goes the other way. Leave a row empty to "
                    "drop it. Only a part in a spending category can count towards a goal."
                }
                @for index in 0..rows {
                    @let split = transaction.splits.get(index);
                    fieldset.choices {
                        legend { "Part " (index + 1) }
                        label.field {
                            "Category"
                            select.input name={ "category_" (index) } {
                                option value="" { "None" }
                                @for category in names.offered_categories() {
                                    option value=(category.id)
                                        selected[split.is_some_and(|split| split.category == category.id)] {
                                        (category.label)
                                    }
                                }
                            }
                        }
                        label.field {
                            "Amount"
                            input.input type="text" inputmode="decimal" autocomplete="off"
                                name={ "amount_" (index) }
                                value=[split.map(|split| typed(split.amount.minor_units, sign, currency))];
                        }
                        @if !goals.is_empty() {
                            label.field {
                                "Counts towards"
                                select.input name={ "goal_" (index) } {
                                    option value="" { "No goal" }
                                    @for goal in &goals {
                                        option value=(goal.id)
                                            selected[split.is_some_and(|split| split.goal == Some(goal.id))] {
                                            (goal.name)
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                button.btn.btn-primary type="submit" { "Save parts" }
            }
        }
    }
}

/// One row as sent: each field optional, since a row may be left empty.
#[derive(Default)]
struct Row<'a> {
    category: &'a str,
    amount: &'a str,
    goal: &'a str,
}

/// The splits a splits form sent, in row order, for `transaction`. Fields are `category_<n>`,
/// `amount_<n>` and `goal_<n>` for a row `n` the form can offer; anything else is refused whole
/// and never echoed. A row with neither a category nor an amount is left out.
pub(super) fn parse(
    transaction: &TransactionView,
    fields: &BTreeMap<String, String>,
) -> Result<Vec<SplitInput>, PageError> {
    let unreadable = || PageError::invalid("the form could not be read");
    let mut rows: BTreeMap<usize, Row<'_>> = BTreeMap::new();
    for (name, value) in fields {
        let (field, index) = name.rsplit_once('_').ok_or_else(unreadable)?;
        let index: usize = index.parse().map_err(|_| unreadable())?;
        if index >= MAX_SPLITS {
            return Err(unreadable());
        }
        let row = rows.entry(index).or_default();
        match field {
            "category" => row.category = value,
            "amount" => row.amount = value,
            "goal" => row.goal = value,
            _ => return Err(unreadable()),
        }
    }
    let sign = direction(transaction);
    let currency = transaction.amount.currency;
    let mut splits = Vec::new();
    for row in rows.values() {
        let amount = row.amount.trim();
        if row.category.is_empty() && amount.is_empty() {
            continue;
        }
        if row.category.is_empty() || amount.is_empty() {
            return Err(PageError::invalid(
                "each part needs a category and an amount",
            ));
        }
        let category = row
            .category
            .parse()
            .map_err(|_| PageError::invalid("not a category the form offers"))?;
        let typed = DecimalAmount::parse(amount)
            .and_then(|amount| amount.to_minor_units(currency))
            .map_err(|_| {
                PageError::invalid(
                    "an amount is a number such as 12.50, with no more decimals than its currency has",
                )
            })?;
        let minor_units = typed
            .checked_mul(sign)
            .ok_or_else(|| PageError::invalid("an amount is too large"))?;
        let goal = match row.goal {
            "" => None,
            text => Some(
                text.parse()
                    .map_err(|_| PageError::invalid("not a goal the form offers"))?,
            ),
        };
        splits.push(SplitInput {
            category,
            minor_units,
            goal,
        });
    }
    Ok(splits)
}
