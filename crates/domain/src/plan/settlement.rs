//! Settling a planned item for one month (spec §8.9 R9a): a person says that its
//! occurrences in that month were paid some way the forecast can't see, or won't happen.
//! It never changes the item.

use jiff::civil::Date;

use super::{CalendarMonth, PlanDirection, PlanError, PlanSchedule, PlanSpending, validate_as_of};

/// The most settlements one item may hold, five years of a monthly bill. Replacing a
/// month's own settlement doesn't add one, and a month that has aged out of
/// [`SETTLEMENT_WINDOW_MONTHS`] can still be deleted, so an item at the limit can always
/// make room.
pub const MAX_ITEM_SETTLEMENTS: usize = 60;

/// How many calendar months from today's a settlement may be written for, either way.
pub const SETTLEMENT_WINDOW_MONTHS: i32 = 12;

/// Whether an item may be settled: a recurring bill or a one-off, going out. A budget is
/// reduced by spending and a goal has links, so neither is a payment to settle, and
/// income has no bill to mark paid.
#[must_use]
pub fn settlement_applies(
    direction: PlanDirection,
    schedule: &PlanSchedule,
    spending: Option<PlanSpending>,
) -> bool {
    direction == PlanDirection::Outflow
        && match schedule {
            PlanSchedule::OneOff { .. } => true,
            PlanSchedule::Recurring { .. } => spending != Some(PlanSpending::Budget),
            PlanSchedule::Goal { .. } => false,
        }
}

/// Checks a month to write a settlement for: inside the plan's years, and at most
/// [`SETTLEMENT_WINDOW_MONTHS`] from the month holding `today`.
///
/// # Errors
/// [`PlanError::DateOutOfRange`] outside the plan's years, and
/// [`PlanError::SettlementOutOfWindow`] too far from `today`.
pub fn validate_settlement_month(month: CalendarMonth, today: Date) -> Result<(), PlanError> {
    validate_as_of(month.first_day())?;
    let this_month = CalendarMonth::of(today).map_err(|_| PlanError::DateOutOfRange)?;
    if month.months_from(this_month).abs() > SETTLEMENT_WINDOW_MONTHS {
        return Err(PlanError::SettlementOutOfWindow);
    }
    Ok(())
}

/// The days of `month` an occurrence falls on, as far as `schedule` says.
#[must_use]
pub fn occurrences_in(schedule: &PlanSchedule, month: CalendarMonth) -> Vec<Date> {
    schedule.occurrences(month.first_day(), month.next().first_day())
}

#[cfg(test)]
mod tests {
    use jiff::civil::date;

    use super::{settlement_applies, validate_settlement_month};
    use crate::plan::{IntervalUnit, PlanDirection, PlanError, PlanSchedule, PlanSpending};

    fn recurring() -> PlanSchedule {
        PlanSchedule::Recurring {
            every: 1,
            unit: IntervalUnit::Months,
            anchor_on: date(2026, 1, 10),
            end_on: None,
        }
    }

    #[test]
    fn a_month_twelve_away_is_inside_and_thirteen_is_out() {
        let today = date(2026, 11, 15);
        for inside in ["2025-11", "2026-11", "2027-11"] {
            assert_eq!(
                validate_settlement_month(inside.parse().unwrap(), today),
                Ok(()),
                "inside the window"
            );
        }
        for outside in ["2025-10", "2027-12"] {
            assert_eq!(
                validate_settlement_month(outside.parse().unwrap(), today),
                Err(PlanError::SettlementOutOfWindow),
                "outside the window"
            );
        }
    }

    #[test]
    fn a_month_outside_the_plans_years_is_refused_whatever_today_is() {
        for (month, today) in [
            ("1969-12", date(1970, 1, 1)),
            ("2201-01", date(2200, 12, 31)),
        ] {
            assert_eq!(
                validate_settlement_month(month.parse().unwrap(), today),
                Err(PlanError::DateOutOfRange),
                "outside the plan's years"
            );
        }
    }

    #[test]
    fn only_a_bill_or_a_one_off_going_out_may_be_settled() {
        let out = PlanDirection::Outflow;
        let one_off = PlanSchedule::OneOff {
            on: date(2026, 11, 3),
        };
        let goal = PlanSchedule::Goal {
            start_on: date(2026, 1, 1),
            due_on: date(2027, 1, 1),
            completed: false,
        };
        let bill = Some(PlanSpending::Bill);
        let budget = Some(PlanSpending::Budget);
        assert!(settlement_applies(out, &recurring(), bill), "a bill");
        assert!(settlement_applies(out, &one_off, None), "a one-off");
        assert!(!settlement_applies(out, &recurring(), budget), "a budget");
        assert!(!settlement_applies(out, &goal, None), "a goal");
        let income = PlanDirection::Inflow;
        assert!(!settlement_applies(income, &recurring(), None), "income");
        assert!(!settlement_applies(income, &one_off, None), "a receipt");
    }
}
