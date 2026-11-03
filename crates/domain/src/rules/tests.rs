use super::*;

fn usd() -> Currency {
    Currency::parse("USD").unwrap()
}

fn text(kind: RuleMatchKind, pattern: &str) -> RuleText {
    RuleText::new(kind, RulePattern::parse(pattern).unwrap())
}

fn category(kind: CategoryKind) -> RuleCategory {
    RuleCategory {
        id: CategoryId::new(),
        kind,
        archived: false,
    }
}

fn rule(pattern: &str) -> Rule {
    Rule {
        id: RuleId::new(),
        text: text(RuleMatchKind::Contains, pattern),
        account: None,
        amount: None,
        category: Some(category(CategoryKind::Expense)),
        assignee: None,
    }
}

fn subject(description: &str) -> RuleSubject<'_> {
    RuleSubject {
        account: AccountId::new(),
        amount_minor: -1200,
        currency: usd(),
        description,
        category_open: true,
        assignee_open: true,
        splits: 0,
        reviewed: false,
        transfer_leg: false,
    }
}

#[test]
fn contains_and_starts_with_ignore_case_beyond_ascii() {
    let contains = text(RuleMatchKind::Contains, "Café");
    assert!(contains.found_in(&folded("LE CAFÉ DU COIN")));
    assert!(!contains.found_in(&folded("LE CAFE DU COIN")));
    let starts = text(RuleMatchKind::StartsWith, "corner");
    assert!(starts.found_in(&folded("CORNER SHOP")));
    assert!(!starts.found_in(&folded("THE CORNER SHOP")));
}

#[test]
fn a_final_sigma_folds_like_any_other() {
    let contains = text(RuleMatchKind::Contains, "ΟΔΟΣ");
    assert!(contains.found_in(&folded("ΟΔΟΣΑ 12")));
}

#[test]
fn a_pattern_is_plain_text_not_a_wildcard() {
    for pattern in [".*", "%", "_", "(a+)+$", "\\d"] {
        let rule = rule(pattern);
        assert!(decide(std::slice::from_ref(&rule), &subject("PLAIN SHOP")).is_empty());
        let holding = format!("SHOP {pattern} 12");
        assert!(!decide(std::slice::from_ref(&rule), &subject(&holding)).is_empty());
    }
}

#[test]
fn the_first_match_wins_for_each_field_on_its_own() {
    let member = RuleAssignee {
        id: MemberId::new(),
        disabled: false,
    };
    let first = rule("SHOP");
    let second = Rule {
        assignee: Some(member),
        ..rule("SHOP")
    };
    let third = Rule {
        assignee: Some(RuleAssignee {
            id: MemberId::new(),
            disabled: false,
        }),
        ..rule("SHOP")
    };
    let fills = decide(
        &[first.clone(), second.clone(), third],
        &subject("CORNER SHOP"),
    );
    assert_eq!(fills.category.map(|(by, _)| by), Some(first.id));
    assert_eq!(fills.assignee, Some((second.id, member.id)));
}

#[test]
fn account_and_amount_narrow_a_rule() {
    let seen = subject("SHOP");
    let other_account = Rule {
        account: Some(AccountId::new()),
        ..rule("SHOP")
    };
    assert!(decide(&[other_account], &seen).is_empty());
    let own_account = Rule {
        account: Some(seen.account),
        ..rule("SHOP")
    };
    assert!(!decide(&[own_account], &seen).is_empty());

    let in_range = |min, max, currency| Rule {
        amount: Some(AmountRange::new(currency, min, max).unwrap()),
        ..rule("SHOP")
    };
    assert!(!decide(&[in_range(Some(-1200), Some(-1200), usd())], &seen).is_empty());
    assert!(!decide(&[in_range(None, Some(-1), usd())], &seen).is_empty());
    assert!(decide(&[in_range(Some(1), None, usd())], &seen).is_empty());
    assert!(
        decide(
            &[in_range(None, Some(-1), Currency::parse("JPY").unwrap())],
            &seen
        )
        .is_empty(),
        "the same number of another currency's minor units isn't this amount"
    );
}

#[test]
fn an_amount_range_needs_an_end_and_the_right_order() {
    assert_eq!(
        AmountRange::new(usd(), None, None),
        Err(RuleError::InvalidAmountRange)
    );
    assert_eq!(
        AmountRange::new(usd(), Some(2), Some(1)),
        Err(RuleError::InvalidAmountRange)
    );
    assert!(AmountRange::new(usd(), Some(1), Some(1)).is_ok());
}

#[test]
fn only_what_nobody_has_set_is_filled() {
    let member = RuleAssignee {
        id: MemberId::new(),
        disabled: false,
    };
    let both = Rule {
        assignee: Some(member),
        ..rule("SHOP")
    };
    let filed = RuleSubject {
        category_open: false,
        splits: 1,
        ..subject("SHOP")
    };
    let fills = decide(std::slice::from_ref(&both), &filed);
    assert_eq!(fills.category, None);
    assert_eq!(fills.assignee, Some((both.id, member.id)));
    let assigned = RuleSubject {
        assignee_open: false,
        ..subject("SHOP")
    };
    let fills = decide(std::slice::from_ref(&both), &assigned);
    assert!(fills.category.is_some());
    assert_eq!(fills.assignee, None);
    let cleared_by_a_person = RuleSubject {
        category_open: false,
        assignee_open: false,
        ..subject("SHOP")
    };
    assert!(decide(&[both], &cleared_by_a_person).is_empty());
}

#[test]
fn split_and_reviewed_transactions_are_left_alone_whole() {
    let both = Rule {
        assignee: Some(RuleAssignee {
            id: MemberId::new(),
            disabled: false,
        }),
        ..rule("SHOP")
    };
    let rules = std::slice::from_ref(&both);
    let split = RuleSubject {
        category_open: false,
        splits: 2,
        ..subject("SHOP")
    };
    assert!(decide(rules, &split).is_empty());
    assert_eq!(left_alone(rules, &both, &split), Some(LeftAlone::Split));
    let reviewed = RuleSubject {
        reviewed: true,
        ..subject("SHOP")
    };
    assert!(decide(rules, &reviewed).is_empty());
    assert_eq!(
        left_alone(rules, &both, &reviewed),
        Some(LeftAlone::Reviewed)
    );
}

#[test]
fn a_zero_amount_takes_no_category_but_may_be_assigned() {
    let member = RuleAssignee {
        id: MemberId::new(),
        disabled: false,
    };
    let filing = rule("SHOP");
    let zero = RuleSubject {
        amount_minor: 0,
        ..subject("SHOP")
    };
    let rules = std::slice::from_ref(&filing);
    assert!(decide(rules, &zero).is_empty());
    assert_eq!(
        left_alone(rules, &filing, &zero),
        Some(LeftAlone::ZeroAmount)
    );
    let both = Rule {
        assignee: Some(member),
        ..rule("SHOP")
    };
    let fills = decide(std::slice::from_ref(&both), &zero);
    assert_eq!(fills.category, None);
    assert_eq!(fills.assignee, Some((both.id, member.id)));
}

#[test]
fn a_transfer_leg_takes_only_a_transfer_category() {
    let leg = RuleSubject {
        transfer_leg: true,
        ..subject("SHOP")
    };
    let spending = rule("SHOP");
    let moving = Rule {
        category: Some(category(CategoryKind::Transfer)),
        ..rule("SHOP")
    };
    let rules = std::slice::from_ref(&spending);
    assert!(decide(rules, &leg).is_empty());
    assert_eq!(
        left_alone(rules, &spending, &leg),
        Some(LeftAlone::TransferLeg)
    );
    let income = Rule {
        category: Some(category(CategoryKind::Income)),
        ..rule("SHOP")
    };
    assert!(decide(&[income], &leg).is_empty());
    let fills = decide(&[spending, moving.clone()], &leg);
    assert_eq!(fills.category.map(|(by, _)| by), Some(moving.id));
}

#[test]
fn an_archived_category_or_a_disabled_member_passes_to_the_next_rule() {
    let archived = Rule {
        category: Some(RuleCategory {
            archived: true,
            ..category(CategoryKind::Expense)
        }),
        assignee: Some(RuleAssignee {
            id: MemberId::new(),
            disabled: true,
        }),
        ..rule("SHOP")
    };
    let member = RuleAssignee {
        id: MemberId::new(),
        disabled: false,
    };
    let next = Rule {
        assignee: Some(member),
        ..rule("SHOP")
    };
    let seen = subject("SHOP");
    assert!(decide(std::slice::from_ref(&archived), &seen).is_empty());
    assert_eq!(
        left_alone(std::slice::from_ref(&archived), &archived, &seen),
        None,
        "it has nothing to offer"
    );
    let fills = decide(&[archived, next.clone()], &seen);
    assert_eq!(fills.category.map(|(by, _)| by), Some(next.id));
    assert_eq!(fills.assignee, Some((next.id, member.id)));
}

#[test]
fn a_rule_that_loses_to_an_earlier_one_says_so() {
    let first = rule("SHOP");
    let second = rule("SHOP");
    let rules = [first.clone(), second.clone()];
    let seen = subject("SHOP");
    assert!(decide(&rules, &seen).by(second.id).is_empty());
    assert!(!decide(&rules, &seen).by(first.id).is_empty());
    assert_eq!(
        left_alone(&rules, &second, &seen),
        Some(LeftAlone::EarlierRule)
    );
    assert_eq!(left_alone(&rules, &first, &seen), None);
}

#[test]
fn a_long_hostile_description_is_only_text() {
    let description = format!("{}'; DROP TABLE rules; --", "a".repeat(476));
    let filing = rule("a".repeat(200).as_str());
    assert!(!decide(&[filing], &subject(&description)).is_empty());
    let missing = rule(&format!("{}b", "a".repeat(199)));
    assert!(decide(&[missing], &subject(&description)).is_empty());
}
