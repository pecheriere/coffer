//! How the plan page words a planned item: what kind of item it is, its tier and when it
//! falls due.

use coffer_domain::plan::{IntervalUnit, PlanDirection, PlanSchedule, PlanSpending, PlanTier};

use crate::http::pages::amounts::day;

/// What kind of item it is, in a word or two.
pub(super) fn type_word(
    schedule: &PlanSchedule,
    direction: PlanDirection,
    spending: Option<PlanSpending>,
) -> &'static str {
    match (schedule, direction) {
        (PlanSchedule::Goal { .. }, _) => "Goal",
        (PlanSchedule::Recurring { .. }, PlanDirection::Inflow) => "Income",
        (PlanSchedule::Recurring { .. }, PlanDirection::Outflow) => match spending {
            Some(PlanSpending::Budget) => "Budget",
            Some(PlanSpending::Bill) | None => "Bill",
        },
        (PlanSchedule::OneOff { .. }, PlanDirection::Outflow) => "One-off",
        (PlanSchedule::OneOff { .. }, PlanDirection::Inflow) => "One-off income",
    }
}

/// A tier's name, which heads its group and labels its choice.
pub(super) fn tier_word(tier: PlanTier) -> &'static str {
    match tier {
        PlanTier::MustPay => "Must pay",
        PlanTier::Essentials => "Essentials",
        PlanTier::Flexible => "Flexible",
        PlanTier::SavingToward => "Saving toward",
    }
}

/// What a tier's money may cover, in one line under its choice.
pub(super) fn tier_hint(tier: PlanTier) -> &'static str {
    match tier {
        PlanTier::MustPay => "Never used to cover other spending.",
        PlanTier::Essentials => "Can cover other essentials.",
        PlanTier::Flexible => "Can cover essentials and other flexible spending.",
        PlanTier::SavingToward => "Used only when you choose.",
    }
}

/// The stylesheet class that colours a tier's mark.
pub(super) fn tier_class(tier: PlanTier) -> &'static str {
    match tier {
        PlanTier::MustPay => "tier-must-pay",
        PlanTier::Essentials => "tier-essentials",
        PlanTier::Flexible => "tier-flexible",
        PlanTier::SavingToward => "tier-saving",
    }
}

/// Which way the money goes, after its amount.
pub(super) fn direction_word(direction: PlanDirection) -> &'static str {
    match direction {
        PlanDirection::Outflow => "out",
        PlanDirection::Inflow => "in",
    }
}

/// A unit's name, singular or plural.
pub(super) fn unit_word(unit: IntervalUnit, plural: bool) -> &'static str {
    match (unit, plural) {
        (IntervalUnit::Days, false) => "day",
        (IntervalUnit::Days, true) => "days",
        (IntervalUnit::Weeks, false) => "week",
        (IntervalUnit::Weeks, true) => "weeks",
        (IntervalUnit::Months, false) => "month",
        (IntervalUnit::Months, true) => "months",
        (IntervalUnit::Years, false) => "year",
        (IntervalUnit::Years, true) => "years",
    }
}

/// When it falls due, in words.
pub(super) fn schedule_words(schedule: &PlanSchedule) -> String {
    match *schedule {
        PlanSchedule::Recurring {
            every,
            unit,
            anchor_on,
            end_on,
        } => {
            let every = if every == 1 {
                format!("Every {}", unit_word(unit, false))
            } else {
                format!("Every {every} {}", unit_word(unit, true))
            };
            let until = end_on.map_or_else(String::new, |end| format!(", until {}", day(end)));
            format!("{every} from {}{until}", day(anchor_on))
        }
        PlanSchedule::OneOff { on } => format!("On {}", day(on)),
        PlanSchedule::Goal {
            start_on,
            due_on,
            completed,
        } => {
            let done = if completed { ", completed" } else { "" };
            format!(
                "Set aside from {}, due {}{done}",
                day(start_on),
                day(due_on)
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use coffer_domain::plan::{IntervalUnit, PlanDirection, PlanSchedule, PlanSpending, PlanTier};
    use jiff::civil::date;

    use super::{schedule_words, tier_class, tier_hint, tier_word, type_word};
    use crate::assets::CSS_BASE_CSS;

    const TIERS: [PlanTier; 4] = [
        PlanTier::MustPay,
        PlanTier::Essentials,
        PlanTier::Flexible,
        PlanTier::SavingToward,
    ];

    #[test]
    fn each_tier_has_its_own_word_hint_and_class() {
        let words: Vec<_> = TIERS.map(tier_word).into();
        assert_eq!(
            words,
            ["Must pay", "Essentials", "Flexible", "Saving toward"]
        );
        let mut hints: Vec<_> = TIERS.map(tier_hint).into();
        hints.dedup();
        assert_eq!(hints.len(), TIERS.len(), "one hint per tier");
        assert_eq!(
            tier_hint(PlanTier::MustPay),
            "Never used to cover other spending."
        );
        let css = std::str::from_utf8(CSS_BASE_CSS.body).expect("UTF-8 stylesheet");
        for tier in TIERS {
            let class = tier_class(tier);
            assert!(css.contains(&format!(".{class} {{")), "{class} is styled");
        }
    }

    #[test]
    fn a_schedule_reads_as_a_sentence_of_days() {
        let monthly = PlanSchedule::Recurring {
            every: 1,
            unit: IntervalUnit::Months,
            anchor_on: date(2026, 11, 1),
            end_on: None,
        };
        assert_eq!(schedule_words(&monthly), "Every month from 1 November 2026");
        let fortnightly = PlanSchedule::Recurring {
            every: 2,
            unit: IntervalUnit::Weeks,
            anchor_on: date(2026, 11, 6),
            end_on: Some(date(2027, 6, 30)),
        };
        assert_eq!(
            schedule_words(&fortnightly),
            "Every 2 weeks from 6 November 2026, until 30 June 2027"
        );
        let once = PlanSchedule::OneOff {
            on: date(2026, 12, 3),
        };
        assert_eq!(schedule_words(&once), "On 3 December 2026");
        let goal = PlanSchedule::Goal {
            start_on: date(2026, 11, 1),
            due_on: date(2027, 6, 1),
            completed: true,
        };
        assert_eq!(
            schedule_words(&goal),
            "Set aside from 1 November 2026, due 1 June 2027, completed"
        );
    }

    #[test]
    fn a_recurring_outflow_is_a_bill_unless_it_is_a_budget() {
        let recurring = PlanSchedule::Recurring {
            every: 1,
            unit: IntervalUnit::Months,
            anchor_on: date(2026, 11, 1),
            end_on: None,
        };
        let out = PlanDirection::Outflow;
        assert_eq!(type_word(&recurring, out, Some(PlanSpending::Bill)), "Bill");
        assert_eq!(
            type_word(&recurring, out, Some(PlanSpending::Budget)),
            "Budget"
        );
        assert_eq!(type_word(&recurring, PlanDirection::Inflow, None), "Income");
    }
}
