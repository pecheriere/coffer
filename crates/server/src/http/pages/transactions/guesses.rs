//! The form that confirms at once the guesses rules made on the list's page: every transaction
//! shown that a rule filed or assigned, where nothing set on it is anyone else's choice (a change
//! clears the rule's name), that nobody has reviewed, and that the actor may change. It sends their `pick_<id>`
//! to the batch form's handler, which marks them reviewed through the call
//! `POST /api/v1/transactions/batch` makes and changes nothing else: a rule never reviews, the
//! person does here. Only the rows shown are sent, never a hidden page.

use coffer_domain::transactions::ReviewStatus;
use maud::{Markup, html};

use super::Names;
use crate::http::pages::layout::csrf_field;
use crate::ledger::transactions::{AttributionView, TransactionView};

/// The form, or nothing when no row shown holds a guess the actor may confirm. `back` is the
/// list's filters, as the batch form sends them.
pub(super) fn form(items: &[TransactionView], names: &Names, csrf: &str, back: &str) -> Markup {
    let guesses: Vec<&TransactionView> = items
        .iter()
        .filter(|transaction| is_guess(transaction) && names.may_write(transaction.account))
        .collect();
    if guesses.is_empty() {
        return html! {};
    }
    html! {
        form #confirm-guesses method="post" action="/transactions/batch" {
            (csrf_field(csrf))
            input type="hidden" name="back" value=(back);
            input type="hidden" name="review" value="reviewed";
            @for transaction in &guesses {
                input type="hidden" name={ "pick_" (transaction.id) } value="yes";
            }
            p.fine {
                "Confirming marks them reviewed and changes nothing else about them. "
                "Only the transactions on this page are confirmed."
            }
            button.btn type="submit" { (button_words(guesses.len())) }
        }
    }
}

/// Whether nobody has reviewed a transaction and what is set on it is only rules' choice: a rule
/// filed or assigned it, and whatever else is set a rule set too. A filing or assignee anyone
/// else chose makes it not a guess, so confirming never reviews a person's change with it.
fn is_guess(transaction: &TransactionView) -> bool {
    let set = [
        transaction.categorized_by.as_ref(),
        transaction.assigned_by.as_ref(),
    ];
    let by_rule = |attribution: &AttributionView| attribution.rule.is_some();
    transaction.review == ReviewStatus::Unreviewed
        && set.iter().flatten().any(|attribution| by_rule(attribution))
        && set.iter().flatten().all(|attribution| by_rule(attribution))
}

fn button_words(count: usize) -> String {
    if count == 1 {
        "Confirm the guess rules made on this page".to_owned()
    } else {
        format!("Confirm the {count} guesses rules made on this page")
    }
}

#[cfg(test)]
mod tests {
    use coffer_domain::ids::{AccountId, RuleId};
    use coffer_domain::transactions::ReviewStatus;

    use super::form;
    use crate::http::pages::transactions::fixtures::{by_member, names, transaction};
    use crate::ledger::transactions::{AttributionView, TransactionView};

    /// What a rule chose, run by hand by a member.
    fn by_rule() -> AttributionView {
        AttributionView {
            rule: Some(RuleId::new()),
            ..by_member()
        }
    }

    fn unreviewed(account: AccountId) -> TransactionView {
        TransactionView {
            review: ReviewStatus::Unreviewed,
            ..transaction(account, -1_250)
        }
    }

    fn render(items: &[TransactionView], writable: &[AccountId]) -> String {
        let readable: Vec<AccountId> = items.iter().map(|item| item.account).collect();
        form(
            items,
            &names(&readable, writable),
            "token",
            "review=unreviewed",
        )
        .into_string()
    }

    #[test]
    fn a_guess_is_a_rules_filing_or_assignee_nobody_changed_or_reviewed() {
        let checking = AccountId::new();
        let filed = TransactionView {
            categorized_by: Some(by_rule()),
            ..unreviewed(checking)
        };
        let assigned = TransactionView {
            assigned_by: Some(by_rule()),
            ..unreviewed(checking)
        };
        let by_a_person = TransactionView {
            categorized_by: Some(by_member()),
            ..unreviewed(checking)
        };
        let reviewed = TransactionView {
            review: ReviewStatus::Reviewed,
            categorized_by: Some(by_rule()),
            ..transaction(checking, -500)
        };
        let untouched = unreviewed(checking);
        let both_by_rules = TransactionView {
            categorized_by: Some(by_rule()),
            assigned_by: Some(by_rule()),
            ..unreviewed(checking)
        };
        let refiled_by_a_person = TransactionView {
            categorized_by: Some(by_member()),
            assigned_by: Some(by_rule()),
            ..unreviewed(checking)
        };
        let reassigned_by_a_person = TransactionView {
            categorized_by: Some(by_rule()),
            assigned_by: Some(by_member()),
            ..unreviewed(checking)
        };

        let page = render(
            &[
                filed.clone(),
                assigned.clone(),
                both_by_rules.clone(),
                by_a_person.clone(),
                refiled_by_a_person.clone(),
                reassigned_by_a_person.clone(),
                reviewed.clone(),
                untouched.clone(),
            ],
            &[checking],
        );

        assert!(page.contains("Confirm the 3 guesses rules made on this page"));
        for guess in [&filed, &assigned, &both_by_rules] {
            assert!(page.contains(&format!("name=\"pick_{}\"", guess.id)));
        }
        for left in [
            &by_a_person,
            &refiled_by_a_person,
            &reassigned_by_a_person,
            &reviewed,
            &untouched,
        ] {
            assert!(!page.contains(&left.id.to_string()));
        }
        assert!(page.contains("name=\"review\" value=\"reviewed\""));
        assert!(page.contains("name=\"back\" value=\"review=unreviewed\""));
        assert!(page.contains("name=\"csrf\""));
    }

    #[test]
    fn one_guess_is_named_in_the_singular() {
        let checking = AccountId::new();
        let filed = TransactionView {
            categorized_by: Some(by_rule()),
            ..unreviewed(checking)
        };
        let page = render(&[filed], &[checking]);
        assert!(page.contains(">Confirm the guess rules made on this page<"));
    }

    #[test]
    fn only_guesses_the_actor_may_change_are_sent() {
        let (mine, shared) = (AccountId::new(), AccountId::new());
        let writable = TransactionView {
            categorized_by: Some(by_rule()),
            ..unreviewed(mine)
        };
        let read_only = TransactionView {
            categorized_by: Some(by_rule()),
            ..unreviewed(shared)
        };

        let page = render(&[writable.clone(), read_only.clone()], &[mine]);
        assert!(page.contains("Confirm the guess rules made on this page"));
        assert!(page.contains(&writable.id.to_string()));
        assert!(!page.contains(&read_only.id.to_string()));
    }

    #[test]
    fn a_viewer_who_changes_nothing_or_a_page_without_guesses_gets_no_form() {
        let checking = AccountId::new();
        let filed = TransactionView {
            categorized_by: Some(by_rule()),
            ..unreviewed(checking)
        };
        assert_eq!(render(&[filed], &[]), "");
        assert_eq!(render(&[unreviewed(checking)], &[checking]), "");
        assert_eq!(render(&[], &[checking]), "");
    }
}
