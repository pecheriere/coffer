//! A full rebuild in short transactions: other writers get their turn between batches,
//! and nothing they write is lost or shown half done.

use coffer_domain::accounts::BalanceSource;
use coffer_domain::ids::AccountId;
use coffer_domain::net_worth::{DailyValue, NetWorthInterval};
use coffer_domain::policy::AccountScope;
use coffer_store::{RebuildBatch, Store};
use jiff::ToSpan;
use jiff::civil::{Date, date};

use super::{
    batch, cur, dump, fill, flag, fx, holding, mark, oct, open, rebuild, row_on, rows_of, show_in,
    snap, snapshot, state, written_days,
};

/// Three accounts in id order, the order a rebuild takes them in.
async fn three_accounts(store: &Store) -> [AccountId; 3] {
    let mut ids = [
        holding(store, "USD").await,
        holding(store, "EUR").await,
        holding(store, "USD").await,
    ];
    ids.sort_unstable();
    for (id, minor) in ids.iter().zip([100, 200, 300]) {
        snap(store, *id, oct(1), minor).await;
    }
    ids
}

/// A household with rates and a display currency, its table current up to the 5th.
async fn current(store: &Store) -> [AccountId; 3] {
    let ids = three_accounts(store).await;
    fx(store, "USD", &[(oct(1), "2")]).await;
    show_in(store, "USD").await;
    rebuild(store, oct(5)).await;
    ids
}

/// What a rebuild from the sources gives, leaving the table as a test had it finished.
async fn from_the_sources(store: &Store, through: Date) -> Vec<DailyValue> {
    rebuild(store, through).await;
    dump(store).await
}

async fn report_rows(store: &Store) -> (bool, usize) {
    let data = store
        .net_worth_data(&AccountScope::All, oct(1), oct(5), NetWorthInterval::Day)
        .await
        .unwrap();
    (data.state.rebuild_needed, data.rows.len())
}

#[tokio::test]
async fn batches_reach_what_one_rebuild_reaches() {
    let (_dir, store) = open().await;
    current(&store).await;
    flag(&store).await;

    for expected in [false, false, true] {
        assert_eq!(
            batch(&store, oct(5), 1).await,
            RebuildBatch {
                written: 5,
                finished: expected,
            }
        );
        assert_eq!(state(&store).await.rebuild_needed, !expected);
    }

    let batched = dump(&store).await;
    assert_eq!(batched.len(), 15);
    assert_eq!(from_the_sources(&store, oct(5)).await, batched);
}

/// A batch stops before going past its rows, but always gets an account done, or a
/// long history would never finish.
#[tokio::test]
async fn a_batch_holds_whole_accounts_within_its_rows() {
    let (_dir, store) = open().await;
    let [_, _, long] = three_accounts(&store).await;
    snap(&store, long, date(2026, 9, 25), 1).await;

    let wrote = |batch: RebuildBatch| (batch.written, batch.finished);
    assert_eq!(wrote(batch(&store, oct(5), 11).await), (10, false));
    assert_eq!(wrote(batch(&store, oct(5), 11).await), (11, true));
}

/// What a report reads between batches is the same whichever rows are new: none.
#[tokio::test]
async fn a_report_has_no_rows_between_batches() {
    let (_dir, store) = open().await;
    current(&store).await;
    assert_eq!(report_rows(&store).await, (false, 15));
    flag(&store).await;

    assert_eq!(report_rows(&store).await, (true, 0));
    batch(&store, oct(5), 1).await;
    assert_eq!(report_rows(&store).await, (true, 0));
    batch(&store, oct(5), 1).await;
    assert_eq!(report_rows(&store).await, (true, 0));
    batch(&store, oct(5), 1).await;
    assert_eq!(report_rows(&store).await, (false, 15));
}

/// A balance reaches the rows between two batches whether its account's turn has passed
/// or is still to come, and so does an account that didn't exist when they began.
#[tokio::test]
async fn a_balance_written_between_batches_is_kept() {
    let (_dir, store) = open().await;
    let [done, _, waiting] = current(&store).await;
    flag(&store).await;
    batch(&store, oct(5), 1).await;

    snap(&store, done, oct(3), 111).await;
    snap(&store, waiting, oct(3), 333).await;
    let new = holding(&store, "USD").await;
    snap(&store, new, oct(4), 444).await;
    while !batch(&store, oct(5), 1).await.finished {}

    assert_eq!(row_on(&store, done, oct(3)).await.balance_minor, 111);
    assert_eq!(row_on(&store, waiting, oct(3)).await.balance_minor, 333);
    assert_eq!(row_on(&store, new, oct(4)).await.balance_minor, 444);
    let batched = dump(&store).await;
    assert_eq!(from_the_sources(&store, oct(5)).await, batched);
}

/// A rate moves the rows of the accounts done so far at once, and the others when their
/// turn comes.
#[tokio::test]
async fn a_rate_stored_between_batches_is_kept() {
    let (_dir, store) = open().await;
    let [first, ..] = current(&store).await;
    flag(&store).await;
    batch(&store, oct(5), 1).await;

    fx(&store, "USD", &[(oct(3), "4")]).await;
    while !batch(&store, oct(5), 1).await.finished {}

    let batched = dump(&store).await;
    assert!(batched.iter().any(|row| row.rate_on == Some(oct(3))));
    assert_eq!(batched.iter().filter(|row| row.account == first).count(), 5);
    assert_eq!(from_the_sources(&store, oct(5)).await, batched);
}

/// The accounts done so far are in the old currency, so all of them are done again, and
/// a report afterwards finds no row in the wrong units.
#[tokio::test]
async fn a_display_change_between_batches_starts_over() {
    let (_dir, store) = open().await;
    current(&store).await;
    flag(&store).await;
    batch(&store, oct(5), 1).await;
    batch(&store, oct(5), 1).await;

    show_in(&store, "EUR").await;

    assert!(!batch(&store, oct(5), 1).await.finished, "the first again");
    assert!(!batch(&store, oct(5), 1).await.finished);
    assert!(batch(&store, oct(5), 1).await.finished);
    let batched = dump(&store).await;
    assert!(batched.iter().all(|row| row.display == Some(cur("EUR"))));
    assert_eq!(report_rows(&store).await, (false, 15));
    assert_eq!(from_the_sources(&store, oct(5)).await, batched);
}

/// A crash loses at most the batch it interrupts. The rebuild goes on from the last one
/// committed, up to the day it began with, and doesn't do finished accounts again.
#[tokio::test]
async fn a_rebuild_resumes_after_a_crash() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("coffer.db");
    let store = Store::open(&path).await.unwrap();
    let [done, ..] = current(&store).await;
    flag(&store).await;
    batch(&store, oct(4), 1).await;
    mark(&store, done).await;
    let mut interrupted = store.begin().await.unwrap();
    interrupted
        .rebuild_net_worth_batch(oct(4), 1)
        .await
        .unwrap();
    drop(interrupted);
    drop(store);

    let store = Store::open(&path).await.unwrap();
    assert!(state(&store).await.rebuild_needed);
    assert_eq!(
        store.finish_net_worth_rebuild(oct(9)).await.unwrap(),
        Some(8)
    );

    let after = state(&store).await;
    assert_eq!(
        (after.rebuild_needed, after.filled_through),
        (false, Some(oct(4)))
    );
    assert_eq!(written_days(&store, done).await, []);
    assert_eq!(rows_of(&store, done).await.len(), 4);
}

/// A table with no rebuild to run stays as it is, as when another process finished first.
#[tokio::test]
async fn finishing_with_no_rebuild_needed_writes_nothing() {
    let (_dir, store) = open().await;
    let [one, ..] = current(&store).await;
    mark(&store, one).await;

    assert_eq!(
        store.finish_net_worth_rebuild(oct(9)).await.unwrap(),
        Some(0)
    );

    assert_eq!(written_days(&store, one).await, []);
    assert_eq!(state(&store).await.filled_through, Some(oct(5)));
}

const FAR_BACK: Date = date(1970, 1, 1);

/// Two accounts with every day since 1970: more than one transaction rewrites.
async fn long_history(store: &Store, codes: [&str; 2]) -> [AccountId; 2] {
    let ids = [
        holding(store, codes[0]).await,
        holding(store, codes[1]).await,
    ];
    for id in ids {
        snap(store, id, FAR_BACK, 100).await;
    }
    fx(store, "USD", &[(oct(1), "2")]).await;
    show_in(store, "USD").await;
    rebuild(store, oct(5)).await;
    ids
}

/// Too many rows for the change's own transaction: the table says it needs a rebuild
/// rather than hold the lock for them or show the old currency.
#[tokio::test]
async fn a_display_change_too_big_for_one_transaction_asks_for_a_rebuild() {
    let (_dir, store) = open().await;
    let [dollars, _] = long_history(&store, ["USD", "EUR"]).await;
    let before = row_on(&store, dollars, oct(5)).await;
    assert!(!state(&store).await.rebuild_needed);

    show_in(&store, "EUR").await;

    assert!(state(&store).await.rebuild_needed);
    assert_eq!(row_on(&store, dollars, oct(5)).await, before);
    assert_eq!(report_rows(&store).await, (true, 0));

    store.finish_net_worth_rebuild(oct(5)).await.unwrap();
    assert!(!state(&store).await.rebuild_needed);
    assert!(
        dump(&store)
            .await
            .iter()
            .all(|row| row.display == Some(cur("EUR")))
    );
}

/// So does a first fetch of a long history, and yet the day's rate stays in its own.
#[tokio::test]
async fn a_rate_too_big_for_one_transaction_asks_for_a_rebuild() {
    let (_dir, store) = open().await;
    long_history(&store, ["EUR", "EUR"]).await;

    fx(&store, "USD", &[(oct(4), "3")]).await;
    assert!(!state(&store).await.rebuild_needed, "a recent rate");
    assert_eq!(
        dump(&store)
            .await
            .iter()
            .filter(|row| row.rate_on == Some(oct(4)))
            .count(),
        4,
        "written at once, the 4th and the 5th of each"
    );

    fx(&store, "USD", &[(date(1975, 1, 6), "1.1")]).await;
    assert!(state(&store).await.rebuild_needed, "all that history");
    store.finish_net_worth_rebuild(oct(5)).await.unwrap();
    let batched = dump(&store).await;
    assert_eq!(from_the_sources(&store, oct(5)).await, batched);
}

/// One account from 1970 fits, however large the table: a back-dated balance stays with
/// its own change, and the number is right as soon as it commits.
#[tokio::test]
async fn a_back_dated_balance_stays_in_its_own_transaction() {
    let (_dir, store) = open().await;
    let one = holding(&store, "USD").await;
    show_in(&store, "USD").await;
    rebuild(&store, oct(5)).await;

    let mut tx = store.begin().await.unwrap();
    tx.upsert_balance(snapshot(one, FAR_BACK, 7, BalanceSource::Manual))
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert!(!state(&store).await.rebuild_needed);
    assert_eq!(row_on(&store, one, oct(5)).await.balance_minor, 7);
}

/// A rate that moves too many of the rows done so far starts the rebuild over, as they
/// can't all be rewritten in its transaction, and it can't leave them wrong.
#[tokio::test]
async fn a_rate_too_big_for_one_transaction_starts_a_running_rebuild_over() {
    let (_dir, store) = open().await;
    let mut ids = [
        holding(&store, "EUR").await,
        holding(&store, "EUR").await,
        holding(&store, "EUR").await,
    ];
    ids.sort_unstable();
    for id in ids {
        snap(&store, id, FAR_BACK, 100).await;
    }
    show_in(&store, "USD").await;
    assert!(!batch(&store, oct(5), 1).await.finished);
    assert!(!batch(&store, oct(5), 1).await.finished);

    fx(&store, "USD", &[(date(1975, 1, 6), "1.1")]).await;

    assert!(!batch(&store, oct(5), 1).await.finished, "the first again");
    assert!(!batch(&store, oct(5), 1).await.finished);
    assert!(batch(&store, oct(5), 1).await.finished);
    let batched = dump(&store).await;
    assert!(
        batched
            .iter()
            .any(|row| row.rate_on == Some(date(1975, 1, 6)))
    );
    assert_eq!(from_the_sources(&store, oct(5)).await, batched);
}

/// Back to the currency the rows are still in is a change like any other: none of them
/// has to be right by luck, and a second change on the way is no problem.
#[tokio::test]
async fn two_display_changes_in_a_row_end_in_the_last() {
    let (_dir, store) = open().await;
    long_history(&store, ["USD", "EUR"]).await;

    show_in(&store, "EUR").await;
    assert!(state(&store).await.rebuild_needed);
    show_in(&store, "USD").await;
    assert!(state(&store).await.rebuild_needed);
    show_in(&store, "EUR").await;

    store.finish_net_worth_rebuild(oct(5)).await.unwrap();
    assert!(!state(&store).await.rebuild_needed);
    assert!(
        dump(&store)
            .await
            .iter()
            .all(|row| row.display == Some(cur("EUR")))
    );
    assert_eq!(report_rows(&store).await, (false, 10));
}

/// The limit is on the rows counted exactly: 25,000 go in the change's own transaction,
/// and one more doesn't.
#[tokio::test]
async fn the_limit_is_exactly_twenty_five_thousand_rows() {
    let (_dir, store) = open().await;
    let one = holding(&store, "USD").await;
    snap(&store, one, FAR_BACK, 100).await;
    show_in(&store, "USD").await;
    let last = FAR_BACK.checked_add(24_999.days()).unwrap();
    assert_eq!(rebuild(&store, last).await, 25_000);

    show_in(&store, "EUR").await;
    assert!(!state(&store).await.rebuild_needed, "25,000 rows");
    assert_eq!(
        row_on(&store, one, last).await.display,
        Some(cur("EUR")),
        "written with the change"
    );

    let past = last.tomorrow().unwrap();
    assert_eq!(fill(&store, past).await, 1);
    show_in(&store, "USD").await;
    assert!(state(&store).await.rebuild_needed, "25,001 rows");
    assert_eq!(row_on(&store, one, past).await.display, Some(cur("EUR")));
}

/// Days after a long downtime stop at the limit, and the table can answer in between,
/// up to the day it has reached.
#[tokio::test]
async fn a_fill_of_many_days_takes_several_transactions() {
    let (_dir, store) = open().await;
    let ids = [holding(&store, "USD").await, holding(&store, "USD").await];
    for id in ids {
        snap(&store, id, FAR_BACK, 100).await;
    }
    rebuild(&store, FAR_BACK).await;
    let reached = FAR_BACK.checked_add(12_500.days()).unwrap();
    let wanted = FAR_BACK.checked_add(13_000.days()).unwrap();

    let mut tx = store.begin().await.unwrap();
    assert_eq!(tx.fill_net_worth(wanted).await.unwrap(), 25_000);
    tx.commit().await.unwrap();

    let after = state(&store).await;
    assert_eq!(
        (after.rebuild_needed, after.filled_through),
        (false, Some(reached))
    );

    assert_eq!(fill(&store, wanted).await, 1_000);
    assert_eq!(state(&store).await.filled_through, Some(wanted));
    let filled = dump(&store).await;
    assert_eq!(from_the_sources(&store, wanted).await, filled);
}
