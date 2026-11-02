use jiff::civil::date;
use jiff::tz::TimeZone;

use coffer_domain::sync::SyncRunStatus;

use super::CONNECTION_REMOVED;
use super::fetch::{days_before, window_start};
use super::results::{AccountOutcome, Totals, joined_errors, outcome};
use crate::sync::apply::ApplyReport;

/// A run whose connection is removed after some accounts were applied
/// is partial and says so; one removed before any is failed.
#[test]
fn removal_mid_run_outcomes() {
    let mut totals = Totals::default();
    assert_eq!(
        outcome(&totals, true, false),
        (SyncRunStatus::Failed, CONNECTION_REMOVED.to_owned())
    );
    totals.add(&AccountOutcome::Applied(ApplyReport {
        created: 2,
        ..ApplyReport::default()
    }));
    totals.add(&AccountOutcome::ConnectionRemoved);
    let (status, detail) = outcome(&totals, true, false);
    assert_eq!(status, SyncRunStatus::Partial);
    assert_eq!(
        detail,
        "1 account, 0 with errors; 2 created, 0 attached, 0 updated, 0 deleted, 0 held, \
         0 skipped; the connection is removed"
    );
    assert_eq!(outcome(&totals, false, false).0, SyncRunStatus::Succeeded);
    assert_eq!(outcome(&totals, false, true).0, SyncRunStatus::Partial);
    totals.add(&AccountOutcome::Failed {
        refetch: false,
        passed: 0,
    });
    assert!(
        totals
            .detail(false)
            .starts_with("2 accounts, 1 with errors;"),
        "more than one account is plural"
    );
}

/// The bridge refuses a window of more than 90 days. In every time zone,
/// at every moment of a day, the first window of `SimpleFin` starts less
/// than 90 days before the request. The last second of a day is the worst
/// case: the window starts at midnight, and a clock change inside the
/// window makes its days longer than 24 hours.
#[test]
fn simplefin_first_window_is_within_the_bridge_limit_in_every_time_zone() {
    use crate::egress::{Egress, HostAllowList};
    use crate::sync::provider::{FetchMode, Provider as _};
    use crate::sync::simplefin::SimpleFin;

    let FetchMode::Window { max_days, .. } =
        SimpleFin::new(Egress::for_tests(), HostAllowList::none())
            .capabilities()
            .mode
    else {
        panic!("SimpleFin must fetch by window");
    };
    let first: jiff::Timestamp = "2026-06-01T00:00:00Z".parse().unwrap();
    let mut worst = 0;
    for name in jiff::tz::db().available() {
        let timezone = TimeZone::get(name.as_str()).unwrap();
        for day in 0..800 {
            let midnight = first
                .to_zoned(timezone.clone())
                .date()
                .checked_add(jiff::Span::new().days(day + 1))
                .unwrap()
                .to_zoned(timezone.clone())
                .unwrap();
            let now = midnight
                .timestamp()
                .checked_sub(jiff::Span::new().seconds(1))
                .unwrap();
            let today = now.to_zoned(timezone.clone()).date();
            let start = window_start(&[], None, today, &timezone, max_days, 14)
                .to_zoned(timezone.clone())
                .unwrap()
                .timestamp();
            worst = worst.max(now.as_second() - start.as_second());
        }
    }
    assert!(
        worst < 90 * 24 * 60 * 60,
        "the first window must start less than 90 days before the request in every time zone"
    );
}

#[test]
fn window_rules() {
    let utc = TimeZone::UTC;
    let today = date(2026, 10, 31);
    assert_eq!(
        window_start(&[], None, today, &utc, 90, 14),
        date(2026, 8, 2)
    );
    let yesterday: jiff::Timestamp = "2026-10-30T23:00:00Z".parse().unwrap();
    assert_eq!(
        window_start(&[], Some(yesterday), today, &utc, 90, 14),
        date(2026, 10, 16)
    );
    // The day of the last success is the household's day.
    let auckland = TimeZone::get("Pacific/Auckland").unwrap();
    assert_eq!(
        window_start(&[], Some(yesterday), today, &auckland, 90, 14),
        date(2026, 10, 17)
    );
    let long_ago: jiff::Timestamp = "2025-01-01T00:00:00Z".parse().unwrap();
    assert_eq!(
        window_start(&[], Some(long_ago), today, &utc, 90, 14),
        date(2026, 8, 2),
        "clamped after a long gap"
    );
    // With accounts seen within the window, the one applied longest ago
    // decides, and one never applied gets the maximum window.
    let week_ago: jiff::Timestamp = "2026-10-24T12:00:00Z".parse().unwrap();
    assert_eq!(
        window_start(
            &[(yesterday, Some(yesterday)), (yesterday, Some(week_ago))],
            Some(yesterday),
            today,
            &utc,
            90,
            14
        ),
        date(2026, 10, 10)
    );
    assert_eq!(
        window_start(
            &[(yesterday, Some(yesterday)), (yesterday, None)],
            Some(yesterday),
            today,
            &utc,
            90,
            14
        ),
        date(2026, 8, 2)
    );
    assert_eq!(
        window_start(
            &[(yesterday, Some(long_ago))],
            Some(yesterday),
            today,
            &utc,
            90,
            14
        ),
        date(2026, 8, 2),
        "clamped for an account applied long ago"
    );
    // An account left out of recent answers still counts, so its missing
    // days are fetched when it returns; one not seen within the window
    // (a closed bank account) does not hold every fetch at the maximum.
    let month_ago: jiff::Timestamp = "2026-10-01T12:00:00Z".parse().unwrap();
    assert_eq!(
        window_start(
            &[(yesterday, Some(yesterday)), (month_ago, Some(month_ago))],
            Some(yesterday),
            today,
            &utc,
            90,
            14
        ),
        date(2026, 9, 17)
    );
    assert_eq!(
        window_start(&[(long_ago, None)], Some(yesterday), today, &utc, 90, 14),
        date(2026, 10, 16),
        "an account not seen within the window is left out"
    );
    assert_eq!(days_before(today, -3), today);
    assert_eq!(days_before(today, i64::MAX), jiff::civil::Date::MIN);
}

#[test]
fn connection_errors_join_sanitized() {
    assert_eq!(joined_errors(&[]), None);
    assert_eq!(joined_errors(&["\u{7}".into()]), None);
    assert_eq!(
        joined_errors(&["a https://u:p@h.example".into(), "b".into()]).as_deref(),
        Some("a https://***@h.example; b")
    );
    let long = joined_errors(&["x".repeat(400), "y".repeat(400)]).unwrap();
    assert_eq!(long.chars().count(), 500);
}
