use proptest::prelude::{Just, Strategy, any, prop_assert, prop_assert_eq, prop_oneof, proptest};

use super::{Belonging, BelongingAsked, BeneficiaryChange, resolve_belonging};
use crate::accounts::{AccountError, AccountKind, BelongsTo, Beneficiary};

const NOTHING: BelongingAsked = BelongingAsked {
    belongs_to: None,
    beneficiary: BeneficiaryChange::Keep,
    include_in_net_worth: None,
    include_in_forecast: None,
};

fn label(text: &str) -> Beneficiary {
    Beneficiary::parse(text).unwrap()
}

/// A household account whose owner left the flags on and its `belongs_to` to the kind.
fn household() -> Belonging {
    Belonging {
        belongs_to: BelongsTo::Household,
        overridden: false,
        beneficiary: None,
        include_in_net_worth: true,
        include_in_forecast: true,
    }
}

/// An account held for a child because its kind says so.
fn held(belongs_to: BelongsTo, beneficiary: Option<&str>) -> Belonging {
    Belonging {
        belongs_to,
        overridden: false,
        beneficiary: beneficiary.map(label),
        include_in_net_worth: false,
        include_in_forecast: false,
    }
}

fn flags(belonging: &Belonging) -> (bool, bool) {
    (
        belonging.include_in_net_worth,
        belonging.include_in_forecast,
    )
}

#[test]
fn a_new_account_follows_its_kind() {
    for (kind, belongs_to) in [
        (AccountKind::Education, BelongsTo::SomeoneElse),
        (AccountKind::Custodial, BelongsTo::SomeoneElse),
        (AccountKind::Charitable, BelongsTo::GivenAway),
        (AccountKind::Brokerage, BelongsTo::Household),
    ] {
        let new = resolve_belonging(None, kind, &NOTHING).unwrap();
        assert_eq!(new.belongs_to, belongs_to, "{kind:?}");
        assert!(!new.overridden, "{kind:?} took the kind's default");
    }
}

#[test]
fn each_kind_defaults_out_of_both_flags() {
    for kind in AccountKind::ALL {
        let new = resolve_belonging(None, kind, &NOTHING).unwrap();
        let counted = new.belongs_to == BelongsTo::Household;
        assert_eq!(flags(&new), (counted, counted), "{kind:?}");
    }
}

#[test]
fn asking_for_the_kind_s_own_default_still_counts_as_choosing() {
    let asked = BelongingAsked {
        belongs_to: Some(BelongsTo::Household),
        ..NOTHING
    };
    let chosen = resolve_belonging(None, AccountKind::Checking, &asked).unwrap();
    assert_eq!(chosen.belongs_to, BelongsTo::Household);
    assert!(chosen.overridden);
}

#[test]
fn asking_to_count_money_that_isn_t_the_household_s_is_refused() {
    let net_worth = BelongingAsked {
        include_in_net_worth: Some(true),
        ..NOTHING
    };
    let forecast = BelongingAsked {
        include_in_forecast: Some(true),
        ..NOTHING
    };
    for asked in [&net_worth, &forecast] {
        let refused = resolve_belonging(None, AccountKind::Education, asked);
        assert_eq!(refused, Err(AccountError::CountedButNotTheHouseholds));
    }
    let chosen = BelongingAsked {
        belongs_to: Some(BelongsTo::SomeoneElse),
        include_in_net_worth: Some(true),
        ..NOTHING
    };
    assert_eq!(
        resolve_belonging(Some(&household()), AccountKind::Checking, &chosen),
        Err(AccountError::CountedButNotTheHouseholds)
    );
    assert_eq!(
        AccountError::CountedButNotTheHouseholds.to_string(),
        "an account that isn't the household's can't be counted: set belongs_to to household first"
    );
}

#[test]
fn asking_not_to_count_it_is_fine() {
    let asked = BelongingAsked {
        include_in_net_worth: Some(false),
        include_in_forecast: Some(false),
        ..NOTHING
    };
    let new = resolve_belonging(None, AccountKind::Education, &asked).unwrap();
    assert_eq!(flags(&new), (false, false));
}

#[test]
fn a_chosen_household_stays_over_a_change_of_kind() {
    let current = Belonging {
        overridden: true,
        ..household()
    };
    let moved = resolve_belonging(Some(&current), AccountKind::Education, &NOTHING).unwrap();
    assert_eq!(moved.belongs_to, BelongsTo::Household);
    assert!(moved.overridden, "still chosen");
    assert_eq!(flags(&moved), (true, true), "and it counts");
}

#[test]
fn a_chosen_one_that_isn_t_the_household_s_stays_over_a_change_of_kind() {
    let current = Belonging {
        overridden: true,
        ..held(BelongsTo::SomeoneElse, None)
    };
    let moved = resolve_belonging(Some(&current), AccountKind::Checking, &NOTHING).unwrap();
    assert_eq!(moved.belongs_to, BelongsTo::SomeoneElse);
    assert_eq!(flags(&moved), (false, false));
}

#[test]
fn a_defaulted_one_follows_a_change_of_kind() {
    let other_asset = household();
    let education =
        resolve_belonging(Some(&other_asset), AccountKind::Education, &NOTHING).unwrap();
    assert_eq!(education.belongs_to, BelongsTo::SomeoneElse);
    assert!(!education.overridden, "it follows the kind, not a choice");
    assert_eq!(flags(&education), (false, false));

    let brokerage = resolve_belonging(Some(&education), AccountKind::Brokerage, &NOTHING).unwrap();
    assert_eq!(brokerage.belongs_to, BelongsTo::Household);
    assert!(!brokerage.overridden);
    assert_eq!(flags(&brokerage), (true, true));
}

#[test]
fn moving_away_turns_both_off() {
    let asked = BelongingAsked {
        belongs_to: Some(BelongsTo::GivenAway),
        ..NOTHING
    };
    let moved = resolve_belonging(Some(&household()), AccountKind::Savings, &asked).unwrap();
    assert_eq!(moved.belongs_to, BelongsTo::GivenAway);
    assert_eq!(flags(&moved), (false, false));
}

#[test]
fn moving_back_turns_on_what_isn_t_named() {
    let current = held(BelongsTo::SomeoneElse, None);
    let plain = BelongingAsked {
        belongs_to: Some(BelongsTo::Household),
        ..NOTHING
    };
    let back = resolve_belonging(Some(&current), AccountKind::Education, &plain).unwrap();
    assert_eq!(flags(&back), (true, true));

    let one_off = BelongingAsked {
        include_in_forecast: Some(false),
        ..plain
    };
    let back = resolve_belonging(Some(&current), AccountKind::Education, &one_off).unwrap();
    assert_eq!(flags(&back), (true, false), "one named false stays off");
}

#[test]
fn a_household_account_keeps_its_flag_over_a_change_of_kind() {
    let current = Belonging {
        include_in_net_worth: false,
        ..household()
    };
    let savings = resolve_belonging(Some(&current), AccountKind::Savings, &NOTHING).unwrap();
    assert_eq!(flags(&savings), (false, true));

    let current = Belonging {
        include_in_forecast: false,
        ..household()
    };
    let savings = resolve_belonging(Some(&current), AccountKind::Savings, &NOTHING).unwrap();
    assert_eq!(flags(&savings), (true, false));
}

#[test]
fn a_flag_asked_on_a_household_account_is_what_is_asked() {
    let current = Belonging {
        include_in_net_worth: false,
        ..household()
    };
    let asked = BelongingAsked {
        include_in_net_worth: Some(true),
        include_in_forecast: Some(false),
        ..NOTHING
    };
    let changed = resolve_belonging(Some(&current), AccountKind::Checking, &asked).unwrap();
    assert_eq!(flags(&changed), (true, false));
}

#[test]
fn a_beneficiary_on_the_household_is_refused() {
    let asked = BelongingAsked {
        beneficiary: BeneficiaryChange::Set(label("Child A")),
        ..NOTHING
    };
    for current in [None, Some(&household())] {
        assert_eq!(
            resolve_belonging(current, AccountKind::Checking, &asked),
            Err(AccountError::BeneficiaryOnTheHousehold)
        );
    }
    assert_eq!(
        AccountError::BeneficiaryOnTheHousehold.to_string(),
        "only an account that isn't the household's has a beneficiary"
    );
}

#[test]
fn moving_back_clears_the_beneficiary() {
    let current = held(BelongsTo::SomeoneElse, Some("Child A"));
    let asked = BelongingAsked {
        belongs_to: Some(BelongsTo::Household),
        ..NOTHING
    };
    let back = resolve_belonging(Some(&current), AccountKind::Education, &asked).unwrap();
    assert_eq!(back.beneficiary, None);
}

#[test]
fn moving_between_the_two_keeps_the_label() {
    let current = held(BelongsTo::SomeoneElse, Some("Child A"));
    let asked = BelongingAsked {
        belongs_to: Some(BelongsTo::GivenAway),
        ..NOTHING
    };
    let moved = resolve_belonging(Some(&current), AccountKind::Education, &asked).unwrap();
    assert_eq!(moved.belongs_to, BelongsTo::GivenAway);
    assert_eq!(moved.beneficiary, Some(label("Child A")));
}

#[test]
fn a_label_is_set_cleared_and_kept_on_money_held_for_someone() {
    let current = held(BelongsTo::SomeoneElse, Some("Child A"));
    let change = |beneficiary| BelongingAsked {
        beneficiary,
        ..NOTHING
    };
    let resolve = |asked: &BelongingAsked| {
        resolve_belonging(Some(&current), AccountKind::Education, asked)
            .unwrap()
            .beneficiary
    };
    assert_eq!(resolve(&NOTHING), Some(label("Child A")));
    assert_eq!(resolve(&change(BeneficiaryChange::Clear)), None);
    assert_eq!(
        resolve(&change(BeneficiaryChange::Set(label("Child B")))),
        Some(label("Child B"))
    );
}

#[test]
fn what_a_request_asks_of_a_beneficiary() {
    assert_eq!(BeneficiaryChange::parse(None), Ok(BeneficiaryChange::Keep));
    for empty in ["", " ", "   "] {
        assert_eq!(
            BeneficiaryChange::parse(Some(empty)),
            Ok(BeneficiaryChange::Clear),
            "{empty:?}"
        );
    }
    assert_eq!(
        BeneficiaryChange::parse(Some("  Child A ")),
        Ok(BeneficiaryChange::Set(label("Child A")))
    );
    for bad in ["\u{200B}", "Child\u{0007}A", &"x".repeat(65)] {
        assert_eq!(
            BeneficiaryChange::parse(Some(bad)),
            Err(AccountError::InvalidBeneficiary),
            "{}",
            bad.escape_unicode()
        );
    }
}

#[test]
fn whitespace_other_than_a_plain_space_refuses_rather_than_clears() {
    for blank in [
        "\t",
        "\n",
        "\u{00A0}",
        "\u{2003}",
        "\u{2028}",
        "\u{3000}",
        " \u{00A0} ",
    ] {
        assert_eq!(
            BeneficiaryChange::parse(Some(blank)),
            Err(AccountError::InvalidBeneficiary),
            "{}",
            blank.escape_unicode()
        );
    }
}

fn any_belongs_to() -> impl Strategy<Value = BelongsTo> {
    prop_oneof![
        Just(BelongsTo::Household),
        Just(BelongsTo::SomeoneElse),
        Just(BelongsTo::GivenAway),
    ]
}

fn any_kind() -> impl Strategy<Value = AccountKind> {
    (0..AccountKind::ALL.len()).prop_map(|at| AccountKind::ALL[at])
}

fn any_label() -> impl Strategy<Value = Option<Beneficiary>> {
    prop_oneof![Just(None), Just(Some("Child A")), Just(Some("Family fund")),]
        .prop_map(|text| text.map(label))
}

fn any_current() -> impl Strategy<Value = Belonging> {
    (
        any_belongs_to(),
        any::<bool>(),
        any_label(),
        any::<bool>(),
        any::<bool>(),
    )
        .prop_map(
            |(belongs_to, overridden, beneficiary, include_in_net_worth, include_in_forecast)| {
                Belonging {
                    belongs_to,
                    overridden,
                    beneficiary,
                    include_in_net_worth,
                    include_in_forecast,
                }
            },
        )
}

fn any_asked() -> impl Strategy<Value = BelongingAsked> {
    (
        proptest::option::of(any_belongs_to()),
        prop_oneof![
            Just(BeneficiaryChange::Keep),
            Just(BeneficiaryChange::Clear),
            any_label()
                .prop_map(|text| text.map_or(BeneficiaryChange::Clear, BeneficiaryChange::Set)),
        ],
        proptest::option::of(any::<bool>()),
        proptest::option::of(any::<bool>()),
    )
        .prop_map(
            |(belongs_to, beneficiary, include_in_net_worth, include_in_forecast)| BelongingAsked {
                belongs_to,
                beneficiary,
                include_in_net_worth,
                include_in_forecast,
            },
        )
}

proptest! {
    /// What the rule answers is what `0022`'s CHECKs allow, and it is a fixed point.
    #[test]
    fn resolved_belonging_always_passes_the_table(
        current in proptest::option::of(any_current()),
        kind in any_kind(),
        asked in any_asked(),
    ) {
        if let Ok(resolved) = resolve_belonging(current.as_ref(), kind, &asked) {
            prop_assert!(
                resolved.belongs_to == BelongsTo::Household
                    || (!resolved.include_in_net_worth && !resolved.include_in_forecast)
            );
            prop_assert!(
                resolved.belongs_to != BelongsTo::Household || resolved.beneficiary.is_none()
            );
            let again = resolve_belonging(Some(&resolved), kind, &NOTHING);
            prop_assert_eq!(again, Ok(resolved));
        }
    }
}
