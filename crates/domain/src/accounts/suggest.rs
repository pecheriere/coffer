//! A guess at an account's kind from the name its provider gave it (spec §8.11 B12). The name is
//! the provider's text, so this is only ever a guess for a member to confirm: nothing applies
//! it. It is computed at read: this reads no clock, logs nothing and stores nothing.

use super::AccountKind;

/// Terms, each as adjacent words, and the kind each suggests.
type Terms = [(&'static [&'static str], AccountKind)];

/// Terms that say what an account is, whatever else its name holds.
const SPECIFIC: &Terms = &[
    (&["529"], AccountKind::Education),
    (&["coverdell"], AccountKind::Education),
    (&["college", "savings"], AccountKind::Education),
    (&["education", "savings"], AccountKind::Education),
    (&["utma"], AccountKind::Custodial),
    (&["ugma"], AccountKind::Custodial),
    (&["custodial"], AccountKind::Custodial),
    (&["daf"], AccountKind::Charitable),
    (&["donor", "advised"], AccountKind::Charitable),
    (&["charitable"], AccountKind::Charitable),
    (&["ira"], AccountKind::Retirement),
    (&["roth"], AccountKind::Retirement),
    (&["401k"], AccountKind::Retirement),
    (&["401", "k"], AccountKind::Retirement),
    (&["403b"], AccountKind::Retirement),
    (&["403", "b"], AccountKind::Retirement),
    (&["457b"], AccountKind::Retirement),
    (&["457", "b"], AccountKind::Retirement),
    (&["thrift", "savings", "plan"], AccountKind::Retirement),
    (&["pension"], AccountKind::Retirement),
    (&["retirement"], AccountKind::Retirement),
    (&["rsu"], AccountKind::EquityComp),
    (&["espp"], AccountKind::EquityComp),
    (&["stock", "plan"], AccountKind::EquityComp),
    (&["mortgage"], AccountKind::Mortgage),
    (&["heloc"], AccountKind::Mortgage),
    (&["credit", "card"], AccountKind::CreditCard),
];

/// Terms that a specific one overrides: a `UTMA Brokerage` is custodial.
const GENERIC: &Terms = &[
    (&["checking"], AccountKind::Checking),
    (&["chequing"], AccountKind::Checking),
    (&["savings"], AccountKind::Savings),
    (&["hysa"], AccountKind::Savings),
    (&["loan"], AccountKind::Loan),
    (&["brokerage"], AccountKind::Brokerage),
    (&["visa"], AccountKind::CreditCard),
    (&["mastercard"], AccountKind::CreditCard),
    (&["amex"], AccountKind::CreditCard),
];

/// Terms that leave a name suggesting nothing, whatever else it holds: a health savings account
/// isn't cash and isn't any of the kinds, so Plaid's `hsa` is no hint either.
const NOTHING: &[&[&str]] = &[&["hsa"], &["h", "s", "a"], &["health", "savings"]];

/// The kind `name` suggests: the one kind of a specific term, else the one kind of a generic
/// term, else none. Two kinds at the same strength suggest nothing, a generic term doesn't
/// settle two specific ones, and `hsa`, `h s a` or `health savings` means nothing is suggested. Words are
/// maximal runs of ASCII letters and digits, so a term matches whole words only: `x5291` is not a
/// 529.
#[must_use]
pub fn suggest_kind(name: &str) -> Option<AccountKind> {
    let name = name.to_ascii_lowercase();
    let words: Vec<&str> = name
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();
    if NOTHING.iter().any(|term| holds(&words, term)) {
        return None;
    }
    match kinds_named(SPECIFIC, &words)[..] {
        [kind] => Some(kind),
        [] => match kinds_named(GENERIC, &words)[..] {
            [kind] => Some(kind),
            _ => None,
        },
        _ => None,
    }
}

/// The different kinds of the `terms` that `words` hold.
fn kinds_named(terms: &Terms, words: &[&str]) -> Vec<AccountKind> {
    let mut kinds = Vec::new();
    for (term, kind) in terms {
        if holds(words, term) && !kinds.contains(kind) {
            kinds.push(*kind);
        }
    }
    kinds
}

/// Whether `words` hold the words of `term`, adjacent and in order.
fn holds(words: &[&str], term: &[&str]) -> bool {
    (0..words.len()).any(|at| words[at..].starts_with(term))
}

#[cfg(test)]
mod tests {
    use proptest::prelude::{any, proptest};

    use super::suggest_kind;
    use crate::accounts::AccountKind::{
        self, Brokerage, Charitable, Checking, CreditCard, Custodial, Education, EquityComp, Loan,
        Mortgage, Retirement, Savings,
    };

    fn assert_suggests(cases: &[(&str, Option<AccountKind>)]) {
        for (name, expected) in cases {
            assert_eq!(suggest_kind(name), *expected, "{name:?}");
        }
    }

    #[test]
    fn suggestions() {
        assert_suggests(&[
            ("Everyday Checking", Some(Checking)),
            ("High Yield Savings", Some(Savings)),
            ("My 529 Plan", Some(Education)),
            ("College Savings 529", Some(Education)),
            ("College Savings", Some(Education)),
            ("UTMA Brokerage", Some(Custodial)),
            ("Roth IRA", Some(Retirement)),
            ("IRA Brokerage", Some(Retirement)),
            ("401(k)", Some(Retirement)),
            ("Donor-Advised Fund", Some(Charitable)),
            ("Visa Signature", Some(CreditCard)),
            ("Visa Checking", None),
            ("Education First Checking", Some(Checking)),
            ("Savings and Checking", None),
            ("Roth 529", None),
            ("Loan Savings", None),
            ("", None),
            ("  -- () / !! ", None),
            ("日本語の口座", None),
            ("Épargne", None),
            ("🏦", None),
        ]);
    }

    #[test]
    fn every_term_suggests_its_kind() {
        assert_suggests(&[
            ("529", Some(Education)),
            ("coverdell", Some(Education)),
            ("college savings", Some(Education)),
            ("education savings", Some(Education)),
            ("utma", Some(Custodial)),
            ("ugma", Some(Custodial)),
            ("custodial", Some(Custodial)),
            ("daf", Some(Charitable)),
            ("donor advised", Some(Charitable)),
            ("charitable", Some(Charitable)),
            ("ira", Some(Retirement)),
            ("roth", Some(Retirement)),
            ("401k", Some(Retirement)),
            ("401 k", Some(Retirement)),
            ("403b", Some(Retirement)),
            ("403 b", Some(Retirement)),
            ("457b", Some(Retirement)),
            ("457 b", Some(Retirement)),
            ("thrift savings plan", Some(Retirement)),
            ("pension", Some(Retirement)),
            ("retirement", Some(Retirement)),
            ("rsu", Some(EquityComp)),
            ("espp", Some(EquityComp)),
            ("stock plan", Some(EquityComp)),
            ("mortgage", Some(Mortgage)),
            ("heloc", Some(Mortgage)),
            ("credit card", Some(CreditCard)),
            ("checking", Some(Checking)),
            ("chequing", Some(Checking)),
            ("savings", Some(Savings)),
            ("hysa", Some(Savings)),
            ("loan", Some(Loan)),
            ("brokerage", Some(Brokerage)),
            ("visa", Some(CreditCard)),
            ("mastercard", Some(CreditCard)),
            ("amex", Some(CreditCard)),
        ]);
    }

    /// A specific term beats a generic one, and a generic one can't settle two specific ones.
    #[test]
    fn a_specific_kind_outranks_generic_ones_but_two_suggest_nothing() {
        assert_suggests(&[
            ("Mortgage Savings", Some(Mortgage)),
            ("Credit Card Checking", Some(CreditCard)),
            ("Visa Credit Card", Some(CreditCard)),
            ("Roth 529 Checking", None),
            ("Mortgage Credit Card", None),
            ("Roth IRA 401k Pension", Some(Retirement)),
            ("Checking Chequing", Some(Checking)),
        ]);
    }

    /// However a plan writes its number, and a specific term beats the `savings` in its name.
    #[test]
    fn retirement_plans_are_named_with_or_without_brackets() {
        assert_suggests(&[
            ("403(b)", Some(Retirement)),
            ("457(b)", Some(Retirement)),
            ("403(b) Savings", Some(Retirement)),
            ("457(b) Savings Plan", Some(Retirement)),
            ("Thrift Savings Plan", Some(Retirement)),
            ("Thrift Checking", Some(Checking)),
            ("Thrift Savings Account", Some(Savings)),
            ("First Thrift Savings", Some(Savings)),
            ("Thrift Plan Savings", Some(Savings)),
            ("403 Checking", Some(Checking)),
        ]);
    }

    /// An HSA isn't cash, and Plaid's `hsa` is no hint either: such a name suggests nothing,
    /// whatever else it holds.
    #[test]
    fn a_health_savings_account_suggests_nothing() {
        assert_suggests(&[
            ("HSA", None),
            ("hsa", None),
            ("Health Savings Account", None),
            ("HSA Checking", None),
            ("HSA Brokerage", None),
            ("Roth HSA", None),
            ("529 Health Savings", None),
            ("Health-Savings Credit Card", None),
            ("Health Savings", None),
            ("H.S.A. Checking", None),
            ("H S A", None),
            ("Roth h.s.a.", None),
            ("H.S. Checking", Some(Checking)),
            ("S.H.A. Checking", Some(Checking)),
            ("Health Checking", Some(Checking)),
            ("Health Plan Savings", Some(Savings)),
            ("Hsas Savings", Some(Savings)),
        ]);
    }

    /// A term of two words needs them next to each other, in order.
    #[test]
    fn the_words_of_a_term_are_adjacent() {
        assert_suggests(&[
            ("Credit Union Card", None),
            ("Card Credit", None),
            ("Donor Fund Advised", None),
            ("401 x k", None),
            ("College of Savings", Some(Savings)),
            ("Credit-Card", Some(CreditCard)),
            ("credit   card", Some(CreditCard)),
        ]);
    }

    #[test]
    fn digits_inside_a_longer_word_do_not_match() {
        assert_suggests(&[
            ("Checking x5291", Some(Checking)),
            ("Acct 15290", None),
            ("529x", None),
        ]);
    }

    #[test]
    fn a_term_inside_a_longer_word_does_not_match() {
        assert_suggests(&[
            ("Kira", None),
            ("Irate", None),
            ("Loans", None),
            ("Pensions", None),
            ("Divisa", None),
        ]);
    }

    /// Only ASCII folds: a character that lowercases or looks like a term's letter isn't one.
    #[test]
    fn non_ascii_letters_are_not_folded_into_a_term() {
        assert_suggests(&[
            ("\u{0130}RA", None),
            ("\u{017f}avings", None),
            ("401\u{212a}", None),
            ("ｉｒａ", None),
        ]);
    }

    /// Only ASCII letters and digits make a word, so any other character ends one.
    #[test]
    fn a_non_ascii_character_ends_a_word() {
        assert_suggests(&[
            ("529\u{e9}", Some(Education)),
            ("Plan\u{2014}529", Some(Education)),
        ]);
    }

    #[test]
    fn case_does_not_matter() {
        assert_suggests(&[
            ("EVERYDAY CHECKING", Some(Checking)),
            ("roth ira", Some(Retirement)),
            ("cReDiT cArD", Some(CreditCard)),
            ("401K", Some(Retirement)),
            ("VISA CHECKING", None),
        ]);
    }

    proptest! {
        #[test]
        fn suggest_never_panics(name in any::<String>()) {
            let _ = suggest_kind(&name);
        }

        #[test]
        fn a_suggestion_is_deterministic(name in "(?i)((roth|529|visa|checking|savings|credit|card) ?){0,6}|\\PC{0,40}") {
            proptest::prop_assert_eq!(suggest_kind(&name), suggest_kind(&name));
            proptest::prop_assert_eq!(suggest_kind(&name), suggest_kind(&name.to_ascii_uppercase()));
        }
    }
}
