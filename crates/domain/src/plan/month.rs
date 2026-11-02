//! A calendar month, written `YYYY-MM`.

use std::fmt;
use std::str::FromStr;

use jiff::Span;
use jiff::civil::Date;
use serde::{Deserialize, Serialize};

/// Returned for a string that is not a month written `YYYY-MM`, or a date
/// outside the years 0000 to 9999, which that form cannot write. Never echoes
/// the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid month")]
pub struct InvalidMonth;

/// One calendar month, in the years 0000 to 9999. Ordered chronologically. Its
/// fields are private and every constructor keeps them in range, so
/// [`Self::first_day`] always names a real date and its text always parses
/// back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
#[serde(try_from = "String")]
pub struct CalendarMonth {
    year: i16,
    month: i8,
}

impl CalendarMonth {
    /// The month `date` falls in.
    ///
    /// # Errors
    /// Returns [`InvalidMonth`] for a date before year 0.
    pub fn of(date: Date) -> Result<Self, InvalidMonth> {
        if date.year() < 0 {
            return Err(InvalidMonth);
        }
        Ok(Self {
            year: date.year(),
            month: date.month(),
        })
    }

    /// The first day of the month.
    #[must_use]
    pub fn first_day(self) -> Date {
        // Every constructor keeps the year and month inside the calendar, so
        // this date exists and the fallback never runs.
        Date::new(self.year, self.month, 1).unwrap_or(Date::MAX)
    }

    /// The month after this one. The last month of the calendar, 9999-12, is
    /// its own successor, so a loop over months needs a bound of its own.
    #[must_use]
    pub fn next(self) -> Self {
        self.first_day()
            .checked_add(Span::new().months(1))
            .ok()
            .and_then(|date| Self::of(date).ok())
            .unwrap_or(self)
    }
}

impl fmt::Display for CalendarMonth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}", self.year, self.month)
    }
}

impl FromStr for CalendarMonth {
    type Err = InvalidMonth;

    /// Accepts exactly four ASCII digits, `-`, and a month from `01` to `12`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let &[y0, y1, y2, y3, b'-', m0, m1] = s.as_bytes() else {
            return Err(InvalidMonth);
        };
        if ![y0, y1, y2, y3, m0, m1].iter().all(u8::is_ascii_digit) {
            return Err(InvalidMonth);
        }
        let year = [y0, y1, y2, y3]
            .into_iter()
            .fold(0_i16, |acc, digit| acc * 10 + i16::from(digit - b'0'));
        let month = i8::try_from((m0 - b'0') * 10 + (m1 - b'0')).map_err(|_| InvalidMonth)?;
        if (1..=12).contains(&month) {
            Ok(Self { year, month })
        } else {
            Err(InvalidMonth)
        }
    }
}

impl TryFrom<String> for CalendarMonth {
    type Error = InvalidMonth;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl Serialize for CalendarMonth {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

#[cfg(feature = "openapi")]
impl utoipa::PartialSchema for CalendarMonth {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        use utoipa::openapi::schema::{ObjectBuilder, Type};
        ObjectBuilder::new()
            .schema_type(Type::String)
            .pattern(Some("^[0-9]{4}-(0[1-9]|1[0-2])$"))
            .description(Some("A calendar month, written YYYY-MM."))
            .into()
    }
}

#[cfg(feature = "openapi")]
impl utoipa::ToSchema for CalendarMonth {
    fn name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("CalendarMonth")
    }
}

#[cfg(test)]
mod tests {
    use jiff::civil::{Date, date};
    use proptest::prelude::Strategy;

    use super::{CalendarMonth, InvalidMonth};

    fn month(s: &str) -> CalendarMonth {
        s.parse().unwrap()
    }

    #[test]
    fn month_roundtrips_and_rejects() {
        for text in ["2026-01", "2026-12", "0000-01", "9999-12", "1970-07"] {
            assert_eq!(month(text).to_string(), text, "a month roundtrips");
            let json = format!("\"{text}\"");
            assert_eq!(
                serde_json::to_string(&month(text)).unwrap(),
                json,
                "serializes as text"
            );
            assert_eq!(
                serde_json::from_str::<CalendarMonth>(&json).unwrap(),
                month(text),
                "deserializes from text"
            );
        }
        for text in [
            "2026-13",
            "2026-00",
            "2026-1",
            "26-01",
            "",
            "2026",
            "2026-",
            "-2026-01",
            "+026-01",
            "-026-01",
            "2026-01-01",
            " 2026-01",
            "2026-01 ",
            "2026/01",
            "202a-01",
            "2026-1a",
            "2026-+1",
            "٢٠٢٦-٠١",
            "20٢6-01",
        ] {
            assert_eq!(
                text.parse::<CalendarMonth>(),
                Err(InvalidMonth),
                "a malformed month is refused"
            );
        }
    }

    #[test]
    fn a_refused_month_is_not_echoed() {
        let error = serde_json::from_str::<CalendarMonth>("\"sensitive-text\"").unwrap_err();
        assert!(
            !error.to_string().contains("sensitive"),
            "the input was echoed"
        );
        assert_eq!(InvalidMonth.to_string(), "invalid month", "fixed message");
    }

    #[test]
    fn months_sort_chronologically() {
        assert!(month("2026-12") < month("2027-01"), "across a year");
        assert!(month("2026-02") < month("2026-10"), "within a year");
    }

    #[test]
    fn of_first_day_and_next() {
        assert_eq!(
            CalendarMonth::of(date(2028, 2, 29)),
            Ok(month("2028-02")),
            "leap day"
        );
        assert_eq!(month("2028-02").first_day(), date(2028, 2, 1), "first day");
        assert_eq!(month("2026-11").next(), month("2026-12"), "within a year");
        assert_eq!(month("2026-12").next(), month("2027-01"), "across a year");
    }

    #[test]
    fn the_last_month_is_its_own_successor() {
        assert_eq!(
            month("9999-12").next(),
            month("9999-12"),
            "no month after the calendar"
        );
        assert_eq!(
            month("9999-11").next(),
            month("9999-12"),
            "still inside the calendar"
        );
    }

    #[test]
    fn a_date_before_year_zero_has_no_month() {
        assert_eq!(
            CalendarMonth::of(date(-1, 12, 31)),
            Err(InvalidMonth),
            "year -1"
        );
        assert_eq!(
            CalendarMonth::of(Date::MIN),
            Err(InvalidMonth),
            "the calendar's first day"
        );
        assert_eq!(
            CalendarMonth::of(date(0, 1, 1)).map(|month| month.to_string()),
            Ok("0000-01".to_owned()),
            "year 0 is the first the form can write"
        );
    }

    fn any_date() -> impl Strategy<Value = Date> {
        (-9999_i16..=9999, 1_i8..=12, 1_i8..=31).prop_map(|(year, month, day)| {
            let last = date(year, month, 1).days_in_month();
            date(year, month, day.min(last))
        })
    }

    proptest::proptest! {
        #[test]
        fn a_month_contains_its_dates_and_its_text_parses_back(any in any_date()) {
            let Ok(of) = CalendarMonth::of(any) else {
                proptest::prop_assert!(any.year() < 0, "a writable year was refused");
                return Ok(());
            };
            proptest::prop_assert!(any.year() >= 0, "an unwritable year was accepted");
            proptest::prop_assert_eq!(
                of.to_string().parse::<CalendarMonth>(),
                Ok(of),
                "the text parses back"
            );
            let first = of.first_day();
            proptest::prop_assert_eq!(first.year(), any.year(), "same year");
            proptest::prop_assert_eq!(first.month(), any.month(), "same month");
            proptest::prop_assert_eq!(first.day(), 1, "first day");
            proptest::prop_assert_eq!(CalendarMonth::of(first), Ok(of), "first day roundtrips");
            if of.next() != of {
                proptest::prop_assert!(of < of.next(), "next is later");
                proptest::prop_assert_eq!(
                    of.next().first_day(),
                    first.last_of_month().tomorrow().unwrap(),
                    "next starts the day after this month ends"
                );
            }
        }
    }
}
