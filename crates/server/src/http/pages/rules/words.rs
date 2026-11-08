//! What the rules page says of a rule, of what trying or running it would change, and of what
//! running it or taking that back did. Every `match` here is exhaustive, so a new reason to
//! leave a transaction alone fails the build until it has words.

use coffer_domain::ids::{CategoryId, MemberId};
use coffer_domain::rules::RuleMatchKind;

use super::Names;
use crate::http::pages::amounts::grouped;
use crate::ledger::rules::RuleAmount;
use crate::ledger::rules::run::{RuleApplied, RuleLeftAlone};
use crate::ledger::rules::undo::RuleUndone;

pub(super) fn match_kind_label(kind: RuleMatchKind) -> &'static str {
    match kind {
        RuleMatchKind::Contains => "contains",
        RuleMatchKind::StartsWith => "starts with",
    }
}

/// `contains “uber”`: what a rule looks for, quoted, so spaces at its ends show.
pub(super) fn match_words(kind: RuleMatchKind, pattern: &str) -> String {
    format!("{} \u{201c}{pattern}\u{201d}", match_kind_label(kind))
}

/// The amounts a rule is for: `amounts from -50.00 to -10.00 USD`.
pub(super) fn amount_words(amount: RuleAmount) -> String {
    let currency = amount.currency;
    let code = currency.as_str();
    match (amount.min_minor, amount.max_minor) {
        (Some(min), Some(max)) => format!(
            "amounts from {} to {} {code}",
            grouped(min, currency),
            grouped(max, currency)
        ),
        (Some(min), None) => format!("amounts of {} {code} or more", grouped(min, currency)),
        (None, Some(max)) => format!("amounts of {} {code} or less", grouped(max, currency)),
        (None, None) => format!("any amount in {code}"),
    }
}

/// What a rule fills, or would: `files under Groceries and assigns Bob`.
pub(super) fn fills_words(
    category: Option<CategoryId>,
    assignee: Option<MemberId>,
    names: &Names,
) -> String {
    match (category, assignee) {
        (Some(category), Some(assignee)) => format!(
            "files under {} and assigns {}",
            names.category(category),
            names.member(assignee)
        ),
        (Some(category), None) => format!("files under {}", names.category(category)),
        (None, Some(assignee)) => format!("assigns {}", names.member(assignee)),
        (None, None) => "fills nothing".to_owned(),
    }
}

/// `1 transaction`, `3 transactions`.
pub(super) fn transactions(count: u32) -> String {
    if count == 1 {
        "1 transaction".to_owned()
    } else {
        format!("{count} transactions")
    }
}

/// Why the transactions a rule matches, with something still open, stay as they are: one
/// sentence for each reason that counts any.
pub(super) fn left_alone_sentences(left: RuleLeftAlone) -> Vec<String> {
    let RuleLeftAlone {
        reviewed,
        split,
        transfer_leg,
        income_on_outflow,
        zero_amount,
        earlier_rule,
    } = left;
    [
        (
            reviewed,
            "someone has reviewed, and a rule never changes a reviewed transaction",
        ),
        (
            split,
            "with several splits, which only a person divides between categories",
        ),
        (
            transfer_leg,
            "on one side of a transfer, which a rule files only under a transfer category and only \
             when both sides are one movement of money",
        ),
        (
            income_on_outflow,
            "of money going out, which a rule doesn't file under an income category",
        ),
        (zero_amount, "of zero, which can't be filed"),
        (
            earlier_rule,
            "an earlier rule fills first: run that one instead",
        ),
    ]
    .into_iter()
    .filter(|(count, _)| *count > 0)
    .map(|(count, why)| format!("{} {why}.", transactions(count)))
    .collect()
}

/// What running a rule by hand did, said of `names`' category and member.
pub(super) fn applied_sentences(
    done: RuleApplied,
    category: Option<CategoryId>,
    assignee: Option<MemberId>,
    names: &Names,
) -> Vec<String> {
    if done.changed == 0 {
        return vec![
            "It changed nothing: what it matches was already filled, or is left alone for a \
             reason below."
                .to_owned(),
        ];
    }
    let mut said = vec![format!("It changed {}.", transactions(done.changed))];
    if done.categorized > 0 {
        let under = category.map_or("its category", |id| names.category(id));
        said.push(format!(
            "It filed {} under {under}.",
            transactions(done.categorized)
        ));
    }
    if done.assigned > 0 {
        let to = assignee.map_or("its member", |id| names.member(id));
        said.push(format!(
            "It assigned {} to {to}.",
            transactions(done.assigned)
        ));
    }
    said.push("Nothing was marked reviewed.".to_owned());
    said
}

/// What taking back a rule's work did.
pub(super) fn undone_sentences(done: RuleUndone) -> Vec<String> {
    let mut said = Vec::new();
    if done.changed == 0 {
        said.push("There was nothing it filled left to take back.".to_owned());
    } else {
        said.push(format!(
            "It emptied {} and opened them again for a rule to fill.",
            transactions(done.changed)
        ));
        if done.categorized > 0 {
            said.push(format!(
                "{} lost the category it filed.",
                transactions(done.categorized)
            ));
        }
        if done.assigned > 0 {
            said.push(format!(
                "{} lost the member it assigned.",
                transactions(done.assigned)
            ));
        }
    }
    if done.reviewed > 0 {
        said.push(format!(
            "It left {} someone has reviewed: change those by hand if they are wrong.",
            transactions(done.reviewed)
        ));
    }
    said
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use coffer_domain::money::Currency;
    use coffer_domain::rules::RuleMatchKind;

    use super::{amount_words, left_alone_sentences, match_words, transactions, undone_sentences};
    use crate::http::pages::word_checks::assert_worded;
    use crate::ledger::rules::RuleAmount;
    use crate::ledger::rules::run::RuleLeftAlone;
    use crate::ledger::rules::undo::RuleUndone;

    #[test]
    fn every_reason_to_leave_one_alone_has_a_sentence() {
        let left = RuleLeftAlone {
            reviewed: 1,
            split: 2,
            transfer_leg: 3,
            income_on_outflow: 4,
            zero_amount: 5,
            earlier_rule: 6,
        };
        let sentences = left_alone_sentences(left);
        assert_eq!(sentences.len(), 6, "one sentence for each reason");
        for sentence in &sentences {
            assert_worded(sentence, &BTreeSet::new());
        }
        assert_eq!(
            sentences[0],
            format!(
                "{} someone has reviewed, and a rule never changes a reviewed transaction.",
                transactions(1)
            )
        );
    }

    #[test]
    fn a_reason_that_counts_none_says_nothing() {
        assert_eq!(
            left_alone_sentences(RuleLeftAlone::default()),
            Vec::<String>::new()
        );
        let only = RuleLeftAlone {
            zero_amount: 1,
            ..RuleLeftAlone::default()
        };
        assert_eq!(
            left_alone_sentences(only),
            ["1 transaction of zero, which can't be filed."]
        );
    }

    #[test]
    fn counts_agree_with_their_noun() {
        assert_eq!(transactions(0), "0 transactions");
        assert_eq!(transactions(1), "1 transaction");
        assert_eq!(transactions(2), "2 transactions");
    }

    #[test]
    fn a_pattern_shows_its_spaces_inside_quotes() {
        assert_eq!(
            match_words(RuleMatchKind::StartsWith, "TAX "),
            "starts with \u{201c}TAX \u{201d}"
        );
    }

    #[test]
    fn an_amount_range_reads_with_either_end_open() {
        let usd = Currency::parse("USD").unwrap();
        let range = |min_minor, max_minor| RuleAmount {
            currency: usd,
            min_minor,
            max_minor,
        };
        assert_eq!(
            amount_words(range(Some(-5_000), Some(-1_000))),
            "amounts from -50.00 to -10.00 USD"
        );
        assert_eq!(
            amount_words(range(Some(-123_456), None)),
            "amounts of -1,234.56 USD or more"
        );
        assert_eq!(
            amount_words(range(None, Some(0))),
            "amounts of 0.00 USD or less"
        );
    }

    #[test]
    fn taking_back_nothing_says_so_and_names_what_it_left() {
        let nothing = undone_sentences(RuleUndone {
            reviewed: 2,
            ..RuleUndone::default()
        });
        assert_eq!(
            nothing,
            [
                "There was nothing it filled left to take back.",
                "It left 2 transactions someone has reviewed: change those by hand if they are wrong.",
            ]
        );
        let some = undone_sentences(RuleUndone {
            changed: 3,
            categorized: 3,
            assigned: 1,
            reviewed: 0,
        });
        for sentence in &some {
            assert_worded(sentence, &BTreeSet::new());
        }
        assert_eq!(some.len(), 3);
    }
}
