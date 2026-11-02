//! What Plaid's account `type` and `subtype` say an account is.

use coffer_domain::accounts::AccountKind;

/// What Plaid's `type` and `subtype` suggest the account is. Plaid has no subtype for a
/// donor-advised fund, so that stays to the account's name.
pub(super) fn kind_hint(kind: &str, subtype: Option<&str>) -> Option<AccountKind> {
    match (kind, subtype) {
        ("depository", Some("checking")) => Some(AccountKind::Checking),
        ("depository", Some("savings")) => Some(AccountKind::Savings),
        ("credit", _) => Some(AccountKind::CreditCard),
        ("loan", Some("mortgage")) => Some(AccountKind::Mortgage),
        ("loan", _) => Some(AccountKind::Loan),
        ("investment", Some("brokerage")) => Some(AccountKind::Brokerage),
        ("investment", Some("401k" | "403b" | "ira" | "roth" | "pension")) => {
            Some(AccountKind::Retirement)
        }
        ("investment", Some("529" | "education savings account")) => Some(AccountKind::Education),
        ("investment", Some("ugma" | "utma")) => Some(AccountKind::Custodial),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use coffer_domain::accounts::AccountKind;

    use super::kind_hint;

    #[test]
    fn kind_hints() {
        for (kind, subtype, expected) in [
            ("depository", Some("checking"), Some(AccountKind::Checking)),
            ("depository", Some("savings"), Some(AccountKind::Savings)),
            ("depository", Some("cd"), None),
            ("depository", None, None),
            ("credit", Some("credit card"), Some(AccountKind::CreditCard)),
            ("credit", None, Some(AccountKind::CreditCard)),
            ("loan", Some("mortgage"), Some(AccountKind::Mortgage)),
            ("loan", Some("student"), Some(AccountKind::Loan)),
            ("loan", None, Some(AccountKind::Loan)),
            (
                "investment",
                Some("brokerage"),
                Some(AccountKind::Brokerage),
            ),
            ("investment", Some("401k"), Some(AccountKind::Retirement)),
            ("investment", Some("403b"), Some(AccountKind::Retirement)),
            ("investment", Some("ira"), Some(AccountKind::Retirement)),
            ("investment", Some("roth"), Some(AccountKind::Retirement)),
            ("investment", Some("pension"), Some(AccountKind::Retirement)),
            ("investment", Some("529"), Some(AccountKind::Education)),
            (
                "investment",
                Some("education savings account"),
                Some(AccountKind::Education),
            ),
            ("investment", Some("utma"), Some(AccountKind::Custodial)),
            ("investment", Some("ugma"), Some(AccountKind::Custodial)),
            ("investment", Some("hsa"), None),
            ("depository", Some("529"), None),
            ("other", None, None),
        ] {
            assert_eq!(kind_hint(kind, subtype), expected, "{kind} {subtype:?}");
        }
    }
}
