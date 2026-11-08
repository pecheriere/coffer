//! Changing a rule from its page, through [`Ledger::update_rule`](crate::ledger::Ledger::update_rule)
//! as `PATCH /api/v1/rules/{id}` does. The form carries what it showed, so only the fields the
//! person changed are sent: someone else's change to another field meanwhile stays. A field the
//! person changed that also changed since the page opened is refused with fixed words, so a
//! stale page never puts back what someone else set. The ledger writes and audits only what
//! differs, and leaves what the rule already did as it is.

use std::collections::BTreeSet;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Redirect;
use coffer_domain::ids::RuleId;
use coffer_domain::policy::{Action, authorize};
use serde::Deserialize;

use super::form::{RuleForm, Shown};
use super::refused;
use crate::auth::AuthError;
use crate::http::AppState;
use crate::http::pages::{PageError, PageForm, PageSession};
use crate::http::problem::{Problem, ProblemCode};
use crate::ledger::rules::RulePatch;
use crate::ledger::transactions::Patch;

const CHANGED: &str = "Someone changed this rule after the page opened, in a field you changed \
                       too, so this save wasn't applied. Open the rule again to see it as it is.";

/// The change form: the rule's fields, and `shown_*`, what the page showed of them. Unknown
/// fields are refused: a forged form fails whole.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ChangeForm {
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
    #[serde(default)]
    shown_match_kind: String,
    #[serde(default)]
    shown_pattern: String,
    #[serde(default)]
    shown_account: String,
    #[serde(default)]
    shown_amount_min: String,
    #[serde(default)]
    shown_amount_max: String,
    #[serde(default)]
    shown_amount_currency: String,
    #[serde(default)]
    shown_category: String,
    #[serde(default)]
    shown_assignee: String,
    #[serde(default)]
    shown_position: String,
    #[serde(default)]
    shown_enabled: String,
}

impl ChangeForm {
    /// The rule as sent, and what the page showed.
    fn split(self) -> (RuleForm, Shown) {
        let shown = Shown {
            match_kind: self.shown_match_kind,
            pattern: self.shown_pattern,
            account: self.shown_account,
            amount_min: self.shown_amount_min,
            amount_max: self.shown_amount_max,
            amount_currency: self.shown_amount_currency,
            category: self.shown_category,
            assignee: self.shown_assignee,
            position: self.shown_position,
            enabled: self.shown_enabled,
        };
        let sent = RuleForm {
            csrf: self.csrf,
            match_kind: self.match_kind,
            pattern: self.pattern,
            account: self.account,
            amount_min: self.amount_min,
            amount_max: self.amount_max,
            amount_currency: self.amount_currency,
            category: self.category,
            assignee: self.assignee,
            position: self.position,
            enabled: self.enabled,
        };
        (sent, shown)
    }
}

/// A field of a rule's form. The amount's lowest, highest and currency are one field: they make
/// one range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Field {
    MatchKind,
    Pattern,
    Account,
    Amount,
    Category,
    Assignee,
    Position,
    Enabled,
}

/// The fields that differ between two of a rule's forms.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Differing(BTreeSet<Field>);

impl Differing {
    fn between(one: &Shown, other: &Shown) -> Self {
        let amount = |shown: &Shown| {
            [
                shown.amount_min.clone(),
                shown.amount_max.clone(),
                shown.amount_currency.clone(),
            ]
        };
        let fields = [
            (Field::MatchKind, one.match_kind != other.match_kind),
            (Field::Pattern, one.pattern != other.pattern),
            (Field::Account, one.account != other.account),
            (Field::Amount, amount(one) != amount(other)),
            (Field::Category, one.category != other.category),
            (Field::Assignee, one.assignee != other.assignee),
            (Field::Position, one.position != other.position),
            (Field::Enabled, one.enabled != other.enabled),
        ];
        Self(
            fields
                .into_iter()
                .filter_map(|(field, differs)| differs.then_some(field))
                .collect(),
        )
    }

    fn has(&self, field: Field) -> bool {
        self.0.contains(&field)
    }

    /// Whether a field differs in both.
    fn overlaps(&self, other: &Self) -> bool {
        !self.0.is_disjoint(&other.0)
    }
}

/// The patch of a field the person changed, or none of one they left.
fn patch_of<T>(changed: bool, value: Option<T>) -> Patch<T> {
    match (changed, value) {
        (false, _) => Patch::Absent,
        (true, None) => Patch::Clear,
        (true, Some(value)) => Patch::Set(value),
    }
}

fn changed_meanwhile() -> PageError {
    PageError(
        Problem::new(
            StatusCode::CONFLICT,
            ProblemCode::Conflict,
            "Changed meanwhile",
        )
        .with_detail(CHANGED.to_owned()),
    )
}

/// Changes a rule through the service the API uses, sending only what the person changed. An
/// empty order keeps the rule's place. The household's right to write is checked before the
/// form is read; the rule's account is the ledger's to check.
pub(super) async fn update(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    form: Result<PageForm<ChangeForm>, PageError>,
) -> Result<Redirect, PageError> {
    authorize(&session.actor(), Action::WriteHousehold).map_err(AuthError::from)?;
    let id: RuleId = id.parse().map_err(|_| PageError::not_found())?;
    let PageForm(form) = form?;
    let (mut sent, shown) = form.split();
    let actor = session.verify(&state, &sent.csrf)?;
    let current = Shown::of(&state.ledger.get_rule(&actor, id).await.map_err(refused)?);
    let asked = Differing::between(&sent.sent(), &shown);
    if asked.overlaps(&Differing::between(&current, &shown)) {
        return Err(changed_meanwhile());
    }
    if !asked.has(Field::Account) {
        // A range sent without a currency is in the rule's account as it is now.
        sent.account = current.account;
    }
    let fields = sent.read(&state, &actor).await?;
    let patch = RulePatch {
        match_kind: asked.has(Field::MatchKind).then_some(fields.match_kind),
        pattern: asked.has(Field::Pattern).then_some(fields.pattern),
        account: patch_of(asked.has(Field::Account), fields.account),
        amount: patch_of(asked.has(Field::Amount), fields.amount),
        category: patch_of(asked.has(Field::Category), fields.category),
        assignee: patch_of(asked.has(Field::Assignee), fields.assignee),
        position: fields.position.filter(|_| asked.has(Field::Position)),
        enabled: asked.has(Field::Enabled).then_some(fields.enabled),
    };
    state
        .ledger
        .update_rule(&actor, id, patch)
        .await
        .map_err(refused)?;
    Ok(Redirect::to(&format!("/rules/{id}")))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{Differing, Field, Shown};

    fn rule() -> Shown {
        Shown {
            match_kind: "contains".to_owned(),
            pattern: "uber".to_owned(),
            amount_min: "-50.00".to_owned(),
            amount_currency: "USD".to_owned(),
            category: "groceries".to_owned(),
            position: "3".to_owned(),
            enabled: "yes".to_owned(),
            ..Shown::default()
        }
    }

    #[test]
    fn the_same_form_differs_nowhere() {
        assert_eq!(Differing::between(&rule(), &rule()), Differing::default());
    }

    #[test]
    fn any_end_or_the_currency_of_a_range_changes_the_whole_amount() {
        for change in [
            |shown: &mut Shown| shown.amount_min = "-40.00".to_owned(),
            |shown: &mut Shown| shown.amount_max = "-10.00".to_owned(),
            |shown: &mut Shown| shown.amount_currency = "EUR".to_owned(),
        ] {
            let mut other = rule();
            change(&mut other);
            let differing = Differing::between(&rule(), &other);
            assert_eq!(differing, Differing(BTreeSet::from([Field::Amount])));
        }
    }

    #[test]
    fn changes_to_different_fields_do_not_overlap() {
        let mut mine = rule();
        mine.enabled = String::new();
        let mut theirs = rule();
        theirs.category = "dining".to_owned();
        let asked = Differing::between(&mine, &rule());
        let moved = Differing::between(&theirs, &rule());
        assert!(!asked.overlaps(&moved));
    }

    #[test]
    fn a_change_to_a_field_that_moved_meanwhile_overlaps() {
        let mut mine = rule();
        mine.category = "travel".to_owned();
        mine.enabled = String::new();
        let mut theirs = rule();
        theirs.category = "dining".to_owned();
        let asked = Differing::between(&mine, &rule());
        let moved = Differing::between(&theirs, &rule());
        assert!(asked.overlaps(&moved));
    }
}
