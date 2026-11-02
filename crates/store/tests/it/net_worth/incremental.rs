//! Each change rewrites exactly the rows it moves, inside its own transaction.

use std::path::Path;

use coffer_domain::accounts::BalanceSource;
use coffer_domain::fx::RateLeg;
use coffer_domain::net_worth::ConversionState;
use jiff::civil::date;

use super::{
    at, bring_in, connection, cur, days, dump, fill, fx, holding, kept, mark, observations, oct,
    open, rebuild, row_on, rows_of, show_in, snap, snapshot, state, written_days,
};

#[tokio::test]
async fn a_new_snapshot_rebuilds_up_to_the_next() {
    let (_dir, store) = open().await;
    let one = holding(&store, "USD").await;
    snap(&store, one, oct(1), 100).await;
    snap(&store, one, oct(20), 200).await;
    rebuild(&store, oct(31)).await;
    mark(&store, one).await;

    snap(&store, one, oct(10), 150).await;

    assert_eq!(written_days(&store, one).await, days(10, 19));
    let row = row_on(&store, one, oct(19)).await;
    assert_eq!((row.balance_minor, row.balance_on), (150, oct(10)));
}

#[tokio::test]
async fn a_same_day_overwrite_changes_that_stretch() {
    let (_dir, store) = open().await;
    let one = holding(&store, "USD").await;
    for (day, minor) in [(1, 100), (10, 150), (20, 200)] {
        snap(&store, one, oct(day), minor).await;
    }
    rebuild(&store, oct(31)).await;
    mark(&store, one).await;

    snap(&store, one, oct(10), 175).await;

    assert_eq!(written_days(&store, one).await, days(10, 19));
    assert_eq!(row_on(&store, one, oct(10)).await.balance_minor, 175);
}

#[tokio::test]
async fn a_changed_source_alone_changes_that_stretch() {
    let (_dir, store) = open().await;
    let one = holding(&store, "USD").await;
    snap(&store, one, oct(1), 100).await;
    rebuild(&store, oct(3)).await;

    let mut tx = store.begin().await.unwrap();
    tx.upsert_balance(snapshot(one, oct(1), 100, BalanceSource::Sync))
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(
        row_on(&store, one, oct(3)).await.balance_source,
        BalanceSource::Sync
    );
}

#[tokio::test]
async fn the_last_snapshot_s_stretch_runs_to_through() {
    let (_dir, store) = open().await;
    let one = holding(&store, "USD").await;
    snap(&store, one, oct(1), 100).await;
    rebuild(&store, oct(10)).await;
    mark(&store, one).await;

    snap(&store, one, oct(5), 150).await;

    assert_eq!(written_days(&store, one).await, days(5, 10));
}

/// The next snapshot is after the last day filled: the stretch stops at that day.
#[tokio::test]
async fn a_stretch_stops_at_through() {
    let (_dir, store) = open().await;
    let one = holding(&store, "USD").await;
    snap(&store, one, oct(1), 100).await;
    snap(&store, one, oct(20), 200).await;
    rebuild(&store, oct(10)).await;

    snap(&store, one, oct(5), 150).await;

    assert_eq!(rows_of(&store, one).await.last().unwrap().on, oct(10));
}

/// A snapshot after the last day filled moves no row.
#[tokio::test]
async fn a_snapshot_after_through_writes_no_row() {
    let (_dir, store) = open().await;
    let one = holding(&store, "USD").await;
    snap(&store, one, oct(1), 100).await;
    rebuild(&store, oct(10)).await;
    mark(&store, one).await;

    snap(&store, one, oct(12), 150).await;

    assert_eq!(written_days(&store, one).await, []);
    assert_eq!(rows_of(&store, one).await.len(), 10);
}

#[tokio::test]
async fn a_back_dated_first_snapshot_makes_earlier_rows() {
    let (_dir, store) = open().await;
    let one = holding(&store, "USD").await;
    snap(&store, one, oct(10), 100).await;
    rebuild(&store, oct(15)).await;
    assert_eq!(rows_of(&store, one).await[0].on, oct(10));
    mark(&store, one).await;

    snap(&store, one, oct(5), 50).await;

    assert_eq!(written_days(&store, one).await, days(5, 9));
    assert_eq!(row_on(&store, one, oct(5)).await.balance_minor, 50);
}

#[tokio::test]
async fn an_unchanged_snapshot_writes_no_row() {
    let (_dir, store) = open().await;
    let one = holding(&store, "USD").await;
    snap(&store, one, oct(1), 100).await;
    rebuild(&store, oct(5)).await;
    mark(&store, one).await;

    snap(&store, one, oct(1), 100).await;

    assert_eq!(written_days(&store, one).await, []);
}

#[tokio::test]
async fn a_future_snapshot_has_no_row_until_its_day() {
    let (_dir, store) = open().await;
    let one = holding(&store, "USD").await;
    snap(&store, one, oct(1), 100).await;
    rebuild(&store, oct(15)).await;

    snap(&store, one, oct(20), 200).await;

    let rows = rows_of(&store, one).await;
    assert_eq!(rows.last().unwrap().on, oct(15));
    assert_eq!(rows.last().unwrap().balance_minor, 100);

    fill(&store, oct(22)).await;
    assert_eq!(row_on(&store, one, oct(19)).await.balance_minor, 100);
    assert_eq!(row_on(&store, one, oct(20)).await.balance_minor, 200);
    assert_eq!(row_on(&store, one, oct(22)).await.balance_minor, 200);
}

#[tokio::test]
async fn rows_start_no_earlier_than_1970() {
    let (_dir, store) = open().await;
    let one = holding(&store, "USD").await;
    rebuild(&store, date(1970, 1, 3)).await;

    snap(&store, one, date(1, 1, 1), 100).await;

    let rows = rows_of(&store, one).await;
    assert_eq!(
        rows.iter().map(|row| row.on).collect::<Vec<_>>(),
        [date(1970, 1, 1), date(1970, 1, 2), date(1970, 1, 3)]
    );
    assert_eq!(rows[0].balance_on, date(1, 1, 1));
}

/// Three accounts, in the display currency and two others, each with a rate from
/// the 1st, rebuilt through the 20th and marked.
async fn three_currencies() -> (
    tempfile::TempDir,
    coffer_store::Store,
    [coffer_domain::ids::AccountId; 3],
) {
    let (dir, store) = open().await;
    let usd = holding(&store, "USD").await;
    let jpy = holding(&store, "JPY").await;
    let gbp = holding(&store, "GBP").await;
    for one in [usd, jpy, gbp] {
        snap(&store, one, oct(1), 1_000).await;
    }
    fx(&store, "USD", &[(oct(1), "1.1")]).await;
    fx(&store, "JPY", &[(oct(1), "170")]).await;
    fx(&store, "GBP", &[(oct(1), "0.8")]).await;
    show_in(&store, "USD").await;
    rebuild(&store, oct(20)).await;
    for one in [usd, jpy, gbp] {
        mark(&store, one).await;
    }
    (dir, store, [usd, jpy, gbp])
}

#[tokio::test]
async fn a_rate_rebuilds_its_currency_from_that_date() {
    let (_dir, store, [usd, jpy, gbp]) = three_currencies().await;

    fx(&store, "JPY", &[(oct(10), "180")]).await;

    assert_eq!(written_days(&store, jpy).await, days(10, 20));
    assert_eq!(row_on(&store, jpy, oct(10)).await.rate_on, Some(oct(1)));
    assert_eq!(written_days(&store, usd).await, []);
    assert_eq!(written_days(&store, gbp).await, []);
}

#[tokio::test]
async fn a_rate_of_the_display_currency_rebuilds_the_others() {
    let (_dir, store, [usd, jpy, gbp]) = three_currencies().await;

    fx(&store, "USD", &[(oct(10), "1.2")]).await;

    assert_eq!(written_days(&store, jpy).await, days(10, 20));
    assert_eq!(written_days(&store, gbp).await, days(10, 20));
    assert_eq!(written_days(&store, usd).await, []);
}

#[tokio::test]
async fn an_unchanged_rate_writes_no_row() {
    let (_dir, store, [_usd, jpy, _gbp]) = three_currencies().await;

    fx(&store, "JPY", &[(oct(1), "170")]).await;

    assert_eq!(written_days(&store, jpy).await, []);
}

const FRIDAY: jiff::civil::Date = date(2026, 10, 30);
const SATURDAY: jiff::civil::Date = date(2026, 10, 31);
const SUNDAY: jiff::civil::Date = date(2026, 11, 1);
const MONDAY: jiff::civil::Date = date(2026, 11, 2);

#[tokio::test]
async fn a_new_rate_fills_a_weekend() {
    let (_dir, store) = open().await;
    let livret = holding(&store, "EUR").await;
    snap(&store, livret, FRIDAY, 100_000).await;
    fx(&store, "USD", &[(FRIDAY, "1.1172")]).await;
    show_in(&store, "USD").await;
    rebuild(&store, MONDAY).await;
    assert_eq!(row_on(&store, livret, SUNDAY).await.rate_on, Some(FRIDAY));
    mark(&store, livret).await;

    fx(&store, "USD", &[(MONDAY, "1.2")]).await;

    assert_eq!(written_days(&store, livret).await, [MONDAY]);
    let monday = row_on(&store, livret, MONDAY).await;
    assert_eq!(
        (monday.display_minor, monday.rate_on),
        (Some(120_000), Some(MONDAY))
    );
}

#[tokio::test]
async fn a_correction_reaches_days_carried_from_it() {
    let (_dir, store) = open().await;
    let livret = holding(&store, "EUR").await;
    snap(&store, livret, FRIDAY, 100_000).await;
    fx(&store, "USD", &[(FRIDAY, "1.1172")]).await;
    show_in(&store, "USD").await;
    rebuild(&store, SUNDAY).await;

    fx(&store, "USD", &[(FRIDAY, "1.2")]).await;

    for day in [FRIDAY, SATURDAY, SUNDAY] {
        assert_eq!(
            row_on(&store, livret, day).await.display_minor,
            Some(120_000)
        );
    }
}

#[tokio::test]
async fn a_new_display_currency_rebuilds_everything_in_its_transaction() {
    let (_dir, store) = open().await;
    let dollars = holding(&store, "USD").await;
    let euros = holding(&store, "EUR").await;
    snap(&store, dollars, oct(1), 1_000).await;
    snap(&store, euros, oct(1), 1_000).await;
    fx(&store, "USD", &[(oct(1), "2")]).await;
    show_in(&store, "USD").await;
    rebuild(&store, oct(3)).await;
    let before = dump(&store).await;

    let mut tx = store.begin().await.unwrap();
    assert_eq!(
        tx.set_display_currency(cur("EUR")).await.unwrap(),
        Some(cur("USD"))
    );
    drop(tx);
    assert_eq!(dump(&store).await, before, "rolled back: rows unchanged");

    show_in(&store, "EUR").await;
    let after = dump(&store).await;
    assert_eq!(after.len(), before.len());
    assert!(after.iter().all(|row| row.display == Some(cur("EUR"))));
    let dollars_row = row_on(&store, dollars, oct(1)).await;
    assert_eq!(dollars_row.display_minor, Some(500));
    assert_eq!(dollars_row.rate_leg, Some(RateLeg::Held));
    assert_eq!(
        row_on(&store, euros, oct(1)).await.conversion,
        ConversionState::Same
    );
}

#[tokio::test]
async fn the_same_display_currency_writes_no_row() {
    let (_dir, store) = open().await;
    let one = holding(&store, "USD").await;
    snap(&store, one, oct(1), 1_000).await;
    show_in(&store, "USD").await;
    rebuild(&store, oct(3)).await;
    mark(&store, one).await;

    show_in(&store, "USD").await;

    assert_eq!(written_days(&store, one).await, []);
}

/// Everything a dropped transaction did is gone, of the table as of its state.
#[tokio::test]
async fn a_failed_change_leaves_rows_as_they_were() {
    let (_dir, store) = open().await;
    let bank = connection(&store).await;
    let one = holding(&store, "JPY").await;
    let feed = kept(&store, bank, "feed", &[(oct(2), 7, "JPY")]).await;
    snap(&store, one, oct(1), 1_000).await;
    fx(&store, "USD", &[(oct(1), "1.1")]).await;
    fx(&store, "JPY", &[(oct(1), "170")]).await;
    show_in(&store, "USD").await;
    rebuild(&store, oct(5)).await;
    let (rows, table_state) = (dump(&store).await, state(&store).await);

    let mut tx = store.begin().await.unwrap();
    tx.upsert_balance(snapshot(one, oct(2), 5, BalanceSource::Manual))
        .await
        .unwrap();
    drop(tx);
    assert_eq!(dump(&store).await, rows, "a snapshot");

    let mut tx = store.begin().await.unwrap();
    tx.upsert_fx_rates(
        cur("JPY"),
        coffer_domain::fx::FxSource::Ecb,
        &observations(&[(oct(2), "180")]),
        at(),
    )
    .await
    .unwrap();
    drop(tx);
    assert_eq!(dump(&store).await, rows, "a rate");

    let mut tx = store.begin().await.unwrap();
    tx.set_display_currency(cur("EUR")).await.unwrap();
    drop(tx);
    assert_eq!(dump(&store).await, rows, "a display currency");

    let mut tx = store.begin().await.unwrap();
    assert_eq!(
        tx.bring_in_kept_balances(feed, one, cur("JPY"))
            .await
            .unwrap()
            .written,
        1
    );
    drop(tx);
    assert_eq!(dump(&store).await, rows, "a bind");

    let mut tx = store.begin().await.unwrap();
    tx.fill_net_worth(oct(9)).await.unwrap();
    drop(tx);
    assert_eq!(dump(&store).await, rows, "a fill");
    assert_eq!(state(&store).await, table_state, "a fill");

    let mut tx = store.begin().await.unwrap();
    tx.rebuild_net_worth(oct(9)).await.unwrap();
    drop(tx);
    assert_eq!(dump(&store).await, rows, "a rebuild");
    assert_eq!(state(&store).await, table_state, "a rebuild");
}

#[tokio::test]
async fn a_bind_s_kept_balances_rebuild_the_account() {
    let (_dir, store) = open().await;
    let bank = connection(&store).await;
    let one = holding(&store, "USD").await;
    snap(&store, one, oct(5), 500).await;
    rebuild(&store, oct(10)).await;
    let feed = kept(
        &store,
        bank,
        "feed",
        &[
            (oct(1), 10, "USD"),
            (oct(2), 20, "USD"),
            (oct(3), 30, "USD"),
            (oct(5), 55, "USD"),
            (oct(8), 80, "USD"),
        ],
    )
    .await;

    assert_eq!(bring_in(&store, feed, one, "USD").await, 4);

    let balances: Vec<_> = rows_of(&store, one)
        .await
        .iter()
        .map(|row| (row.on, row.balance_minor, row.balance_source))
        .collect();
    let (sync, manual) = (BalanceSource::Sync, BalanceSource::Manual);
    assert_eq!(
        balances,
        [
            (oct(1), 10, sync),
            (oct(2), 20, sync),
            (oct(3), 30, sync),
            (oct(4), 30, sync),
            (oct(5), 500, manual),
            (oct(6), 500, manual),
            (oct(7), 500, manual),
            (oct(8), 80, sync),
            (oct(9), 80, sync),
            (oct(10), 80, sync),
        ]
    );
}

#[tokio::test]
async fn a_kept_balance_in_another_currency_changes_nothing() {
    let (_dir, store) = open().await;
    let bank = connection(&store).await;
    let one = holding(&store, "USD").await;
    snap(&store, one, oct(5), 500).await;
    rebuild(&store, oct(10)).await;
    let feed = kept(&store, bank, "feed", &[(oct(1), 10, "EUR")]).await;
    mark(&store, one).await;

    assert_eq!(bring_in(&store, feed, one, "USD").await, 0);

    assert_eq!(written_days(&store, one).await, []);
    assert_eq!(rows_of(&store, one).await[0].on, oct(5));
}

#[tokio::test]
async fn a_bind_with_nothing_kept_touches_no_row() {
    let (_dir, store) = open().await;
    let bank = connection(&store).await;
    let one = holding(&store, "USD").await;
    snap(&store, one, oct(5), 500).await;
    rebuild(&store, oct(10)).await;
    let feed = kept(&store, bank, "feed", &[]).await;
    mark(&store, one).await;

    assert_eq!(bring_in(&store, feed, one, "USD").await, 0);

    assert_eq!(written_days(&store, one).await, []);
}

#[tokio::test]
async fn a_bind_s_days_after_through_wait_for_the_fill() {
    let (_dir, store) = open().await;
    let bank = connection(&store).await;
    let one = holding(&store, "USD").await;
    snap(&store, one, oct(5), 500).await;
    rebuild(&store, oct(10)).await;
    let feed = kept(&store, bank, "feed", &[(oct(12), 120, "USD")]).await;
    mark(&store, one).await;

    assert_eq!(bring_in(&store, feed, one, "USD").await, 1);
    assert_eq!(written_days(&store, one).await, []);
    assert_eq!(rows_of(&store, one).await.last().unwrap().on, oct(10));

    fill(&store, oct(13)).await;
    assert_eq!(row_on(&store, one, oct(11)).await.balance_minor, 500);
    assert_eq!(row_on(&store, one, oct(12)).await.balance_minor, 120);
    assert_eq!(row_on(&store, one, oct(13)).await.balance_minor, 120);
}

/// How often `needle`, in lower case, appears in each file under
/// `crates/store/src` holding it, by path: whatever the case, and whatever spaces,
/// line breaks and line continuations lie between its words.
fn counts_of(needle: &str) -> Vec<(String, usize)> {
    fn walk(dir: &Path, root: &Path, needle: &str, found: &mut Vec<(String, usize)>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, root, needle, found);
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap().to_lowercase();
            let words = source.replace('\\', " ");
            let count = words
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .matches(needle)
                .count();
            if count > 0 {
                let name = path.strip_prefix(root).unwrap().display().to_string();
                found.push((name, count));
            }
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = Vec::new();
    walk(&root, &root, needle, &mut found);
    found.sort();
    found
}

/// A snapshot written anywhere else would leave the table stale until the next
/// rebuild: a new writer fails here until it refreshes the table. Each file's
/// count is exact, so a second writer in a known file fails too.
#[test]
fn every_snapshot_writer_is_known() {
    let known = |name: &str, count| (name.to_owned(), count);
    assert_eq!(
        counts_of("into balance_snapshots"),
        [
            known("balances.rs", 1),
            // The bind, and the fixture of its migration test.
            known("kept_balances.rs", 2),
            // The fixture of the migration test.
            known("net_worth/tests.rs", 1),
        ]
    );
    for changing in ["update balance_snapshots", "delete from balance_snapshots"] {
        assert_eq!(counts_of(changing), []);
    }
}

#[tokio::test]
async fn nothing_is_written_while_a_rebuild_is_needed() {
    let (_dir, store) = open().await;
    let bank = connection(&store).await;
    let one = holding(&store, "EUR").await;
    let waiting = state(&store).await;
    assert!(waiting.rebuild_needed);
    assert_eq!(waiting.filled_through, None);

    snap(&store, one, oct(1), 100).await;
    fx(&store, "USD", &[(oct(1), "1.1")]).await;
    show_in(&store, "USD").await;
    let feed = kept(&store, bank, "feed", &[(oct(2), 7, "EUR")]).await;
    bring_in(&store, feed, one, "EUR").await;
    assert_eq!(fill(&store, oct(5)).await, 0);

    assert_eq!(dump(&store).await, []);
    assert_eq!(state(&store).await, waiting);
}

/// A flagged table that holds rows and a day stays as it is too.
#[tokio::test]
async fn a_flagged_table_keeps_its_rows_until_the_rebuild() {
    let (_dir, store) = open().await;
    let one = holding(&store, "USD").await;
    snap(&store, one, oct(1), 100).await;
    rebuild(&store, oct(3)).await;
    store
        .execute_unchecked_for_tests(
            "UPDATE net_worth_state SET rebuild_needed = 1 WHERE filled_through = ?",
            &oct(3).to_string(),
        )
        .await
        .unwrap();
    let (rows, flagged) = (dump(&store).await, state(&store).await);
    assert!(flagged.rebuild_needed);

    snap(&store, one, oct(2), 200).await;
    fill(&store, oct(5)).await;

    assert_eq!(dump(&store).await, rows);
    assert_eq!(state(&store).await, flagged);
}
