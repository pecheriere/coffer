//! Plan history (money redesign §10.3): a planned item's amount, direction, category,
//! spending kind and tier are effective-dated versions. A version is written only from this
//! month or next, and the version in force in a month is the latest-written row that has
//! started by then, so a write can never change what a past month reads.

use std::fmt;
use std::str::FromStr;

use jiff::civil::Date;
use serde::{Deserialize, Deserializer, Serialize};

use super::{CalendarMonth, PlanDirection, PlanError, PlanItemType, PlanSpending, PlanTier};
use crate::ids::CategoryId;
use crate::money::Money;

/// The values of a planned item that change over time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanValues {
    pub amount: Money,
    pub direction: PlanDirection,
    pub category: CategoryId,
    pub spending: Option<PlanSpending>,
    pub tier: Option<PlanTier>,
}

/// One row of an item's history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanVersion {
    /// Numbers the writes of one item: 1, 2, 3, in the order they were made.
    pub seq: u32,
    /// The first month the values apply to.
    pub effective_from: CalendarMonth,
    /// The day the row was written.
    pub written_on: Date,
    pub values: PlanValues,
}

/// Months `from` (inclusive, `None`: since ever) to `until` (exclusive, `None`: for ever).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MonthSpan {
    pub from: Option<CalendarMonth>,
    pub until: Option<CalendarMonth>,
}

impl MonthSpan {
    /// Every month.
    pub const ALWAYS: Self = Self {
        from: None,
        until: None,
    };

    /// Whether `date` falls in one of the span's months.
    #[must_use]
    pub fn contains(self, date: Date) -> bool {
        CalendarMonth::of(date).is_ok_and(|month| self.contains_month(month))
    }

    /// Whether `month` is one of the span's months.
    #[must_use]
    pub fn contains_month(self, month: CalendarMonth) -> bool {
        self.from.is_none_or(|from| from <= month) && self.until.is_none_or(|until| month < until)
    }

    /// Whether the two spans share a month.
    #[must_use]
    pub fn overlaps(self, other: Self) -> bool {
        let starts_before_other_ends = match (self.from, other.until) {
            (Some(from), Some(until)) => from < until,
            _ => true,
        };
        let other_starts_before_end = match (other.from, self.until) {
            (Some(from), Some(until)) => from < until,
            _ => true,
        };
        starts_before_other_ends && other_starts_before_end
    }
}

/// A row of the history with the months it is in force for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VersionSpan<'a> {
    pub span: MonthSpan,
    pub version: &'a PlanVersion,
}

/// An item's history: not empty, strictly increasing in `seq`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanVersions(Vec<PlanVersion>);

impl PlanVersions {
    /// # Errors
    /// Returns [`PlanError::EmptyVersions`] for no rows and
    /// [`PlanError::UnorderedVersions`] when `seq` does not strictly increase.
    pub fn new(rows: Vec<PlanVersion>) -> Result<Self, PlanError> {
        if rows.is_empty() {
            return Err(PlanError::EmptyVersions);
        }
        if rows.windows(2).any(|pair| pair[0].seq >= pair[1].seq) {
            return Err(PlanError::UnorderedVersions);
        }
        Ok(Self(rows))
    }

    /// The rows, oldest write first.
    #[must_use]
    pub fn rows(&self) -> &[PlanVersion] {
        &self.0
    }

    /// The number the next write takes.
    #[must_use]
    pub fn next_seq(&self) -> u32 {
        self.0.last().map_or(1, |last| last.seq.saturating_add(1))
    }

    fn row_in_force(&self, month: CalendarMonth) -> &PlanVersion {
        self.0
            .iter()
            .rev()
            .find(|row| row.effective_from <= month)
            .unwrap_or(&self.0[0])
    }

    /// The values in force in `month`: the row written last among those effective by then,
    /// or the first row when none is.
    #[must_use]
    pub fn in_force(&self, month: CalendarMonth) -> &PlanValues {
        &self.row_in_force(month).values
    }

    /// The row that takes effect in `month` and is in force there, if any: a change written
    /// ahead of that month. The first row never is, as it also reads for the months before it.
    #[must_use]
    pub fn starting_in(&self, month: CalendarMonth) -> Option<&PlanVersion> {
        let row = self.row_in_force(month);
        (row.effective_from == month && row.seq != self.0[0].seq).then_some(row)
    }

    /// Each row that is ever in force, with the months it is.
    #[must_use]
    pub fn timeline(&self) -> Vec<VersionSpan<'_>> {
        self.0
            .iter()
            .enumerate()
            .filter_map(|(index, version)| {
                let until = self.0[index + 1..]
                    .iter()
                    .map(|later| later.effective_from)
                    .min();
                let from = (index > 0).then_some(version.effective_from);
                if let (Some(from), Some(until)) = (from, until)
                    && until <= from
                {
                    return None;
                }
                Some(VersionSpan {
                    span: MonthSpan { from, until },
                    version,
                })
            })
            .collect()
    }

    /// Every row in force for `month` at some moment of it: the one in force when the month
    /// opened and each row written during it.
    #[must_use]
    pub fn held_during(&self, month: CalendarMonth) -> Vec<&PlanValues> {
        let opened = month.first_day();
        let started = |row: &&PlanVersion| row.effective_from <= month;
        let mut held: Vec<&PlanValues> = self
            .0
            .iter()
            .rev()
            .filter(started)
            .find(|row| row.written_on < opened)
            .map(|row| &row.values)
            .into_iter()
            .collect();
        held.extend(
            self.0
                .iter()
                .filter(started)
                .filter(|row| CalendarMonth::of(row.written_on) == Ok(month))
                .map(|row| &row.values),
        );
        if held.is_empty() {
            held.push(self.in_force(month));
        }
        held
    }
}

/// From when a patch changes the versioned values: `"this_month"`, `"next_month"` or a
/// month written `YYYY-MM`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EffectiveFrom {
    #[default]
    ThisMonth,
    NextMonth,
    Month(CalendarMonth),
}

impl EffectiveFrom {
    /// The month a change takes effect from, for an item of `item_type`, today being `today`.
    ///
    /// # Errors
    /// Returns [`PlanError::PastVersion`] for any month but this one and the next, and
    /// [`PlanError::NextMonthNeedsRecurring`] for next month on an item that isn't recurring.
    pub fn resolve(self, today: Date, item_type: PlanItemType) -> Result<CalendarMonth, PlanError> {
        let this = CalendarMonth::of(today).map_err(|_| PlanError::DateOutOfRange)?;
        let next = this.next();
        let month = match self {
            Self::ThisMonth => this,
            Self::NextMonth => next,
            Self::Month(month) => month,
        };
        if month != this && month != next {
            return Err(PlanError::PastVersion);
        }
        if month == next && item_type != PlanItemType::Recurring {
            return Err(PlanError::NextMonthNeedsRecurring);
        }
        Ok(month)
    }
}

impl fmt::Display for EffectiveFrom {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ThisMonth => f.write_str("this_month"),
            Self::NextMonth => f.write_str("next_month"),
            Self::Month(month) => write!(f, "{month}"),
        }
    }
}

impl FromStr for EffectiveFrom {
    type Err = PlanError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "this_month" => Ok(Self::ThisMonth),
            "next_month" => Ok(Self::NextMonth),
            other => other
                .parse()
                .map(Self::Month)
                .map_err(|_| PlanError::InvalidEffectiveFrom),
        }
    }
}

impl<'de> Deserialize<'de> for EffectiveFrom {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(|_| {
            serde::de::Error::custom("effective_from must be this_month, next_month or YYYY-MM")
        })
    }
}

impl Serialize for EffectiveFrom {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

#[cfg(feature = "openapi")]
impl utoipa::PartialSchema for EffectiveFrom {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        use utoipa::openapi::schema::{ObjectBuilder, Type};
        ObjectBuilder::new()
            .schema_type(Type::String)
            .pattern(Some("^(this_month|next_month|[0-9]{4}-(0[1-9]|1[0-2]))$"))
            .description(Some(
                "From when the change applies: this_month (the default), next_month, or a \
                 month written YYYY-MM that is this month or next.",
            ))
            .into()
    }
}

#[cfg(feature = "openapi")]
impl utoipa::ToSchema for EffectiveFrom {
    fn name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("EffectiveFrom")
    }
}

#[cfg(test)]
mod tests;
