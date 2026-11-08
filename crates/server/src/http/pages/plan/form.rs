//! The fields of a planned item's form, shared by adding and changing one, and how the form
//! is read into what the ledger takes. The page reads only the fields the chosen kind of
//! schedule needs; every rule about what an item may hold stays in the ledger, and every
//! refusal here is fixed text that never echoes the input.

use coffer_domain::ids::{AccountId, CategoryId};
use coffer_domain::money::{Currency, DecimalAmount, Money, format_minor_units};
use coffer_domain::plan::{
    IntervalUnit, PlanDirection, PlanItemType, PlanSchedule, PlanSpending, spending_applies,
    strict_date,
};
use coffer_domain::transactions::CategoryKind;
use jiff::civil::Date;
use maud::{Markup, html};
use serde::Deserialize;

use super::words::unit_word;
use crate::http::pages::PageError;
use crate::ledger::accounts::AccountView;
use crate::ledger::categories::CategoryGroupView;
use crate::ledger::plan_items::{PlannedItemInput, PlannedItemPatch, PlannedItemView};
use crate::ledger::transactions::Patch;

/// What the form offers to choose from.
pub(super) struct Choices<'a> {
    pub(super) groups: &'a [CategoryGroupView],
    /// Every account the viewer sees, closed ones too, so an old hint keeps its name.
    pub(super) accounts: &'a [AccountView],
    /// The currency chosen for a new item.
    pub(super) currency: Option<Currency>,
}

impl Choices<'_> {
    /// Whether a new item has a category to use: one for spending or income, with it and its
    /// group not archived.
    pub(super) fn has_usable_category(&self) -> bool {
        self.groups
            .iter()
            .filter(|group| !group.archived)
            .any(|group| {
                group
                    .categories
                    .iter()
                    .any(|category| !category.archived && category.kind != CategoryKind::Transfer)
            })
    }
}

/// A planned item's form. Unknown fields are refused: a forged form fails whole.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ItemForm {
    #[serde(default)]
    pub(super) csrf: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    direction: String,
    #[serde(default)]
    category: String,
    #[serde(default)]
    amount: String,
    #[serde(default)]
    currency: String,
    #[serde(default, rename = "type")]
    item_type: String,
    #[serde(default)]
    every: String,
    #[serde(default)]
    unit: String,
    #[serde(default)]
    anchor_on: String,
    #[serde(default)]
    end_on: String,
    #[serde(default)]
    on: String,
    #[serde(default)]
    start_on: String,
    #[serde(default)]
    due_on: String,
    #[serde(default)]
    completed: String,
    #[serde(default)]
    spending: String,
    #[serde(default)]
    account_hint: String,
    #[serde(default)]
    notes: String,
    #[serde(default)]
    active: String,
}

/// An item as the form says it, read but not yet checked by the ledger.
pub(super) struct ItemFields {
    name: String,
    category: CategoryId,
    amount: Money,
    direction: PlanDirection,
    account_hint: Option<AccountId>,
    notes: String,
    active: bool,
    schedule: PlanSchedule,
    spending: Option<PlanSpending>,
}

impl ItemFields {
    pub(super) fn into_input(self) -> PlannedItemInput {
        PlannedItemInput {
            name: self.name,
            category: self.category,
            amount: self.amount,
            direction: self.direction,
            account_hint: self.account_hint,
            notes: Some(self.notes),
            active: Some(self.active),
            schedule: self.schedule,
            spending: self.spending,
        }
    }

    /// Every field, so the ledger changes what differs: empty notes clear them, and no
    /// account clears the hint.
    pub(super) fn into_patch(self) -> PlannedItemPatch {
        PlannedItemPatch {
            name: Some(self.name),
            category: Some(self.category),
            amount: Some(self.amount),
            direction: Some(self.direction),
            account_hint: self.account_hint.map_or(Patch::Clear, Patch::Set),
            notes: Patch::Set(self.notes),
            active: Some(self.active),
            schedule: Some(self.schedule),
            spending: self.spending,
        }
    }
}

fn day_of(text: &str, refusal: &'static str) -> Result<Date, PageError> {
    strict_date::parse(text).ok_or_else(|| PageError::invalid(refusal))
}

fn checkbox(text: &str) -> Result<bool, PageError> {
    match text {
        "yes" => Ok(true),
        "" => Ok(false),
        _ => Err(PageError::invalid("not an answer the form offers")),
    }
}

fn schedule_of(form: &ItemForm) -> Result<PlanSchedule, PageError> {
    let item_type: PlanItemType = form
        .item_type
        .parse()
        .map_err(|_| PageError::invalid("not a kind of schedule the form offers"))?;
    Ok(match item_type {
        PlanItemType::Recurring => PlanSchedule::Recurring {
            every: form
                .every
                .trim()
                .parse()
                .map_err(|_| PageError::invalid("how often is a whole number, such as 1"))?,
            unit: form
                .unit
                .parse()
                .map_err(|_| PageError::invalid("not a unit the form offers"))?,
            anchor_on: day_of(&form.anchor_on, "the first day is not a real day")?,
            end_on: match form.end_on.as_str() {
                "" => None,
                text => Some(day_of(text, "the last day is not a real day")?),
            },
        },
        PlanItemType::OneOff => PlanSchedule::OneOff {
            on: day_of(&form.on, "the day is not a real day")?,
        },
        PlanItemType::Goal => PlanSchedule::Goal {
            start_on: day_of(
                &form.start_on,
                "the day to start setting aside is not a real day",
            )?,
            due_on: day_of(&form.due_on, "the day it is due is not a real day")?,
            completed: checkbox(&form.completed)?,
        },
    })
}

impl ItemForm {
    /// The item the form says, with the fixed refusal of the first field that can't be read.
    pub(super) fn fields(self) -> Result<ItemFields, PageError> {
        let direction: PlanDirection = self
            .direction
            .parse()
            .map_err(|_| PageError::invalid("not a direction the form offers"))?;
        let category = self
            .category
            .parse()
            .map_err(|_| PageError::invalid("not a category the form offers"))?;
        let currency = Currency::parse(&self.currency)
            .map_err(|_| PageError::invalid("not a currency coffer knows"))?;
        let minor_units = DecimalAmount::parse(self.amount.trim())
            .and_then(|amount| amount.to_minor_units(currency))
            .map_err(|_| {
                PageError::invalid(
                    "an amount is a number such as 1234.56, with no more decimals than its \
                     currency has",
                )
            })?;
        let schedule = schedule_of(&self)?;
        // A kind of spending belongs to a recurring outflow alone: the field is on the form
        // whatever is chosen, and read only where the ledger says it applies.
        let spending = if spending_applies(direction, &schedule) && !self.spending.is_empty() {
            Some(
                self.spending
                    .parse()
                    .map_err(|_| PageError::invalid("not a kind of spending the form offers"))?,
            )
        } else {
            None
        };
        let account_hint = match self.account_hint.as_str() {
            "" => None,
            text => Some(
                text.parse()
                    .map_err(|_| PageError::invalid("not an account the form offers"))?,
            ),
        };
        Ok(ItemFields {
            name: self.name,
            category,
            amount: Money {
                minor_units,
                currency,
            },
            direction,
            account_hint,
            notes: self.notes,
            active: checkbox(&self.active)?,
            schedule,
            spending,
        })
    }
}

const UNITS: [IntervalUnit; 4] = [
    IntervalUnit::Days,
    IntervalUnit::Weeks,
    IntervalUnit::Months,
    IntervalUnit::Years,
];

/// The form's fields, filled from `item` when it changes one. A new item offers each kind of
/// schedule with its fields; an existing one keeps its kind, so only its fields are shown.
pub(super) fn fields(choices: &Choices<'_>, item: Option<&PlannedItemView>) -> Markup {
    let direction = item.map(|item| item.direction);
    let currency = item.map_or(choices.currency, |item| Some(item.amount.currency));
    let amount = item.map(|item| format_minor_units(item.amount.minor_units, item.amount.currency));
    html! {
        label.field {
            "Name, such as Rent"
            input.input type="text" name="name" required maxlength="100" autocomplete="off"
                value=[item.map(|item| item.name.as_str())];
        }
        fieldset.choices {
            legend { "Which way the money goes" }
            label.choice {
                input type="radio" name="direction" value="outflow" required
                    checked[direction != Some(PlanDirection::Inflow)];
                "Out: a bill, a budget, a one-off or a goal"
            }
            label.choice {
                input type="radio" name="direction" value="inflow"
                    checked[direction == Some(PlanDirection::Inflow)];
                "In: income"
            }
        }
        (category_field(choices, item))
        label.field {
            "Amount, such as 1234.56"
            input.input type="text" name="amount" required inputmode="decimal"
                autocomplete="off" value=[amount];
        }
        label.field {
            "Currency"
            select.input name="currency" required {
                @if currency.is_none() {
                    option value="" selected { "Choose a currency" }
                }
                @for one in Currency::all() {
                    option value=(one.as_str()) selected[currency == Some(one)] { (one.as_str()) }
                }
            }
        }
        @match item {
            Some(item) => {
                input type="hidden" name="type" value=(item.schedule.item_type().as_str());
                @match item.schedule {
                    PlanSchedule::Recurring { .. } => (recurring_fields(item)),
                    PlanSchedule::OneOff { .. } => (one_off_fields(Some(item))),
                    PlanSchedule::Goal { .. } => (goal_fields(Some(item))),
                }
            }
            None => {
                label.field {
                    "When it falls due"
                    select.input name="type" required {
                        option value="" selected { "Choose how often" }
                        option value=(PlanItemType::Recurring.as_str()) { "It repeats" }
                        option value=(PlanItemType::OneOff.as_str()) { "It happens once" }
                        option value=(PlanItemType::Goal.as_str()) { "It's a goal to save for" }
                    }
                    span.fine { "Fill in the fields below for that choice only; the others are left alone." }
                }
                fieldset.choices {
                    legend { "If it repeats" }
                    (recurring_inputs(None))
                }
                fieldset.choices {
                    legend { "If it happens once" }
                    (one_off_fields(None))
                }
                fieldset.choices {
                    legend { "If it's a goal" }
                    (goal_fields(None))
                }
            }
        }
        (account_field(choices, item))
        label.field {
            "Notes"
            textarea.input name="notes" rows="2" maxlength="2000" {
                (item.and_then(|item| item.notes.as_deref()).unwrap_or_default())
            }
        }
        label.choice {
            input type="checkbox" name="active" value="yes" checked[item.is_none_or(|item| item.active)];
            "Counts in the forecast"
        }
    }
}

fn recurring_fields(item: &PlannedItemView) -> Markup {
    html! {
        p.fine { "It repeats. A planned item's kind of schedule never changes." }
        (recurring_inputs(Some(item)))
    }
}

fn recurring_inputs(item: Option<&PlannedItemView>) -> Markup {
    let (every, unit, anchor_on, end_on) = match item.map(|item| item.schedule) {
        Some(PlanSchedule::Recurring {
            every,
            unit,
            anchor_on,
            end_on,
        }) => (Some(every), Some(unit), Some(anchor_on), end_on),
        _ => (None, None, None, None),
    };
    let spending = item.and_then(|item| item.spending);
    html! {
        label.field {
            "Every how many"
            input.input type="number" name="every" min="1" max="999" step="1"
                value=(every.unwrap_or(1));
        }
        label.field {
            "Of what"
            select.input name="unit" {
                @for one in UNITS {
                    option value=(one.as_str())
                        selected[unit.map_or(one == IntervalUnit::Months, |unit| unit == one)] {
                        (unit_word(one, true))
                    }
                }
            }
        }
        label.field {
            "First day it falls on"
            input.input type="date" name="anchor_on" value=[anchor_on.map(|day| day.to_string())];
        }
        label.field {
            "Last day it may fall on, if it ends"
            input.input type="date" name="end_on" value=[end_on.map(|day| day.to_string())];
        }
        label.field {
            "For money going out, how it counts"
            select.input name="spending" {
                option value=(PlanSpending::Bill.as_str())
                    selected[spending != Some(PlanSpending::Budget)] {
                    "A bill: it counts in full"
                }
                option value=(PlanSpending::Budget.as_str())
                    selected[spending == Some(PlanSpending::Budget)] {
                    "A budget: this month's spending in its category comes off it"
                }
            }
        }
    }
}

fn one_off_fields(item: Option<&PlannedItemView>) -> Markup {
    let on = match item.map(|item| item.schedule) {
        Some(PlanSchedule::OneOff { on }) => Some(on.to_string()),
        _ => None,
    };
    html! {
        label.field {
            "The day"
            input.input type="date" name="on" value=[on];
        }
    }
}

fn goal_fields(item: Option<&PlannedItemView>) -> Markup {
    let (start_on, due_on, completed) = match item.map(|item| item.schedule) {
        Some(PlanSchedule::Goal {
            start_on,
            due_on,
            completed,
        }) => (
            Some(start_on.to_string()),
            Some(due_on.to_string()),
            completed,
        ),
        _ => (None, None, false),
    };
    html! {
        p.fine { "The amount is the target, set aside evenly from the first day to the day it is due." }
        label.field {
            "Start setting aside on"
            input.input type="date" name="start_on" value=[start_on];
        }
        label.field {
            "Due on"
            input.input type="date" name="due_on" value=[due_on];
        }
        label.choice {
            input type="checkbox" name="completed" value="yes" checked[completed];
            "Completed: it needs no more money"
        }
    }
}

/// The categories a planned item may use, by group, and the item's own whatever it is now.
fn category_field(choices: &Choices<'_>, item: Option<&PlannedItemView>) -> Markup {
    let current = item.map(|item| item.category);
    html! {
        label.field {
            "Category: one for spending when money goes out, for income when it comes in"
            select.input name="category" required {
                @if current.is_none() {
                    option value="" selected { "Choose a category" }
                }
                @for group in choices.groups {
                    @let usable: Vec<_> = group.categories.iter().filter(|category| {
                        Some(category.id) == current
                            || (!group.archived && !category.archived
                                && category.kind != CategoryKind::Transfer)
                    }).collect();
                    @if !usable.is_empty() {
                        optgroup label=(group.name) {
                            @for category in usable {
                                option value=(category.id) selected[Some(category.id) == current] {
                                    (category.name)
                                    @match category.kind {
                                        CategoryKind::Expense => " (spending)",
                                        CategoryKind::Income => " (income)",
                                        CategoryKind::Transfer => " (transfer)",
                                    }
                                    @if category.archived || group.archived { ", archived" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The open accounts an item may be expected on, and the item's own whatever it is now.
fn account_field(choices: &Choices<'_>, item: Option<&PlannedItemView>) -> Markup {
    let current = item.and_then(|item| item.account_hint);
    html! {
        label.field {
            "Account it is expected on, if any"
            select.input name="account_hint" {
                option value="" selected[current.is_none()] { "No particular account" }
                @for account in choices.accounts.iter().filter(|account| !account.closed || Some(account.id) == current) {
                    option value=(account.id) selected[Some(account.id) == current] {
                        (account.name)
                        @if account.closed { " (closed)" }
                    }
                }
            }
        }
    }
}
