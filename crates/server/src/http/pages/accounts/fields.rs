//! The words and fields every form about an account uses, on this page and on Connections: the
//! kinds, whose money it is and who it is held for. A label may be a child's name: it goes only
//! to the service and to who sees its account, in a row, a form and the list of labels. Every
//! refusal is fixed words, and nothing here logs a field.

use std::collections::BTreeSet;
use std::fmt::Display;

use coffer_domain::accounts::{AccountError, AccountKind, BelongsTo};
use maud::{Markup, html};

use crate::ledger::accounts::AccountView;

pub(in crate::http::pages) const KIND_UNKNOWN: &str =
    "Choose one of the account kinds in the list.";
pub(in crate::http::pages) const BELONGS_TO_UNKNOWN: &str =
    "Choose one of the answers in the list for whose money it is.";
const KIND_FINE: &str = "Checking and savings count as cash in the forecast. The forecast counts \
    only cash and debts due soon. Education and custodial accounts are held for someone else, and \
    charitable ones are given away: they stay out of net worth unless you say they're the \
    household's.";
/// Who sees a label, for the hints of every form.
pub(in crate::http::pages) const SEEN_BY: &str = "A name or a purpose. Anyone who can see this \
    account sees it, on this page and in net worth, and it stays in the audit log.";
/// What money that isn't the household's does, said beside every answer to whose it is.
pub(in crate::http::pages) const NOT_THE_HOUSEHOLD_S: &str = "Money held for someone else, such \
    as a 529 or a custodial account for a child, or given away, such as a donor-advised fund, \
    stays out of net worth and out of the forecast. Net worth shows it apart, in a bucket beside \
    the total, and never adds it in.";
const NAME_INVALID: &str = "The name takes 1 to 100 characters, on one line.";
const HELD_FOR_INVALID: &str =
    "Held for takes a name or a purpose of up to 64 plain characters, on one line.";
const HELD_FOR_ON_HOUSEHOLD: &str =
    "Held for is only for money that isn't the household's: empty it, or choose another answer.";
const COUNTED_BUT_NOT_THE_HOUSEHOLD_S: &str = "Only the household's money can count in net worth \
    or the forecast: say it's the household's first.";
const STOPS_FOLLOWING: &str =
    "Picking an answer stops it following the kind for good, even the one it has now.";
const AUDITED: &str = "Every change goes in the audit log.";
const ANSWERS: [BelongsTo; 3] = [
    BelongsTo::Household,
    BelongsTo::SomeoneElse,
    BelongsTo::GivenAway,
];

/// How a kind reads in a select and a row.
pub(in crate::http::pages) fn kind_label(kind: AccountKind) -> &'static str {
    match kind {
        AccountKind::Checking => "Checking",
        AccountKind::Savings => "Savings",
        AccountKind::CreditCard => "Credit card",
        AccountKind::Loan => "Loan",
        AccountKind::Mortgage => "Mortgage",
        AccountKind::Brokerage => "Brokerage",
        AccountKind::Retirement => "Retirement",
        AccountKind::Education => "Education savings (529)",
        AccountKind::Custodial => "Custodial (UTMA, UGMA)",
        AccountKind::Charitable => "Charitable (donor-advised fund)",
        AccountKind::EquityComp => "Equity compensation",
        AccountKind::Property => "Property",
        AccountKind::Vehicle => "Vehicle",
        AccountKind::OtherAsset => "Other asset",
        AccountKind::OtherLiability => "Other liability",
    }
}

/// How an answer to "Whose money is it?" reads.
pub(in crate::http::pages) fn answer_label(answer: BelongsTo) -> &'static str {
    match answer {
        BelongsTo::Household => "The household's",
        BelongsTo::SomeoneElse => "Someone else's, held for them",
        BelongsTo::GivenAway => "Given away",
    }
}

/// The answers every form offers. What its empty answer means differs, so each form adds that.
pub(in crate::http::pages) fn answer_options() -> Markup {
    html! {
        @for answer in ANSWERS {
            option value=(answer.as_str()) { (answer_label(answer)) }
        }
    }
}

/// A form's answer: `""` asks nothing.
pub(in crate::http::pages) fn parse_belongs_to(
    raw: &str,
) -> Result<Option<BelongsTo>, AccountError> {
    match raw {
        "" => Ok(None),
        raw => raw.parse().map(Some),
    }
}

/// A form's kind: `""` asks nothing.
pub(in crate::http::pages) fn parse_kind(raw: &str) -> Result<Option<AccountKind>, AccountError> {
    match raw {
        "" => Ok(None),
        raw => raw.parse().map(Some),
    }
}

/// The select of kinds, for the form of the row `row`, offering `options`. `guess` is the sentence
/// to put right under it when its choice is only a guess.
pub(in crate::http::pages) fn kind_field(
    row: impl Display,
    options: &Markup,
    guess: Option<&str>,
) -> Markup {
    let field = format!("kind-{row}");
    let hint = format!("kind-fine-{row}");
    let guess_hint = format!("kind-guess-{row}");
    let described_by = if guess.is_some() {
        format!("{guess_hint} {hint}")
    } else {
        hint.clone()
    };
    html! {
        div.field {
            label for=(field) { "Kind" }
            select .input #(field) name="kind" required aria-describedby=(described_by) { (options) }
            @if let Some(guess) = guess {
                span.fine #(guess_hint) { (guess) }
            }
            span.fine #(hint) { (KIND_FINE) }
        }
    }
}

/// The field for the label, for the form of the row `row`. The hint is the input's description
/// and stays out of its name, which a screen reader repeats.
pub(in crate::http::pages) fn held_for_field(
    row: impl Display,
    value: Option<&str>,
    fine: &str,
) -> Markup {
    let field = format!("held-for-{row}");
    let hint = format!("held-for-fine-{row}");
    html! {
        div.field {
            label for=(field) { "Held for (optional)" }
            input .input #(field) type="text" name="beneficiary" value=[value]
                maxlength="64" list="beneficiaries" aria-describedby=(hint);
            span.fine #(hint) { (fine) }
        }
    }
}

/// The labels of `accounts`, which is what the actor sees, once each and in a fixed order. The
/// forms' `Held for` offers them, so that one name stays one bucket.
pub(in crate::http::pages) fn beneficiaries(accounts: &[AccountView]) -> Markup {
    let labels: BTreeSet<&str> = accounts
        .iter()
        .filter_map(|account| account.beneficiary.as_deref())
        .collect();
    html! {
        datalist #beneficiaries {
            @for label in labels {
                option value=(label) {}
            }
        }
    }
}

/// What a row says of an account that isn't the household's.
pub(in crate::http::pages) fn whose_words(view: &AccountView) -> Option<String> {
    let label = view.beneficiary.as_deref();
    match (view.belongs_to, label) {
        (BelongsTo::Household, _) => None,
        (BelongsTo::SomeoneElse, Some(label)) => Some(format!("Held for {label}")),
        (BelongsTo::SomeoneElse, None) => Some("Held for someone else".to_owned()),
        (BelongsTo::GivenAway, Some(label)) => Some(format!("Given away: {label}")),
        (BelongsTo::GivenAway, None) => Some("Given away".to_owned()),
    }
}

/// The hidden fields a form changing whose money an account is carries: what the page showed
/// when it was drawn, so that a form older than the account changes nothing.
pub(in crate::http::pages) fn shown_fields(view: &AccountView) -> Markup {
    html! {
        input type="hidden" name="shown_kind" value=(view.kind.as_str());
        input type="hidden" name="shown_belongs_to" value=(view.belongs_to.as_str());
        input type="hidden" name="shown_chosen" value=(view.belongs_to_overridden);
        input type="hidden" name="shown_beneficiary"
            value=(view.beneficiary.as_deref().unwrap_or_default());
    }
}

/// What [`shown_fields`] sent back.
pub(in crate::http::pages) struct Shown<'a> {
    pub kind: &'a str,
    pub belongs_to: &'a str,
    pub chosen: &'a str,
    pub beneficiary: &'a str,
}

impl Shown<'_> {
    /// Whether the page this came from showed the account as it is now. Any answer sent from an
    /// older one was chosen by someone who hadn't seen what has happened since, and whether the
    /// answer was chosen decides what the form's words said a change of kind would do.
    pub(in crate::http::pages) fn matches(&self, current: &AccountView) -> bool {
        self.kind == current.kind.as_str()
            && self.belongs_to == current.belongs_to.as_str()
            && self.chosen == current.belongs_to_overridden.to_string()
            && self.beneficiary == current.beneficiary.as_deref().unwrap_or_default()
    }
}

/// The select of whose money `view` is, for the form of the row `row`. Its empty answer leaves
/// it, since a change can't return a chosen answer to following the kind, and its hint says what
/// each answer does before it happens: the page has no script to say it after a choice.
pub(in crate::http::pages) fn whose_field(row: impl Display, view: &AccountView) -> Markup {
    let answer = format!("whose-{row}");
    let hint = format!("whose-fine-{row}");
    let leave_it = if view.belongs_to_overridden {
        "Leave it as you chose"
    } else {
        "Leave it, following the kind"
    };
    html! {
        div.field {
            label for=(answer) { "Whose money is it?" }
            select .input #(answer) name="belongs_to" aria-describedby=(hint) {
                option value="" selected { (leave_it) }
                (answer_options())
            }
            span.fine #(hint) {
                (following(view)) " "
                @if !view.belongs_to_overridden {
                    (STOPS_FOLLOWING) " "
                }
                (moving(view)) " " (NOT_THE_HOUSEHOLD_S) " " (AUDITED)
            }
        }
    }
}

/// What a change of kind does to whose money `view` is.
fn following(view: &AccountView) -> &'static str {
    let named = view.beneficiary.is_some();
    match (view.belongs_to_overridden, view.belongs_to, named) {
        (true, ..) => {
            "You chose this, so it stays whatever the kind. It can't go back to following the kind."
        }
        (false, BelongsTo::Household, _) => {
            "You haven't chosen, so this follows the kind. An education, custodial or charitable \
             kind takes it out of net worth, and out of the forecast if it's cash or a debt due \
             soon."
        }
        (false, _, true) => {
            "You haven't chosen, so this follows the kind. An education or custodial kind makes it \
             held for someone else and charitable makes it given away: it stays out of net worth \
             and keeps the name. Any other kind makes it the household's and counts it in net \
             worth, and removes the name."
        }
        (false, _, false) => {
            "You haven't chosen, so this follows the kind. An education or custodial kind makes it \
             held for someone else and charitable makes it given away: it stays out of net worth. \
             Any other kind makes it the household's and counts it in net worth."
        }
    }
}

/// What another answer does to `view`.
fn moving(view: &AccountView) -> &'static str {
    match (view.belongs_to, view.beneficiary.is_some()) {
        (BelongsTo::Household, _) => {
            "Saying it's someone else's or given away takes it out of net worth, over its whole \
             history, and out of the forecast if it's cash or a debt due soon. If you say it's \
             the household's again later, it counts again, even where you had turned that off."
        }
        (_, true) => {
            "Saying it's the household's counts it in net worth, over its whole history, and \
             removes the name. The forecast counts it only if it's cash or a debt due soon."
        }
        (_, false) => {
            "Saying it's the household's counts it in net worth, over its whole history. The \
             forecast counts it only if it's cash or a debt due soon."
        }
    }
}

/// The hint under `Held for` on a form changing `view`.
pub(in crate::http::pages) fn held_for_fine(view: &AccountView) -> String {
    match (view.belongs_to, view.beneficiary.is_some()) {
        (BelongsTo::Household, _) => {
            "A name or a purpose, only for money that isn't the household's.".to_owned()
        }
        (_, true) => format!("{SEEN_BY} Empty it to remove the name."),
        (_, false) => SEEN_BY.to_owned(),
    }
}

/// A form's `Held for` asks a change only when it differs from what the page showed. The box
/// starts with the account's name, so sent back as it was it asks for nothing, and a move to the
/// household's removes the name as the form said.
pub(in crate::http::pages) fn asked_beneficiary(
    sent: Option<String>,
    shown: &str,
) -> Option<String> {
    sent.filter(|asked| asked.trim() != shown)
}

/// A refusal of the account service in a form's words: the service's speak to the API, which
/// names fields as its schema does. Any other refusal keeps its fixed words.
pub(in crate::http::pages) fn form_words(message: &'static str) -> &'static str {
    [
        (AccountError::InvalidName, NAME_INVALID),
        (AccountError::InvalidBeneficiary, HELD_FOR_INVALID),
        (
            AccountError::BeneficiaryOnTheHousehold,
            HELD_FOR_ON_HOUSEHOLD,
        ),
        (
            AccountError::CountedButNotTheHouseholds,
            COUNTED_BUT_NOT_THE_HOUSEHOLD_S,
        ),
    ]
    .into_iter()
    .find(|(err, _)| err.message() == message)
    .map_or(message, |(_, words)| words)
}

#[cfg(test)]
mod tests {
    use coffer_domain::accounts::AccountError;

    use super::form_words;

    #[test]
    fn the_service_s_refusals_read_in_the_form_s_words() {
        for err in [
            AccountError::InvalidName,
            AccountError::InvalidBeneficiary,
            AccountError::BeneficiaryOnTheHousehold,
            AccountError::CountedButNotTheHouseholds,
        ] {
            let words = form_words(err.message());
            assert_ne!(words, err.message(), "{err:?}");
            assert!(words.ends_with('.'));
            assert!(!words.contains('_'), "no wire name");
            assert!(!words.contains("beneficiary"), "the form says Held for");
        }
        assert_eq!(form_words("anything else"), "anything else");
    }
}
