//! Tiers (money redesign §4.1): which bucket a planned outflow's money is set in. Income
//! has none, a goal is always saving toward, and nothing else is.

use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::plan::{PlanDirection, PlanError, PlanSchedule, PlanSpending};
use crate::transactions::string_enum;

string_enum!(
    /// Which bucket a planned outflow's money is set in, deciding what may cover an
    /// overspend: never from must pay, and from saving toward only when a person chooses.
    PlanTier, PlanError::UnknownTier,
    {
        MustPay => "must_pay",
        Essentials => "essentials",
        Flexible => "flexible",
        SavingToward => "saving_toward",
    }
);

/// The tier an item takes when none is sent: none for income, saving toward for a goal,
/// flexible for a budget and must pay for a bill or a one-off outflow.
#[must_use]
pub fn default_tier(
    direction: PlanDirection,
    schedule: &PlanSchedule,
    spending: Option<PlanSpending>,
) -> Option<PlanTier> {
    if direction == PlanDirection::Inflow {
        return None;
    }
    Some(match (schedule, spending) {
        (PlanSchedule::Goal { .. }, _) => PlanTier::SavingToward,
        (PlanSchedule::Recurring { .. }, Some(PlanSpending::Budget)) => PlanTier::Flexible,
        _ => PlanTier::MustPay,
    })
}

/// The tier an item ends up with: `sent`, or the default for its kind when `sent` is `None`.
///
/// # Errors
/// [`PlanError::IncomeHasNoTier`] for a tier sent on income,
/// [`PlanError::GoalIsSavingToward`] for any other than saving toward on a goal, and
/// [`PlanError::OnlyGoalSavesToward`] for saving toward on anything else.
pub fn resolved_tier(
    sent: Option<PlanTier>,
    direction: PlanDirection,
    schedule: &PlanSchedule,
    spending: Option<PlanSpending>,
) -> Result<Option<PlanTier>, PlanError> {
    let Some(default) = default_tier(direction, schedule, spending) else {
        return match sent {
            Some(_) => Err(PlanError::IncomeHasNoTier),
            None => Ok(None),
        };
    };
    let Some(sent) = sent else {
        return Ok(Some(default));
    };
    let goal = matches!(schedule, PlanSchedule::Goal { .. });
    match (goal, sent == PlanTier::SavingToward) {
        (true, false) => Err(PlanError::GoalIsSavingToward),
        (false, true) => Err(PlanError::OnlyGoalSavesToward),
        _ => Ok(Some(sent)),
    }
}

#[cfg(test)]
mod tests {
    use jiff::civil::date;
    use proptest::prelude::{Just, Strategy};
    use proptest::prop_oneof;

    use super::{PlanTier, default_tier, resolved_tier};
    use crate::plan::{IntervalUnit, PlanDirection, PlanError, PlanSchedule, PlanSpending};

    const OUT: PlanDirection = PlanDirection::Outflow;
    const IN: PlanDirection = PlanDirection::Inflow;

    fn recurring() -> PlanSchedule {
        PlanSchedule::Recurring {
            every: 1,
            unit: IntervalUnit::Months,
            anchor_on: date(2027, 1, 1),
            end_on: None,
        }
    }

    fn one_off() -> PlanSchedule {
        PlanSchedule::OneOff {
            on: date(2027, 1, 1),
        }
    }

    fn goal() -> PlanSchedule {
        PlanSchedule::Goal {
            start_on: date(2027, 1, 1),
            due_on: date(2028, 1, 1),
            completed: false,
        }
    }

    const BILL: Option<PlanSpending> = Some(PlanSpending::Bill);
    const BUDGET: Option<PlanSpending> = Some(PlanSpending::Budget);

    #[test]
    fn defaults_by_kind() {
        for (direction, schedule, spending, expected, what) in [
            (OUT, recurring(), BILL, Some(PlanTier::MustPay), "a bill"),
            (
                OUT,
                recurring(),
                BUDGET,
                Some(PlanTier::Flexible),
                "a budget",
            ),
            (OUT, one_off(), None, Some(PlanTier::MustPay), "a one-off"),
            (OUT, goal(), None, Some(PlanTier::SavingToward), "a goal"),
            (IN, recurring(), None, None, "recurring income"),
            (IN, one_off(), None, None, "one-off income"),
        ] {
            assert_eq!(
                default_tier(direction, &schedule, spending),
                expected,
                "{what} takes its kind's default"
            );
            assert_eq!(
                resolved_tier(None, direction, &schedule, spending),
                Ok(expected),
                "{what} sent no tier resolves to the default"
            );
        }
    }

    #[test]
    fn rules_refuse_each_case() {
        assert_eq!(
            resolved_tier(Some(PlanTier::Flexible), IN, &recurring(), None),
            Err(PlanError::IncomeHasNoTier),
            "income has no tier"
        );
        assert_eq!(
            resolved_tier(Some(PlanTier::MustPay), OUT, &goal(), None),
            Err(PlanError::GoalIsSavingToward),
            "a goal is always saving toward"
        );
        assert_eq!(
            resolved_tier(Some(PlanTier::SavingToward), OUT, &recurring(), BILL),
            Err(PlanError::OnlyGoalSavesToward),
            "only a goal is saving toward"
        );
        assert_eq!(
            resolved_tier(Some(PlanTier::SavingToward), OUT, &one_off(), None),
            Err(PlanError::OnlyGoalSavesToward),
            "a one-off is not saving toward"
        );
        assert_eq!(
            resolved_tier(Some(PlanTier::SavingToward), OUT, &goal(), None),
            Ok(Some(PlanTier::SavingToward)),
            "a goal may say what it is"
        );
    }

    #[test]
    fn a_bill_may_be_any_spending_tier() {
        for tier in [PlanTier::MustPay, PlanTier::Essentials, PlanTier::Flexible] {
            for (schedule, spending) in [
                (recurring(), BILL),
                (recurring(), BUDGET),
                (one_off(), None),
            ] {
                assert_eq!(
                    resolved_tier(Some(tier), OUT, &schedule, spending),
                    Ok(Some(tier)),
                    "a spending tier is kept"
                );
            }
        }
    }

    #[test]
    fn texts_parse_and_unknown_ones_are_refused() {
        for (text, tier) in [
            ("must_pay", PlanTier::MustPay),
            ("essentials", PlanTier::Essentials),
            ("flexible", PlanTier::Flexible),
            ("saving_toward", PlanTier::SavingToward),
        ] {
            assert_eq!(text.parse(), Ok(tier), "known text");
            assert_eq!(tier.as_str(), text, "text roundtrips");
            assert_eq!(
                serde_json::to_string(&tier).unwrap(),
                format!("\"{text}\""),
                "plain text in JSON"
            );
        }
        assert_eq!(
            "emergency_fund".parse::<PlanTier>(),
            Err(PlanError::UnknownTier),
            "the emergency fund is not a tier"
        );
    }

    #[test]
    fn messages_are_fixed() {
        for (error, text) in [
            (PlanError::IncomeHasNoTier, "income has no tier"),
            (
                PlanError::GoalIsSavingToward,
                "a goal is always saving toward",
            ),
            (
                PlanError::OnlyGoalSavesToward,
                "only a goal is saving toward",
            ),
            (PlanError::UnknownTier, "unknown plan tier"),
        ] {
            assert_eq!(error.to_string(), text, "the message is fixed");
        }
    }

    /// Migration 0031's CHECK on `planned_items`, written in Rust: a tier is one of the
    /// four, held only by an outflow, and saving toward exactly when it is a goal.
    fn check_passes(
        direction: PlanDirection,
        schedule: &PlanSchedule,
        tier: Option<PlanTier>,
    ) -> bool {
        tier.is_none_or(|tier| {
            direction == PlanDirection::Outflow
                && (tier == PlanTier::SavingToward) == matches!(schedule, PlanSchedule::Goal { .. })
        })
    }

    fn any_tier() -> impl Strategy<Value = Option<PlanTier>> {
        prop_oneof![
            Just(None),
            Just(Some(PlanTier::MustPay)),
            Just(Some(PlanTier::Essentials)),
            Just(Some(PlanTier::Flexible)),
            Just(Some(PlanTier::SavingToward)),
        ]
    }

    fn any_direction() -> impl Strategy<Value = PlanDirection> {
        prop_oneof![Just(OUT), Just(IN)]
    }

    fn any_schedule() -> impl Strategy<Value = PlanSchedule> {
        prop_oneof![Just(recurring()), Just(one_off()), Just(goal())]
    }

    fn any_spending() -> impl Strategy<Value = Option<PlanSpending>> {
        prop_oneof![Just(None), Just(BILL), Just(BUDGET)]
    }

    proptest::proptest! {
        #[test]
        fn resolved_tier_always_passes_the_table(
            sent in any_tier(),
            direction in any_direction(),
            schedule in any_schedule(),
            spending in any_spending(),
        ) {
            if let Ok(tier) = resolved_tier(sent, direction, &schedule, spending) {
                proptest::prop_assert!(
                    check_passes(direction, &schedule, tier),
                    "a resolved tier passes the table's check"
                );
                proptest::prop_assert_eq!(
                    tier.is_some(),
                    direction == PlanDirection::Outflow,
                    "every outflow has a tier, and nothing else does"
                );
            }
        }
    }
}
