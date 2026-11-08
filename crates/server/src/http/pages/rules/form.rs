//! The form that adds a rule or changes one, and the handlers behind it. The form holds every
//! field, so a change sends them all; the ledger writes and audits only what differs, and
//! leaves what the rule already did as it is. Every refusal is fixed text.

use axum::extract::{Path, State};
use axum::response::Redirect;
use coffer_domain::identity::Actor;
use coffer_domain::ids::{AccountId, CategoryId, MemberId, RuleId};
use coffer_domain::money::{Currency, DecimalAmount, format_minor_units};
use coffer_domain::policy::{Action, authorize};
use coffer_domain::rules::{RuleMatchKind, RulePattern};
use coffer_domain::transactions::MAX_POSITION;
use maud::{Markup, html};
use serde::Deserialize;

use super::words::match_kind_label;
use super::{Names, refused};
use crate::auth::AuthError;
use crate::http::AppState;
use crate::http::pages::layout::csrf_field;
use crate::http::pages::{PageError, PageForm, PageSession};
use crate::ledger::rules::{RuleAmount, RuleInput, RulePatch, RuleView};
use crate::ledger::transactions::Patch;

const MATCH_KINDS: [RuleMatchKind; 2] = [RuleMatchKind::Contains, RuleMatchKind::StartsWith];

/// What the form does when sent.
#[derive(Clone, Copy)]
pub(super) enum Target {
    Create,
    Change(RuleId),
}

/// The form's fields, filled from `rule` when there is one.
pub(super) fn fields(target: Target, rule: Option<&RuleView>, names: &Names, csrf: &str) -> Markup {
    let (action, button, order) = match target {
        Target::Create => (
            "/rules".to_owned(),
            "Add rule",
            "Order: lower runs first. Leave empty to run after the last rule.",
        ),
        Target::Change(id) => (
            format!("/rules/{id}"),
            "Save changes",
            "Order: lower runs first. Leave empty to keep its place.",
        ),
    };
    let amount = rule.and_then(|rule| rule.amount);
    let decimal = |minor: Option<i64>| {
        minor
            .zip(amount)
            .map(|(minor, amount)| format_minor_units(minor, amount.currency))
            .unwrap_or_default()
    };
    html! {
        form method="post" action=(action) {
            (csrf_field(csrf))
            label.field {
                "When the bank description"
                select.input name="match_kind" required {
                    @for kind in MATCH_KINDS {
                        option value=(kind.as_str())
                            selected[rule.is_some_and(|rule| rule.match_kind == kind)] {
                            (match_kind_label(kind))
                        }
                    }
                }
            }
            label.field {
                "This text, ignoring case. Spaces count: \u{201c}TAX \u{201d} doesn't match TAXI."
                input.input type="text" name="pattern" required maxlength=(RulePattern::MAX_CHARS)
                    autocomplete="off" value=[rule.map(|rule| rule.pattern.as_str())];
            }
            (account_field(rule, names))
            fieldset.choices {
                legend { "Only these amounts (optional)" }
                p.fine {
                    "Money out is negative: for payments of 10.00 to 50.00, enter -50.00 as the "
                    "lowest and -10.00 as the highest. Leave both empty for any amount."
                }
                label.field {
                    "Lowest"
                    input.input type="text" name="amount_min" inputmode="decimal" autocomplete="off"
                        value=(decimal(amount.and_then(|amount| amount.min_minor)));
                }
                label.field {
                    "Highest"
                    input.input type="text" name="amount_max" inputmode="decimal" autocomplete="off"
                        value=(decimal(amount.and_then(|amount| amount.max_minor)));
                }
                label.field {
                    "Currency"
                    select.input name="amount_currency" {
                        option value="" selected[amount.is_none()] { "The account's currency" }
                        @for currency in Currency::all() {
                            option value=(currency.as_str())
                                selected[amount.is_some_and(|amount| amount.currency == currency)] {
                                (currency.as_str())
                            }
                        }
                    }
                }
            }
            (category_field(rule, names))
            (assignee_field(rule, names))
            label.field {
                (order)
                input.input type="number" name="position" min="0" max=(MAX_POSITION)
                    value=[rule.map(|rule| rule.position)];
            }
            label.choice {
                input type="checkbox" name="enabled" value="yes"
                    checked[rule.is_none_or(|rule| rule.enabled)];
                "Run it on new transactions as they arrive"
            }
            button.btn.btn-primary type="submit" { (button) }
        }
    }
}

fn account_field(rule: Option<&RuleView>, names: &Names) -> Markup {
    let current = rule.and_then(|rule| rule.account);
    html! {
        label.field {
            "On"
            select.input name="account" {
                option value="" selected[rule.is_some() && current.is_none()] { "Every account" }
                @for (id, account) in &names.accounts {
                    // A closed account takes no new rule, but one already for it stays offered.
                    @if !account.closed || current == Some(*id) {
                        option value=(id) selected[current == Some(*id)] {
                            (account.name) " (" (account.currency.as_str()) ")"
                            @if account.closed { ", closed" }
                        }
                    }
                }
            }
        }
    }
}

fn category_field(rule: Option<&RuleView>, names: &Names) -> Markup {
    let current = rule.and_then(|rule| rule.category);
    html! {
        label.field {
            "File under"
            select.input name="category" {
                option value="" selected[current.is_none()] { "No category" }
                @for group in &names.groups {
                    optgroup label=(group.name) {
                        @for category in &group.categories {
                            // An archived category takes no new rule, but one a rule already
                            // names stays offered, so saving another change keeps it.
                            @if !(category.archived || group.archived) || current == Some(category.id) {
                                option value=(category.id) selected[current == Some(category.id)] {
                                    (category.name)
                                    @if category.archived || group.archived { " (archived)" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn assignee_field(rule: Option<&RuleView>, names: &Names) -> Markup {
    let current = rule.and_then(|rule| rule.assignee);
    html! {
        label.field {
            "Assign to"
            select.input name="assignee" {
                option value="" selected[current.is_none()] { "Nobody" }
                @for (id, member) in &names.members {
                    @if !member.disabled || current == Some(*id) {
                        option value=(id) selected[current == Some(*id)] {
                            (member.name)
                            @if member.disabled { " (disabled)" }
                        }
                    }
                }
            }
        }
    }
}

/// Unknown fields are refused: a forged form fails whole.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RuleForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    match_kind: String,
    #[serde(default)]
    pattern: String,
    #[serde(default)]
    account: String,
    #[serde(default)]
    amount_min: String,
    #[serde(default)]
    amount_max: String,
    #[serde(default)]
    amount_currency: String,
    #[serde(default)]
    category: String,
    #[serde(default)]
    assignee: String,
    #[serde(default)]
    position: String,
    #[serde(default)]
    enabled: String,
}

/// The form read as a rule. Its text is checked by the ledger, which refuses with fixed words.
struct Fields {
    match_kind: RuleMatchKind,
    pattern: String,
    account: Option<AccountId>,
    amount: Option<RuleAmount>,
    category: Option<CategoryId>,
    assignee: Option<MemberId>,
    position: Option<u32>,
    enabled: bool,
}

/// `text` read as a `T`, or none when it is empty, the form's way of choosing nothing.
fn chosen<T: std::str::FromStr>(text: &str, refusal: &'static str) -> Result<Option<T>, PageError> {
    if text.is_empty() {
        return Ok(None);
    }
    text.parse()
        .map(Some)
        .map_err(|_| PageError::invalid(refusal))
}

fn minor(text: &str, currency: Currency) -> Result<Option<i64>, PageError> {
    if text.is_empty() {
        return Ok(None);
    }
    DecimalAmount::parse(text)
        .ok()
        .and_then(|amount| amount.to_minor_units(currency).ok())
        .map(Some)
        .ok_or_else(|| {
            PageError::invalid(
                "an amount is a number such as -12.50, with no more decimals than its currency has",
            )
        })
}

impl RuleForm {
    /// Reads the form. A range without a currency is in the account's, which is read through
    /// the service the API uses, so an account the viewer may not see reads as missing.
    async fn read(self, state: &AppState, actor: &Actor) -> Result<Fields, PageError> {
        let match_kind = self
            .match_kind
            .parse()
            .map_err(|_| PageError::invalid("not a way of matching the form offers"))?;
        let account: Option<AccountId> = chosen(&self.account, "not an account the form offers")?;
        let amount = if self.amount_min.is_empty() && self.amount_max.is_empty() {
            None
        } else {
            let named = if self.amount_currency.is_empty() {
                None
            } else {
                Some(
                    Currency::parse(&self.amount_currency)
                        .map_err(|_| PageError::invalid("not a currency coffer knows"))?,
                )
            };
            let currency = match (named, account) {
                (Some(currency), _) => currency,
                (None, Some(account)) => {
                    state
                        .ledger
                        .get_account(actor, account)
                        .await
                        .map_err(refused)?
                        .currency
                }
                (None, None) => {
                    return Err(PageError::invalid(
                        "amounts on every account need a currency",
                    ));
                }
            };
            Some(RuleAmount {
                currency,
                min_minor: minor(&self.amount_min, currency)?,
                max_minor: minor(&self.amount_max, currency)?,
            })
        };
        let position = chosen::<u32>(
            &self.position,
            "the order is a whole number from 0 to 10000",
        )?;
        let enabled = match self.enabled.as_str() {
            "" => false,
            "yes" => true,
            _ => return Err(PageError::invalid("not an answer the form offers")),
        };
        Ok(Fields {
            match_kind,
            pattern: self.pattern,
            account,
            amount,
            category: chosen(&self.category, "not a category the form offers")?,
            assignee: chosen(&self.assignee, "not a member the form offers")?,
            position,
            enabled,
        })
    }
}

fn set_or_clear<T>(value: Option<T>) -> Patch<T> {
    value.map_or(Patch::Clear, Patch::Set)
}

/// Adds a rule through the service the API uses, then opens its page, which shows what running
/// it would change. The household's right to write is checked before the form is read, so a
/// role that couldn't is refused whatever it sends; the account's is the ledger's to check.
pub(super) async fn create(
    State(state): State<AppState>,
    session: PageSession,
    form: Result<PageForm<RuleForm>, PageError>,
) -> Result<Redirect, PageError> {
    authorize(&session.actor(), Action::WriteHousehold).map_err(AuthError::from)?;
    let PageForm(form) = form?;
    let actor = session.verify(&state, &form.csrf)?;
    let fields = form.read(&state, &actor).await?;
    let input = RuleInput {
        match_kind: fields.match_kind,
        pattern: fields.pattern,
        account: fields.account,
        amount: fields.amount,
        category: fields.category,
        assignee: fields.assignee,
        position: fields.position,
        enabled: Some(fields.enabled),
    };
    let rule = state
        .ledger
        .create_rule(&actor, input, None)
        .await
        .map_err(refused)?;
    Ok(Redirect::to(&format!("/rules/{}", rule.id)))
}

/// Changes a rule through the service the API uses. An empty order keeps the rule's place.
pub(super) async fn update(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    form: Result<PageForm<RuleForm>, PageError>,
) -> Result<Redirect, PageError> {
    authorize(&session.actor(), Action::WriteHousehold).map_err(AuthError::from)?;
    let id: RuleId = id.parse().map_err(|_| PageError::not_found())?;
    let PageForm(form) = form?;
    let actor = session.verify(&state, &form.csrf)?;
    let fields = form.read(&state, &actor).await?;
    let patch = RulePatch {
        match_kind: Some(fields.match_kind),
        pattern: Some(fields.pattern),
        account: set_or_clear(fields.account),
        amount: set_or_clear(fields.amount),
        category: set_or_clear(fields.category),
        assignee: set_or_clear(fields.assignee),
        position: fields.position,
        enabled: Some(fields.enabled),
    };
    state
        .ledger
        .update_rule(&actor, id, patch)
        .await
        .map_err(refused)?;
    Ok(Redirect::to(&format!("/rules/{id}")))
}
