//! How the plan page words a planned item: what kind of item it is and when it falls due.

use coffer_domain::plan::{IntervalUnit, PlanDirection, PlanSchedule, PlanSpending};

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
    use coffer_domain::plan::{IntervalUnit, PlanDirection, PlanSchedule, PlanSpending};
    use jiff::civil::date;

    use super::{schedule_words, type_word};

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
