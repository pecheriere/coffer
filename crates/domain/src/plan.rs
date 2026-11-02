//! The spending plan (spec §6.7, §8.9): what a planned item is, when it falls
//! due and how a sinking fund accrues. Amounts here are positive magnitudes
//! with a [`PlanDirection`], unlike [`Money`] on accounts and splits.

use std::str::FromStr;

use jiff::civil::Date;
use serde::{Deserialize, Serialize};

use crate::identity::label_type;
use crate::money::Money;
use crate::transactions::string_enum;

mod goal;
mod month;
mod schedule;
mod settings;
pub mod strict_date;

pub use goal::{goal_accrued, goal_need, goal_remaining};
pub use month::{CalendarMonth, InvalidMonth};
pub use settings::{ForecastSettings, SafetyFloor};

/// The largest amount of a planned item or a fixed floor, in minor units.
pub const MAX_PLAN_AMOUNT_MINOR: i64 = 1_000_000_000_000_000;
/// The most planned items a household may store.
pub const MAX_PLAN_ITEMS: usize = 500;
/// The most hypothetical items one what-if may add.
pub const MAX_HYPOTHETICAL_ITEMS: usize = 50;
/// The shortest forecast horizon, in months.
pub const MIN_HORIZON_MONTHS: u8 = 1;
/// The longest forecast horizon, in months.
pub const MAX_HORIZON_MONTHS: u8 = 60;
/// The earliest year a schedule date or an `as_of` may fall in.
pub const MIN_PLAN_YEAR: i16 = 1970;
/// The latest year a schedule date or an `as_of` may fall in.
pub const MAX_PLAN_YEAR: i16 = 2200;

/// The largest `every` of a recurring schedule.
const MAX_EVERY: u16 = 999;

/// The most months of outflows a floor may hold.
const MAX_FLOOR_MONTHS: u8 = 60;
/// The longest a balance may be allowed to go unrefreshed, in days.
const MAX_STALE_AFTER_DAYS: u16 = 365;

const SECONDS_PER_DAY: i64 = 86_400;

/// Why a plan value was rejected. Messages never echo the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PlanError {
    #[error(
        "name must be 1 to {max} characters without control characters",
        max = PlannedItemName::MAX_CHARS
    )]
    InvalidName,
    #[error("amount must be between 1 and {MAX_PLAN_AMOUNT_MINOR} minor units")]
    InvalidAmount,
    #[error("every must be 1 to {MAX_EVERY}")]
    InvalidEvery,
    #[error("dates must fall in the years {MIN_PLAN_YEAR} to {MAX_PLAN_YEAR}")]
    DateOutOfRange,
    #[error("the end date cannot be before the anchor date")]
    EndBeforeAnchor,
    #[error("a goal's due date must be after its start date")]
    GoalDatesInverted,
    #[error("a goal must be an outflow")]
    GoalMustBeOutflow,
    #[error("a fixed floor must be between 0 and {MAX_PLAN_AMOUNT_MINOR} minor units")]
    InvalidFloor,
    #[error("the horizon must be {MIN_HORIZON_MONTHS} to {MAX_HORIZON_MONTHS} months")]
    InvalidHorizon,
    #[error("stale after must be 1 to {MAX_STALE_AFTER_DAYS} days")]
    InvalidStaleDays,
    #[error("floor months must be 1 to {MAX_FLOOR_MONTHS}")]
    InvalidFloorMonths,
    #[error("unknown plan direction")]
    UnknownDirection,
    #[error("unknown interval unit")]
    UnknownIntervalUnit,
    #[error("unknown plan item type")]
    UnknownItemType,
    #[error("unknown plan spending kind")]
    UnknownSpending,
}

string_enum!(
    /// Whether a planned item takes money out or brings it in.
    PlanDirection, PlanError::UnknownDirection,
    {
        Outflow => "outflow",
        Inflow => "inflow",
    }
);

string_enum!(
    /// The unit of a recurring schedule's interval.
    IntervalUnit, PlanError::UnknownIntervalUnit,
    {
        Days => "days",
        Weeks => "weeks",
        Months => "months",
        Years => "years",
    }
);

string_enum!(
    /// The kind of a planned item, which never changes once it exists.
    PlanItemType, PlanError::UnknownItemType,
    {
        Recurring => "recurring",
        OneOff => "one_off",
        Goal => "goal",
    }
);

string_enum!(
    /// How a recurring outflow relates to what was spent in its category so
    /// far this month: a bill counts in full, a budget is reduced by it.
    PlanSpending, PlanError::UnknownSpending,
    {
        Bill => "bill",
        Budget => "budget",
    }
);

label_type!(
    /// A planned item's name.
    PlannedItemName,
    PlanError::InvalidName,
    max = 100
);

/// When a planned item falls due.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum PlanSchedule {
    /// The k-th occurrence is `anchor_on + k × every unit`, always counted
    /// from the anchor, so a month-end anchor clamps without drifting.
    Recurring {
        /// How many `unit`s lie between occurrences: 1 to 999.
        #[cfg_attr(feature = "openapi", schema(minimum = 1, maximum = 999))]
        every: u16,
        unit: IntervalUnit,
        /// The first occurrence.
        #[serde(deserialize_with = "strict_date::deserialize")]
        anchor_on: Date,
        /// The last day an occurrence may fall on.
        #[serde(default, deserialize_with = "strict_date::deserialize_option")]
        end_on: Option<Date>,
    },
    /// A single date.
    OneOff {
        #[serde(deserialize_with = "strict_date::deserialize")]
        on: Date,
    },
    /// A sinking fund, set aside linearly from `start_on` to `due_on`.
    Goal {
        #[serde(deserialize_with = "strict_date::deserialize")]
        start_on: Date,
        #[serde(deserialize_with = "strict_date::deserialize")]
        due_on: Date,
        /// A completed goal needs no more money.
        #[serde(default)]
        completed: bool,
    },
}

impl PlanSchedule {
    /// The kind of planned item this schedule belongs to.
    #[must_use]
    pub fn item_type(&self) -> PlanItemType {
        match self {
            Self::Recurring { .. } => PlanItemType::Recurring,
            Self::OneOff { .. } => PlanItemType::OneOff,
            Self::Goal { .. } => PlanItemType::Goal,
        }
    }

    /// Checks the schedule of an item going in `direction`.
    ///
    /// # Errors
    /// Returns the first [`PlanError`] found: `every` outside 1 to 999, a date
    /// outside the plan's years, an end before the anchor, a goal that is not
    /// an outflow or whose due date is not after its start.
    pub fn validate(&self, direction: PlanDirection) -> Result<(), PlanError> {
        match *self {
            Self::Recurring {
                every,
                anchor_on,
                end_on,
                ..
            } => {
                if !(1..=MAX_EVERY).contains(&every) {
                    return Err(PlanError::InvalidEvery);
                }
                validate_as_of(anchor_on)?;
                if let Some(end_on) = end_on {
                    validate_as_of(end_on)?;
                    if end_on < anchor_on {
                        return Err(PlanError::EndBeforeAnchor);
                    }
                }
                Ok(())
            }
            Self::OneOff { on } => validate_as_of(on),
            Self::Goal {
                start_on, due_on, ..
            } => {
                if direction != PlanDirection::Outflow {
                    return Err(PlanError::GoalMustBeOutflow);
                }
                validate_as_of(start_on)?;
                validate_as_of(due_on)?;
                if due_on <= start_on {
                    return Err(PlanError::GoalDatesInverted);
                }
                Ok(())
            }
        }
    }
}

/// Whether an item going in `direction` on `schedule` has a [`PlanSpending`]:
/// only a recurring outflow does.
#[must_use]
pub fn spending_applies(direction: PlanDirection, schedule: &PlanSchedule) -> bool {
    direction == PlanDirection::Outflow && matches!(schedule, PlanSchedule::Recurring { .. })
}

/// The whole days from `earlier` to `later`, negative when they are the other
/// way round. Civil dates have 24-hour days, so this is exact.
fn days_between(earlier: Date, later: Date) -> i64 {
    later.duration_since(earlier).as_secs() / SECONDS_PER_DAY
}

/// Checks the amount of a planned item: 1 to [`MAX_PLAN_AMOUNT_MINOR`].
///
/// # Errors
/// Returns [`PlanError::InvalidAmount`] for anything else.
pub fn validate_amount(amount: Money) -> Result<(), PlanError> {
    if (1..=MAX_PLAN_AMOUNT_MINOR).contains(&amount.minor_units) {
        Ok(())
    } else {
        Err(PlanError::InvalidAmount)
    }
}

/// Checks a forecast's `as_of` date: in the years [`MIN_PLAN_YEAR`] to
/// [`MAX_PLAN_YEAR`].
///
/// # Errors
/// Returns [`PlanError::DateOutOfRange`] for anything else.
pub fn validate_as_of(date: Date) -> Result<(), PlanError> {
    if (MIN_PLAN_YEAR..=MAX_PLAN_YEAR).contains(&date.year()) {
        Ok(())
    } else {
        Err(PlanError::DateOutOfRange)
    }
}

#[cfg(test)]
mod tests {
    use jiff::civil::date;

    use super::{
        ForecastSettings, IntervalUnit, MAX_PLAN_AMOUNT_MINOR, PlanDirection, PlanError,
        PlanItemType, PlanSchedule, PlanSpending, PlannedItemName, SafetyFloor, spending_applies,
        validate_amount, validate_as_of,
    };
    use crate::money::{Currency, Money};

    fn usd(minor_units: i64) -> Money {
        Money {
            minor_units,
            currency: Currency::parse("USD").unwrap(),
        }
    }

    fn recurring(every: u16) -> PlanSchedule {
        PlanSchedule::Recurring {
            every,
            unit: IntervalUnit::Months,
            anchor_on: date(2027, 1, 1),
            end_on: None,
        }
    }

    fn goal(start_on: jiff::civil::Date, due_on: jiff::civil::Date) -> PlanSchedule {
        PlanSchedule::Goal {
            start_on,
            due_on,
            completed: false,
        }
    }

    const OUT: PlanDirection = PlanDirection::Outflow;

    #[test]
    fn only_a_recurring_outflow_has_a_spending_kind() {
        let recurring = PlanSchedule::Recurring {
            every: 1,
            unit: IntervalUnit::Months,
            anchor_on: date(2027, 1, 1),
            end_on: None,
        };
        let one_off = PlanSchedule::OneOff {
            on: date(2027, 1, 1),
        };
        let goal = PlanSchedule::Goal {
            start_on: date(2027, 1, 1),
            due_on: date(2027, 2, 1),
            completed: false,
        };
        assert!(spending_applies(PlanDirection::Outflow, &recurring));
        assert!(!spending_applies(PlanDirection::Inflow, &recurring));
        assert!(!spending_applies(PlanDirection::Outflow, &one_off));
        assert!(!spending_applies(PlanDirection::Outflow, &goal));
    }

    /// One case per `PlanError` that a value check raises.
    #[test]
    fn validate_refuses_each_case() {
        let fine = date(2027, 1, 1);
        let settings = ForecastSettings::default();
        let cases = [
            (
                PlannedItemName::parse("  ").map(|_| ()),
                PlanError::InvalidName,
            ),
            (validate_amount(usd(0)), PlanError::InvalidAmount),
            (recurring(0).validate(OUT), PlanError::InvalidEvery),
            (
                validate_as_of(date(1969, 12, 31)),
                PlanError::DateOutOfRange,
            ),
            (
                PlanSchedule::Recurring {
                    every: 1,
                    unit: IntervalUnit::Months,
                    anchor_on: fine,
                    end_on: Some(date(2026, 12, 31)),
                }
                .validate(OUT),
                PlanError::EndBeforeAnchor,
            ),
            (
                goal(date(2027, 2, 1), fine).validate(OUT),
                PlanError::GoalDatesInverted,
            ),
            (
                goal(fine, date(2028, 1, 1)).validate(PlanDirection::Inflow),
                PlanError::GoalMustBeOutflow,
            ),
            (
                SafetyFloor::Fixed { amount: usd(-1) }.validate(),
                PlanError::InvalidFloor,
            ),
            (
                ForecastSettings {
                    default_horizon_months: 0,
                    ..settings
                }
                .validate(),
                PlanError::InvalidHorizon,
            ),
            (
                ForecastSettings {
                    stale_after_days: 0,
                    ..settings
                }
                .validate(),
                PlanError::InvalidStaleDays,
            ),
            (
                SafetyFloor::MonthsOfOutflows { months: 0 }.validate(),
                PlanError::InvalidFloorMonths,
            ),
        ];
        for (result, error) in cases {
            assert_eq!(result, Err(error), "each check raises its own error");
        }
    }

    #[test]
    fn names_amounts_and_every_stop_at_their_bounds() {
        assert_eq!(
            PlannedItemName::parse(&"x".repeat(101)),
            Err(PlanError::InvalidName),
            "a name of 101 characters is refused"
        );
        assert_eq!(
            PlannedItemName::parse("a\u{7}b"),
            Err(PlanError::InvalidName),
            "a control character is refused"
        );
        assert!(
            PlannedItemName::parse(&"x".repeat(100)).is_ok(),
            "a name of 100 characters is accepted"
        );

        for bad in [0, -1, MAX_PLAN_AMOUNT_MINOR + 1, i64::MIN, i64::MAX] {
            assert_eq!(
                validate_amount(usd(bad)),
                Err(PlanError::InvalidAmount),
                "an amount outside the range is refused"
            );
        }
        for good in [1, MAX_PLAN_AMOUNT_MINOR] {
            assert_eq!(validate_amount(usd(good)), Ok(()), "bounds are accepted");
        }

        for bad in [0, 1000, u16::MAX] {
            assert_eq!(
                recurring(bad).validate(OUT),
                Err(PlanError::InvalidEvery),
                "every outside 1 to 999 is refused"
            );
        }
        for good in [1, 999] {
            assert_eq!(recurring(good).validate(OUT), Ok(()), "bounds are accepted");
        }
    }

    #[test]
    fn every_schedule_date_stays_in_the_plan_years() {
        let early = date(1969, 12, 31);
        let late = date(2201, 1, 1);
        let fine = date(2027, 1, 1);
        for bad in [early, late] {
            assert_eq!(
                validate_as_of(bad),
                Err(PlanError::DateOutOfRange),
                "as_of outside the years is refused"
            );
            for schedule in [
                PlanSchedule::OneOff { on: bad },
                PlanSchedule::Recurring {
                    every: 1,
                    unit: IntervalUnit::Days,
                    anchor_on: bad,
                    end_on: None,
                },
                goal(bad, date(2201, 6, 1)),
            ] {
                assert_eq!(
                    schedule.validate(OUT),
                    Err(PlanError::DateOutOfRange),
                    "a first date outside the years is refused"
                );
            }
        }
        for schedule in [
            PlanSchedule::Recurring {
                every: 1,
                unit: IntervalUnit::Days,
                anchor_on: fine,
                end_on: Some(late),
            },
            goal(fine, late),
            goal(early, fine),
        ] {
            assert_eq!(
                schedule.validate(OUT),
                Err(PlanError::DateOutOfRange),
                "any date outside the years is refused"
            );
        }
        for good in [date(1970, 1, 1), date(2200, 12, 31)] {
            assert_eq!(validate_as_of(good), Ok(()), "the year bounds are accepted");
            assert_eq!(
                PlanSchedule::OneOff { on: good }.validate(OUT),
                Ok(()),
                "the year bounds are accepted"
            );
        }
    }

    #[test]
    fn end_dates_goals_and_directions_are_checked() {
        let fine = date(2027, 1, 1);
        assert_eq!(
            PlanSchedule::Recurring {
                every: 1,
                unit: IntervalUnit::Months,
                anchor_on: fine,
                end_on: Some(fine),
            }
            .validate(OUT),
            Ok(()),
            "an end on the anchor is one occurrence"
        );
        assert_eq!(
            goal(fine, fine).validate(OUT),
            Err(PlanError::GoalDatesInverted),
            "a goal with no period to accrue over is refused"
        );
        assert_eq!(
            goal(fine, date(2027, 1, 2)).validate(OUT),
            Ok(()),
            "a one-day goal is accepted"
        );
        assert_eq!(
            recurring(1).validate(PlanDirection::Inflow),
            Ok(()),
            "a recurring inflow is accepted"
        );
        assert_eq!(
            PlanSchedule::OneOff { on: fine }.validate(PlanDirection::Inflow),
            Ok(()),
            "a one-off inflow is accepted"
        );
    }

    #[test]
    fn enum_texts_parse_and_unknown_ones_are_refused() {
        assert_eq!("outflow".parse(), Ok(PlanDirection::Outflow), "known text");
        assert_eq!("inflow".parse(), Ok(PlanDirection::Inflow), "known text");
        assert_eq!(
            "out".parse::<PlanDirection>(),
            Err(PlanError::UnknownDirection),
            "unknown direction"
        );
        for (text, unit) in [
            ("days", IntervalUnit::Days),
            ("weeks", IntervalUnit::Weeks),
            ("months", IntervalUnit::Months),
            ("years", IntervalUnit::Years),
        ] {
            assert_eq!(text.parse(), Ok(unit), "known text");
            assert_eq!(unit.as_str(), text, "text roundtrips");
        }
        assert_eq!(
            "Months".parse::<IntervalUnit>(),
            Err(PlanError::UnknownIntervalUnit),
            "units are case-sensitive"
        );
        assert_eq!("one_off".parse(), Ok(PlanItemType::OneOff), "known text");
        assert_eq!(
            "oneoff".parse::<PlanItemType>(),
            Err(PlanError::UnknownItemType),
            "unknown item type"
        );
    }

    #[test]
    fn spending_texts_parse_and_serialize() {
        for (text, spending) in [
            ("bill", PlanSpending::Bill),
            ("budget", PlanSpending::Budget),
        ] {
            assert_eq!(text.parse(), Ok(spending), "known text");
            assert_eq!(spending.as_str(), text, "text roundtrips");
            assert_eq!(
                serde_json::to_string(&spending).unwrap(),
                format!("\"{text}\""),
                "plain text in JSON"
            );
        }
        assert_eq!(
            "Budget".parse::<PlanSpending>(),
            Err(PlanError::UnknownSpending),
            "case-sensitive"
        );
    }

    #[test]
    fn item_type_follows_the_schedule() {
        let on = date(2027, 1, 1);
        assert_eq!(
            recurring(1).item_type(),
            PlanItemType::Recurring,
            "recurring"
        );
        assert_eq!(
            PlanSchedule::OneOff { on }.item_type(),
            PlanItemType::OneOff,
            "one-off"
        );
        assert_eq!(goal(on, on).item_type(), PlanItemType::Goal, "goal");
    }

    #[test]
    fn schedule_serde_is_tagged() {
        let one_off = PlanSchedule::OneOff {
            on: date(2027, 1, 1),
        };
        let text = r#"{"type":"one_off","on":"2027-01-01"}"#;
        assert_eq!(
            serde_json::to_string(&one_off).unwrap(),
            text,
            "one-off shape"
        );
        assert_eq!(
            serde_json::from_str::<PlanSchedule>(text).unwrap(),
            one_off,
            "one-off roundtrip"
        );

        let recurring: PlanSchedule = serde_json::from_str(
            r#"{"type":"recurring","every":2,"unit":"weeks","anchor_on":"2027-01-31"}"#,
        )
        .unwrap();
        assert_eq!(
            recurring,
            PlanSchedule::Recurring {
                every: 2,
                unit: IntervalUnit::Weeks,
                anchor_on: date(2027, 1, 31),
                end_on: None,
            },
            "an end date is optional"
        );

        let goal: PlanSchedule = serde_json::from_str(
            r#"{"type":"goal","start_on":"2027-01-01","due_on":"2028-01-01"}"#,
        )
        .unwrap();
        assert_eq!(
            goal,
            PlanSchedule::Goal {
                start_on: date(2027, 1, 1),
                due_on: date(2028, 1, 1),
                completed: false,
            },
            "completed defaults to false"
        );
    }

    /// Every date field of every schedule type, with `date` in it.
    fn schedules_with(date: &str) -> [String; 5] {
        [
            format!(r#"{{"type":"one_off","on":"{date}"}}"#),
            format!(r#"{{"type":"recurring","every":1,"unit":"days","anchor_on":"{date}"}}"#),
            format!(
                r#"{{"type":"recurring","every":1,"unit":"days","anchor_on":"2027-01-01","end_on":"{date}"}}"#
            ),
            format!(r#"{{"type":"goal","start_on":"{date}","due_on":"2028-01-01"}}"#),
            format!(r#"{{"type":"goal","start_on":"2027-01-01","due_on":"{date}"}}"#),
        ]
    }

    #[test]
    fn schedule_dates_are_plain_dates() {
        for text in schedules_with("2027-06-01") {
            assert!(
                serde_json::from_str::<PlanSchedule>(&text).is_ok(),
                "a plain date is accepted"
            );
        }
        for bad in [
            "2027-06-01T00:00:00",
            "2027-06-01T23:30:00-05:00",
            "2027-06-01T23:30:00-05[America/New_York]",
            "2027-06-01 00:00:00",
            "+002027-06-01",
            "-000001-06-01",
            "20270601",
            "2027-6-1",
            "2027-02-30",
            " 2027-06-01",
            "2027-06-01 ",
            "٢٠٢٧-06-01",
        ] {
            for text in schedules_with(bad) {
                assert!(
                    serde_json::from_str::<PlanSchedule>(&text).is_err(),
                    "only a plain YYYY-MM-DD date is accepted"
                );
            }
        }
        let null_end = r#"{"type":"recurring","every":1,"unit":"days","anchor_on":"2027-01-01","end_on":null}"#;
        assert!(
            serde_json::from_str::<PlanSchedule>(null_end).is_ok(),
            "a null end date means none"
        );
    }

    #[test]
    fn a_refused_date_is_not_echoed() {
        let text = r#"{"type":"one_off","on":"sensitive-text"}"#;
        let error = serde_json::from_str::<PlanSchedule>(text).unwrap_err();
        assert!(
            !error.to_string().contains("sensitive"),
            "the input was echoed"
        );
    }

    #[test]
    fn error_messages_state_the_limits() {
        for (error, text) in [
            (
                PlanError::InvalidName,
                "name must be 1 to 100 characters without control characters",
            ),
            (
                PlanError::InvalidAmount,
                "amount must be between 1 and 1000000000000000 minor units",
            ),
            (PlanError::InvalidEvery, "every must be 1 to 999"),
            (
                PlanError::DateOutOfRange,
                "dates must fall in the years 1970 to 2200",
            ),
            (
                PlanError::InvalidFloor,
                "a fixed floor must be between 0 and 1000000000000000 minor units",
            ),
            (
                PlanError::InvalidHorizon,
                "the horizon must be 1 to 60 months",
            ),
            (
                PlanError::InvalidStaleDays,
                "stale after must be 1 to 365 days",
            ),
            (
                PlanError::InvalidFloorMonths,
                "floor months must be 1 to 60",
            ),
        ] {
            assert_eq!(error.to_string(), text, "the message states the limit");
        }
    }

    #[test]
    fn unknown_schedule_field_is_refused() {
        for text in [
            r#"{"type":"one_off","on":"2027-01-01","extra":1}"#,
            r#"{"type":"one_off","on":"2027-01-01","every":1}"#,
            r#"{"type":"goal","start_on":"2027-01-01","due_on":"2028-01-01","on":"2027-01-01"}"#,
            r#"{"type":"recurring","every":1,"unit":"days","anchor_on":"2027-01-01","day":31}"#,
            r#"{"type":"weekly","on":"2027-01-01"}"#,
            r#"{"on":"2027-01-01"}"#,
            r#"{"type":"recurring","every":70000,"unit":"days","anchor_on":"2027-01-01"}"#,
            r#"{"type":"recurring","every":1.5,"unit":"days","anchor_on":"2027-01-01"}"#,
        ] {
            assert!(
                serde_json::from_str::<PlanSchedule>(text).is_err(),
                "a malformed schedule is refused"
            );
        }
    }
}
