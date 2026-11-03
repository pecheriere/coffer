//! Who runs a rebuild: one at a time, or their gaps fill each other and a writer has to
//! win a race for every one. And one that died must not stop the next.

use std::time::{Duration, Instant};

use coffer_domain::ids::AccountId;
use coffer_store::Store;
use jiff::civil::{Date, date};

use super::{dump, flag, holding, oct, open, rebuild, show_in, snap, state};

/// Two accounts with every day since 1970: two batches, with a gap between them.
async fn two_batches(store: &Store) -> [AccountId; 2] {
    let ids = [holding(store, "USD").await, holding(store, "USD").await];
    for id in ids {
        snap(store, id, date(1970, 1, 1), 100).await;
    }
    show_in(store, "USD").await;
    ids
}

async fn small_table(store: &Store) -> Date {
    let one = holding(store, "USD").await;
    snap(store, one, oct(1), 100).await;
    rebuild(store, oct(5)).await;
    flag(store).await;
    oct(5)
}

/// With a runner at work, another leaves the rebuild alone and comes back later.
#[tokio::test]
async fn a_second_runner_leaves_it_to_the_first() {
    let (_dir, store) = open().await;
    let through = small_table(&store).await;
    let rows = dump(&store).await;
    let held = store.hold_net_worth_rebuild_for_tests().unwrap().unwrap();

    assert_eq!(store.finish_net_worth_rebuild(through).await.unwrap(), None);
    assert!(state(&store).await.rebuild_needed);
    assert_eq!(dump(&store).await, rows);

    drop(held);
    assert_eq!(
        store.finish_net_worth_rebuild(through).await.unwrap(),
        Some(5)
    );
    assert!(!state(&store).await.rebuild_needed);
}

/// The server and `coffer rebuild-net-worth` are two processes, each with its own store.
#[tokio::test]
async fn another_store_on_the_database_is_a_second_runner() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("coffer.db");
    let server = Store::open(&path).await.unwrap();
    let command = Store::open(&path).await.unwrap();
    let through = small_table(&server).await;
    let held = command.hold_net_worth_rebuild_for_tests().unwrap().unwrap();

    assert_eq!(
        server.finish_net_worth_rebuild(through).await.unwrap(),
        None
    );

    drop(held);
    assert_eq!(
        server.finish_net_worth_rebuild(through).await.unwrap(),
        Some(5)
    );
}

/// A full rebuild was asked for by someone who wants it done: it waits its turn.
#[tokio::test]
async fn a_full_rebuild_waits_for_the_runner() {
    let (_dir, store) = open().await;
    let through = small_table(&store).await;
    let held = store.hold_net_worth_rebuild_for_tests().unwrap().unwrap();

    let waiting = store.rebuild_net_worth(through);
    tokio::pin!(waiting);
    tokio::select! {
        _ = &mut waiting => panic!("it did not wait"),
        () = tokio::time::sleep(Duration::from_millis(300)) => {}
    }
    assert!(state(&store).await.rebuild_needed);
    drop(held);

    assert_eq!(waiting.await.unwrap(), 5);
    assert!(!state(&store).await.rebuild_needed);
}

/// What a dead runner leaves is its file and its progress. Neither holds anyone up, and
/// the next one goes on. (A killed process drops its lock the same way as this one's.)
#[tokio::test]
async fn a_dead_runner_blocks_nobody() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("coffer.db");
    let store = Store::open(&path).await.unwrap();
    let through = small_table(&store).await;
    let held = store.hold_net_worth_rebuild_for_tests().unwrap().unwrap();
    drop(held);
    drop(store);
    assert!(dir.path().join("coffer.db.rebuild-lock").exists());

    let store = Store::open(&path).await.unwrap();
    assert_eq!(
        store.finish_net_worth_rebuild(through).await.unwrap(),
        Some(5)
    );
}

/// Asked at once, one runs it and the other takes no part, so its batches keep their gap
/// and the row count it reports is every row.
#[tokio::test]
async fn two_runners_keep_the_gap_and_the_count() {
    let (_dir, store) = open().await;
    two_batches(&store).await;
    let through = oct(5);
    let started = Instant::now();

    let (full, other) = tokio::join!(
        store.rebuild_net_worth(through),
        store.finish_net_worth_rebuild(through),
    );

    let rows = dump(&store).await;
    assert_eq!(full.unwrap(), u64::try_from(rows.len()).unwrap());
    assert_eq!(other.unwrap(), None, "the first to ask runs it");
    assert!(started.elapsed() >= Duration::from_millis(150));
    assert!(!state(&store).await.rebuild_needed);
}
