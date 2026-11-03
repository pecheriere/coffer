//! Expected values are worked out by hand from real ECB rates of 2026-10-30
//! unless a test says it uses a fixture. EUR 1 = USD 1.1172 = GBP 0.8338 =
//! JPY 175.68.

use jiff::ToSpan;
use jiff::civil::{Date, date};
use proptest::prelude::{Just, Strategy, prop_assert, prop_assert_eq, prop_oneof, proptest};
use proptest::{collection, sample};

use super::{
    Conversion, FxErrorCode, FxFetchStatus, FxRate, FxSource, HeldRate, RateBook, RateHistory,
    RateLeg, RateLookup, RateObservation, convert, unusual_against,
};
use crate::money::Currency;

const FRIDAY: Date = date(2026, 10, 30);

fn currency(code: &str) -> Currency {
    Currency::parse(code).unwrap()
}

fn rate(text: &str) -> FxRate {
    text.parse().unwrap()
}

fn history(rates: &[(Date, &str)]) -> RateHistory {
    RateHistory::new(
        rates
            .iter()
            .map(|&(on, text)| RateObservation {
                on,
                rate: rate(text),
            })
            .collect(),
    )
}

fn book(entries: &[(&str, &[(Date, &str)])]) -> RateBook {
    let mut book = RateBook::default();
    for &(code, rates) in entries {
        book.insert(currency(code), history(rates));
    }
    book
}

/// The rates of 2026-10-30.
fn friday_book() -> RateBook {
    book(&[
        ("USD", &[(FRIDAY, "1.1172")]),
        ("GBP", &[(FRIDAY, "0.8338")]),
        ("JPY", &[(FRIDAY, "175.68")]),
        ("ISK", &[(FRIDAY, "136")]),
        ("KWD", &[(FRIDAY, "0.34")]),
    ])
}

fn on_friday(minor: i64, from: &str, to: &str) -> Conversion {
    convert(minor, currency(from), currency(to), FRIDAY, &friday_book())
}

fn converted(minor: i64, rate_on: Date, leg: RateLeg) -> Conversion {
    Conversion::Converted {
        minor,
        rate_on,
        leg,
        manual: None,
    }
}

#[test]
fn rate_canonical_form() {
    assert_eq!(rate("1.1150"), rate("1.115"));
    assert_eq!(rate("1.1150").to_string(), "1.115");
    assert_eq!(rate("136").to_string(), "136");
    assert_eq!(rate("136.000").to_string(), "136");
    assert_eq!(rate("0.5").to_string(), "0.5");
    assert_eq!(rate("0.000001").to_string(), "0.000001");
    assert_eq!(rate("20275.17").to_string(), "20275.17");
    assert_eq!(rate("007.50").to_string(), "7.5");
    assert_eq!(rate("9999999999").to_string(), "9999999999");
    assert_eq!(rate("9999.999999").to_string(), "9999.999999");
}

#[test]
fn rate_refuses_anything_else() {
    for bad in [
        "",
        "0",
        "0.000000",
        "-1",
        "+1",
        "1e3",
        " 1",
        "1 ",
        "1.",
        ".5",
        "1,5",
        "1.2.3",
        "١",
        "12345678901",
        "1234567.1234",
        "1.1234567",
    ] {
        assert!(bad.parse::<FxRate>().is_err(), "{bad:?}");
    }
}

#[test]
fn rate_errors_do_not_echo_input() {
    let err = "https://example.test/tracking"
        .parse::<FxRate>()
        .unwrap_err();
    assert!(!err.to_string().contains("example"));
}

#[test]
fn rate_serde_is_canonical_text() {
    assert_eq!(serde_json::to_string(&rate("1.1150")).unwrap(), "\"1.115\"");
    assert_eq!(
        serde_json::from_str::<FxRate>("\"1.1150\"").unwrap(),
        rate("1.115")
    );
    assert!(serde_json::from_str::<FxRate>("1.115").is_err());
    assert!(serde_json::from_str::<FxRate>("\"1e3\"").is_err());
}

#[cfg(feature = "openapi")]
#[test]
fn rate_schema_is_text_with_a_pattern() {
    let schema = serde_json::to_value(<FxRate as utoipa::PartialSchema>::schema()).unwrap();
    assert_eq!(schema["type"], "string");
    assert_eq!(schema["pattern"], r"^[0-9]{1,10}(\.[0-9]{1,6})?$");
}

#[test]
fn status_words_are_fixed() {
    assert_eq!(FxSource::Ecb.as_str(), "ecb");
    assert_eq!("ecb".parse(), Ok(FxSource::Ecb));
    assert_eq!(FxFetchStatus::NotPublished.as_str(), "not_published");
    assert_eq!("failing".parse(), Ok(FxFetchStatus::Failing));
    assert_eq!("ready".parse(), Ok(FxFetchStatus::Ready));
    assert_eq!(FxErrorCode::Malformed.as_str(), "malformed");
    assert_eq!("network".parse(), Ok(FxErrorCode::Network));
    assert_eq!("unavailable".parse(), Ok(FxErrorCode::Unavailable));
    assert!("Ready".parse::<FxFetchStatus>().is_err());
}

#[test]
fn euro_to_dollar() {
    // 100,000 × 1.1172 = 111,720 exactly.
    assert_eq!(
        on_friday(100_000, "EUR", "USD"),
        converted(111_720, FRIDAY, RateLeg::Display)
    );
}

#[test]
fn dollar_to_euro_rounds() {
    // 550,000 / 1.1172 = 492,302.18... and -30,000 / 1.1172 = -26,852.85...
    assert_eq!(
        on_friday(550_000, "USD", "EUR"),
        converted(492_302, FRIDAY, RateLeg::Held)
    );
    assert_eq!(
        on_friday(-30_000, "USD", "EUR"),
        converted(-26_853, FRIDAY, RateLeg::Held)
    );
}

#[test]
fn cross_rate_goes_through_euro() {
    // 100,000 / 0.8338 × 1.1172 = 133,988.96...
    assert_eq!(
        on_friday(100_000, "GBP", "USD"),
        converted(133_989, FRIDAY, RateLeg::Both)
    );
}

#[test]
fn exponents_are_respected() {
    // JPY 1,000,000 / 175.68 × 1.1172 = USD 6,359.2896... = 635,928.96... cents.
    assert_eq!(
        on_friday(1_000_000, "JPY", "USD"),
        converted(635_929, FRIDAY, RateLeg::Both)
    );
    // USD 1,000.00 / 1.1172 × 175.68 = JPY 157,250.26...
    assert_eq!(
        on_friday(100_000, "USD", "JPY"),
        converted(157_250, FRIDAY, RateLeg::Both)
    );
    // KWD 1.234 / 0.34 × 175.68 = JPY 637.61...
    assert_eq!(
        on_friday(1_234, "KWD", "JPY"),
        converted(638, FRIDAY, RateLeg::Both)
    );
    // EUR 1,000.00 × 136 = ISK 136,000.
    assert_eq!(
        on_friday(100_000, "EUR", "ISK"),
        converted(136_000, FRIDAY, RateLeg::Display)
    );
}

#[test]
fn half_even_both_signs() {
    let half = book(&[("USD", &[(FRIDAY, "0.5")])]);
    let at_half = |minor| convert(minor, currency("EUR"), currency("USD"), FRIDAY, &half);
    // 2.5 goes to the even 2, 3.5 to the even 4, and alike below zero.
    assert_eq!(at_half(5), converted(2, FRIDAY, RateLeg::Display));
    assert_eq!(at_half(7), converted(4, FRIDAY, RateLeg::Display));
    assert_eq!(at_half(-5), converted(-2, FRIDAY, RateLeg::Display));
    assert_eq!(at_half(-7), converted(-4, FRIDAY, RateLeg::Display));
    // 0.5 and 1.5.
    assert_eq!(at_half(1), converted(0, FRIDAY, RateLeg::Display));
    assert_eq!(at_half(3), converted(2, FRIDAY, RateLeg::Display));
    let third = book(&[("USD", &[(FRIDAY, "0.3")])]);
    let at_third = |minor| convert(minor, currency("EUR"), currency("USD"), FRIDAY, &third);
    // 0.3 goes towards zero and 0.6 away from it.
    assert_eq!(at_third(1), converted(0, FRIDAY, RateLeg::Display));
    assert_eq!(at_third(2), converted(1, FRIDAY, RateLeg::Display));
    assert_eq!(at_third(-2), converted(-1, FRIDAY, RateLeg::Display));
}

#[test]
fn same_currency_needs_no_rate() {
    let none = RateBook::default();
    let thb = currency("THB");
    assert_eq!(
        convert(i64::MIN, thb, thb, FRIDAY, &none),
        Conversion::Same { minor: i64::MIN }
    );
}

#[test]
fn euro_is_one_with_no_date() {
    assert_eq!(
        RateBook::default().rate(currency("EUR"), FRIDAY),
        RateLookup::Euro
    );
}

#[test]
fn no_later_rate_is_used() {
    let first = date(1999, 1, 4);
    let book = book(&[("USD", &[(first, "1.1789")])]);
    let eur = currency("EUR");
    let usd = currency("USD");
    assert_eq!(
        convert(100, eur, usd, date(1999, 1, 3), &book),
        Conversion::Missing {
            leg: RateLeg::Display,
        }
    );
    assert_eq!(
        book.rate(usd, first),
        RateLookup::Found(HeldRate {
            on: first,
            rate: rate("1.1789"),
            manual: false,
        })
    );
}

#[test]
fn weekend_uses_friday() {
    let saturday = date(2026, 10, 31);
    assert_eq!(
        convert(
            100_000,
            currency("EUR"),
            currency("USD"),
            saturday,
            &friday_book()
        ),
        converted(111_720, FRIDAY, RateLeg::Display)
    );
}

#[test]
fn cross_rate_reports_the_older_date() {
    let wednesday = date(2026, 10, 28);
    let book = book(&[
        ("USD", &[(FRIDAY, "1.1172")]),
        ("GBP", &[(wednesday, "0.8338")]),
    ]);
    let later = FRIDAY;
    assert_eq!(
        convert(100_000, currency("GBP"), currency("USD"), later, &book),
        converted(133_989, wednesday, RateLeg::Held)
    );
    assert_eq!(
        convert(133_989, currency("USD"), currency("GBP"), later, &book),
        converted(100_000, wednesday, RateLeg::Display)
    );
}

#[test]
fn missing_either_leg_is_missing() {
    let usd_only = book(&[("USD", &[(FRIDAY, "1.1172")])]);
    let usd = currency("USD");
    let thb = currency("THB");
    assert_eq!(
        convert(100, usd, thb, FRIDAY, &usd_only),
        Conversion::Missing {
            leg: RateLeg::Display,
        }
    );
    assert_eq!(
        convert(100, thb, usd, FRIDAY, &usd_only),
        Conversion::Missing { leg: RateLeg::Held }
    );
    assert_eq!(
        convert(100, thb, currency("EUR"), FRIDAY, &usd_only),
        Conversion::Missing { leg: RateLeg::Held }
    );
}

#[test]
fn limits_cannot_overflow() {
    let extremes = book(&[
        ("JPY", &[(FRIDAY, "0.000001")]),
        ("KWD", &[(FRIDAY, "9999999999")]),
    ]);
    let jpy = currency("JPY");
    let kwd = currency("KWD");
    for minor in [i64::MIN, i64::MAX] {
        assert_eq!(
            convert(minor, jpy, kwd, FRIDAY, &extremes),
            Conversion::TooLarge
        );
    }
    // The other way round: 2^63 × 10^-6 / 9,999,999,999 × 10^-3 = 0.92... rounds to 1.
    assert_eq!(
        convert(i64::MIN, kwd, jpy, FRIDAY, &extremes),
        converted(-1, FRIDAY, RateLeg::Both)
    );
    assert_eq!(
        convert(i64::MAX, kwd, jpy, FRIDAY, &extremes),
        converted(1, FRIDAY, RateLeg::Both)
    );
}

/// `i64::MIN` has no opposite, so it is too large in either sign, which keeps a
/// conversion antisymmetric at the very edge.
#[test]
fn the_largest_result_is_the_same_in_both_signs() {
    let one = book(&[("USD", &[(FRIDAY, "1")])]);
    let at_one = |minor| convert(minor, currency("EUR"), currency("USD"), FRIDAY, &one);
    assert_eq!(
        at_one(i64::MAX),
        converted(i64::MAX, FRIDAY, RateLeg::Display)
    );
    assert_eq!(
        at_one(-i64::MAX),
        converted(-i64::MAX, FRIDAY, RateLeg::Display)
    );
    assert_eq!(at_one(i64::MIN), Conversion::TooLarge);
    let two = book(&[("USD", &[(FRIDAY, "2")])]);
    let at_two = |minor| convert(minor, currency("EUR"), currency("USD"), FRIDAY, &two);
    assert_eq!(at_two(1_i64 << 62), Conversion::TooLarge);
    assert_eq!(at_two(-(1_i64 << 62)), Conversion::TooLarge);
}

fn opposite(conversion: Conversion) -> Conversion {
    match conversion {
        Conversion::Same { minor } => Conversion::Same { minor: -minor },
        Conversion::Converted {
            minor,
            rate_on,
            leg,
            manual,
        } => Conversion::Converted {
            minor: -minor,
            rate_on,
            leg,
            manual,
        },
        other => other,
    }
}

/// Only the manual rate of a day counts, the fetched one under it stays behind it, and a
/// later fetched rate takes over: what an admin's rate does and doesn't win.
#[test]
fn a_manual_rate_wins_its_own_day_and_a_later_published_rate_takes_over() {
    let [before, entered, after] = [date(2026, 10, 1), date(2026, 10, 2), date(2026, 10, 5)];
    let observed = |on, text| RateObservation {
        on,
        rate: rate(text),
    };
    let history = RateHistory::merged(
        vec![
            observed(before, "1.1"),
            observed(entered, "1.2"),
            observed(after, "1.3"),
        ],
        vec![observed(entered, "9")],
    );
    let held = |on: Date| history.on_or_before(on).unwrap();

    assert_eq!(
        (held(before).rate, held(before).manual),
        (rate("1.1"), false)
    );
    assert_eq!(
        (held(entered).rate, held(entered).manual),
        (rate("9"), true)
    );
    let carried = held(date(2026, 10, 4));
    assert_eq!(
        (carried.on, carried.manual),
        (entered, true),
        "carried forward"
    );
    assert_eq!((held(after).rate, held(after).manual), (rate("1.3"), false));
    assert_eq!(history.rates().len(), 3, "one rate a day");
}

/// A conversion says which side's rate an admin entered, not just that one did.
#[test]
fn a_conversion_names_the_side_with_a_manual_rate() {
    let on = date(2026, 10, 2);
    let observed = RateObservation {
        on,
        rate: rate("2"),
    };
    let mut book = RateBook::default();
    book.insert(currency("USD"), RateHistory::new(vec![observed]));
    book.insert(
        currency("XAF"),
        RateHistory::merged(Vec::new(), vec![observed]),
    );
    book.insert(
        currency("XOF"),
        RateHistory::merged(Vec::new(), vec![observed]),
    );
    let manual_of =
        |from: &str, to: &str| match convert(100, currency(from), currency(to), on, &book) {
            Conversion::Converted { manual, .. } => manual,
            other => panic!("expected a conversion, got {other:?}"),
        };

    assert_eq!(manual_of("USD", "EUR"), None);
    assert_eq!(manual_of("XAF", "EUR"), Some(RateLeg::Held));
    assert_eq!(manual_of("EUR", "XAF"), Some(RateLeg::Display));
    assert_eq!(manual_of("XAF", "USD"), Some(RateLeg::Held));
    assert_eq!(manual_of("USD", "XAF"), Some(RateLeg::Display));
    assert_eq!(manual_of("XAF", "XOF"), Some(RateLeg::Both));
}

/// Exactly 1.5 times away isn't more than that, on either side.
#[test]
fn a_rate_over_one_and_a_half_times_from_the_nearest_published_one_is_unusual() {
    let on = date(2026, 10, 15);
    let near = RateObservation {
        on: date(2026, 10, 14),
        rate: rate("2"),
    };
    let far = RateObservation {
        on: date(2026, 10, 1),
        rate: rate("100"),
    };
    let unusual = |text| unusual_against(rate(text), on, &[far, near]);

    assert_eq!(unusual("3"), None);
    assert_eq!(unusual("3.000001"), Some(near));
    assert_eq!(unusual("1.333334"), None);
    assert_eq!(unusual("1.333333"), Some(near));
    assert_eq!(unusual("0.5"), Some(near));
    assert_eq!(
        unusual_against(rate("0.5"), on, &[]),
        None,
        "nothing to compare"
    );
    let inverted = RateObservation {
        on,
        rate: rate("1.087"),
    };
    assert_eq!(
        unusual_against(rate("0.92"), on, &[inverted]),
        None,
        "near 1 the wrong way round is only 1.18 times away, and passes"
    );
}

/// Leap days, month ends and the days around them.
fn tricky_date() -> impl Strategy<Value = Date> {
    (
        prop_oneof![
            Just(date(2024, 2, 29)),
            Just(date(2028, 2, 29)),
            Just(date(2026, 2, 28)),
            Just(date(2026, 1, 31)),
            Just(date(2026, 12, 31)),
            Just(date(1999, 1, 4)),
        ],
        -40_i32..40,
    )
        .prop_map(|(base, offset)| base.checked_add(offset.days()).unwrap())
}

proptest! {
    #[test]
    fn conversion_is_antisymmetric(
        minor in (i64::MIN + 1)..=i64::MAX,
        from in sample::select(vec!["EUR", "USD", "JPY", "KWD"]),
        to in sample::select(vec!["EUR", "USD", "JPY", "KWD", "THB"]),
        on in tricky_date(),
    ) {
        let rates = book(&[
            ("USD", &[(date(1999, 1, 4), "1.1789")]),
            ("JPY", &[(date(2000, 1, 1), "175.68")]),
            ("KWD", &[(date(2000, 1, 1), "0.34")]),
        ]);
        let (from, to) = (currency(from), currency(to));
        prop_assert_eq!(
            convert(-minor, from, to, on, &rates),
            opposite(convert(minor, from, to, on, &rates))
        );
    }

    #[test]
    fn a_rate_is_never_from_a_later_day(
        days in collection::vec(tricky_date(), 0..20),
        on in tricky_date(),
    ) {
        let rates: Vec<RateObservation> = days
            .iter()
            .map(|&on| RateObservation { on, rate: rate("1.5") })
            .collect();
        let latest = days.iter().copied().filter(|day| *day <= on).max();
        let found = RateHistory::new(rates).on_or_before(on);
        prop_assert_eq!(found.map(|held| held.on), latest);
        prop_assert!(found.is_none_or(|held| held.on <= on));
    }
}
