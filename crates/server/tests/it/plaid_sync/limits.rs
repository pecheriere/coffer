//! What one sync may hold and how long it may take: the byte cap on a pass,
//! the length limit on ids, and the deadline on a fetch.

use std::sync::Arc;
use std::time::Duration;

use super::{GET_ACCOUNTS, MUTATION, SYNC, World, account_json, accounts_answer, page, record};
use axum::http::StatusCode;
use coffer_domain::sync::SyncRunStatus;
use coffer_domain::time::Clock as _;
use coffer_server::sync::apply::MAX_PROVIDER_ID_CHARS;
use coffer_server::sync::plaid::client::PlaidClient;
use coffer_server::sync::plaid::provider::Plaid;
use coffer_server::sync::provider::{FetchResult, ProviderKind};
use coffer_server::sync::scheduler;
use coffer_store::{ConnectionStatus, SyncRunRecord};
use jiff::SignedDuration;
use serde_json::{Value, json};
use tokio::sync::oneshot;

const TOO_MUCH_DATA: &str = "Plaid sent more data than one sync accepts";
const TOO_SLOW: &str = "the provider did not finish within the time one sync may take";
const TOO_MANY_ACCOUNTS: &str = "Plaid listed more accounts than one sync accepts";
const MAX_ACCOUNTS: usize = 1_000;
const NO_ACCOUNT: &str = "Plaid sent records coffer could not read or assign to an account";
const NO_ACCOUNT_ID: &str = "Plaid sent an account without a usable id";

/// A world whose Plaid Item holds one account, so a run reports on these
/// tests' records alone.
async fn world() -> World {
    let world = super::world().await;
    world.script.always(
        GET_ACCOUNTS,
        StatusCode::OK,
        accounts_answer(&[account_json("acct-checking", "depository", &json!(1))]),
    );
    world
}

/// Replaces the world's Plaid provider with one limited to `bytes` a pass.
fn limit_pass(world: &mut World, bytes: usize) {
    let config = world.server.config.plaid.clone().unwrap();
    world.server.use_provider(Arc::new(
        Plaid::new(PlaidClient::for_tests(&world.mock, config)).with_max_pass_bytes(bytes),
    ));
}

/// `body` grown to exactly `size` bytes; JSON allows the trailing whitespace.
fn padded(body: &str, size: usize) -> String {
    format!("{body}{}", " ".repeat(size - body.len()))
}

fn over_long_id() -> String {
    "i".repeat(MAX_PROVIDER_ID_CHARS + 1)
}

/// A record with one of its fields replaced.
fn with_field(id: &str, field: &str, value: &str) -> Value {
    let mut record = record(id, "acct-checking", "1");
    record[field] = json!(value);
    record
}

/// A first sync that saves the cursor `saved` and stores one record.
async fn first_sync(world: &World) {
    world.script.page(page(
        &[record("before", "acct-checking", "1")],
        &[],
        &[],
        "saved",
        false,
    ));
    let run = world.sync().await;
    assert_eq!(run.status, SyncRunStatus::Succeeded, "the first sync works");
}

/// A few big pages, far below the record cap, go over the byte cap: the fetch
/// stops there as a provider failure, with nothing applied and the cursor kept.
#[tokio::test]
async fn big_pages_below_the_record_cap_fail_the_fetch() {
    let mut world = world().await;
    let checking = world.bound("acct-checking").await;
    first_sync(&world).await;
    limit_pass(&mut world, 64 * 1024);
    for (id, more) in [("big-1", true), ("big-2", true), ("never-read", false)] {
        let body = page(&[record(id, "acct-checking", "2")], &[], &[], id, more);
        world.script.page(padded(&body, 40 * 1024));
    }
    let before = world.script.cursors().len();

    let run = world.sync().await;

    assert_eq!(run.status, SyncRunStatus::Failed);
    assert_eq!(run.detail.as_deref(), Some(TOO_MUCH_DATA));
    assert_eq!(
        world.script.cursors().len() - before,
        2,
        "no page is asked for once the cap is passed"
    );
    let connection = world.connection().await;
    assert_eq!(connection.provider_state.as_deref(), Some("saved"));
    assert_eq!(connection.status, ConnectionStatus::Error);
    assert_eq!(connection.last_error.as_deref(), Some(TOO_MUCH_DATA));
    let stored = world.transactions(checking).await;
    assert_eq!(stored.len(), 1, "nothing of the failed fetch is applied");
    assert_eq!(stored[0].sources[0].provider_txn_id, "before");
}

/// A pass of exactly the cap is accepted, and one byte more is not.
#[tokio::test]
async fn the_byte_cap_is_exact() {
    let mut world = world().await;
    let checking = world.bound("acct-checking").await;
    let first = page(&[record("a", "acct-checking", "1")], &[], &[], "c1", true);
    let last = page(&[record("b", "acct-checking", "1")], &[], &[], "c2", false);
    let size = first.len() + last.len();

    limit_pass(&mut world, size - 1);
    world.script.page(first.clone());
    world.script.page(last.clone());
    let run = world.sync().await;
    assert_eq!(run.detail.as_deref(), Some(TOO_MUCH_DATA));
    assert_eq!(world.transactions(checking).await.len(), 0);

    limit_pass(&mut world, size);
    world.script.page(first);
    world.script.page(last);
    let run = world.sync().await;
    assert_eq!(run.status, SyncRunStatus::Succeeded);
    assert_eq!(world.transactions(checking).await.len(), 2);
    assert_eq!(
        world.connection().await.provider_state.as_deref(),
        Some("c2")
    );
}

/// Nothing of an abandoned pass is held, so a restart counts from zero.
#[tokio::test]
async fn a_restart_counts_its_bytes_from_zero() {
    let mut world = world().await;
    let checking = world.bound("acct-checking").await;
    limit_pass(&mut world, 64 * 1024);
    let abandoned = page(
        &[record("old", "acct-checking", "1")],
        &[],
        &[],
        "stale",
        true,
    );
    world.script.page(padded(&abandoned, 40 * 1024));
    world.script.reply(SYNC, StatusCode::BAD_REQUEST, MUTATION);
    let kept = page(
        &[record("new", "acct-checking", "1")],
        &[],
        &[],
        "done",
        false,
    );
    world.script.page(padded(&kept, 40 * 1024));

    let run = world.sync().await;

    assert_eq!(run.status, SyncRunStatus::Succeeded);
    let stored = world.transactions(checking).await;
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].sources[0].provider_txn_id, "new");
    assert_eq!(
        world.connection().await.provider_state.as_deref(),
        Some("done")
    );
}

/// A record with an over-long id of its own is unusable; the page's other
/// records apply, and the cursor moves on because no later fetch could store
/// it. An over-long reference to a pending form can name no stored record, so
/// the record that carries it is kept without it.
#[tokio::test]
async fn an_over_long_id_makes_its_record_unusable() {
    let world = world().await;
    let checking = world.bound("acct-checking").await;
    let long = over_long_id();
    let longest = "m".repeat(MAX_PROVIDER_ID_CHARS);
    world.script.page(page(
        &[
            record("good", "acct-checking", "1"),
            record(&longest, "acct-checking", "1"),
            with_field("long-pending", "pending_transaction_id", &long),
            record(&long, "acct-checking", "1"),
        ],
        &[record(&long, "acct-checking", "1")],
        &[json!({ "transaction_id": long, "account_id": "acct-checking" })],
        "done",
        false,
    ));

    let run = world.sync().await;

    assert_eq!(run.status, SyncRunStatus::Partial);
    assert!(
        run.detail
            .as_deref()
            .is_some_and(|d| d.contains("3 created") && d.contains("3 skipped")),
        "the three usable records apply and the three others are skipped"
    );
    let mut stored: Vec<String> = world
        .transactions(checking)
        .await
        .iter()
        .map(|transaction| transaction.sources[0].provider_txn_id.clone())
        .collect();
    stored.sort();
    assert_eq!(
        stored,
        ["good".to_owned(), "long-pending".to_owned(), longest]
    );
    let connection = world.connection().await;
    assert_eq!(connection.provider_state.as_deref(), Some("done"));
    assert_eq!(connection.status, ConnectionStatus::Active);
    assert_eq!(connection.last_error, None);
}

/// An over-long account id on a record names no account coffer could store:
/// the record is unusable rather than holding the cursor for good, the others
/// apply, and the run counts what it passed by.
#[tokio::test]
async fn an_over_long_account_id_on_a_record_is_counted_and_does_not_hold_the_cursor() {
    let world = world().await;
    let checking = world.bound("acct-checking").await;
    let long = over_long_id();
    world.script.page(page(
        &[
            record("good", "acct-checking", "1"),
            with_field("long-account", "account_id", &long),
        ],
        &[],
        &[json!({ "transaction_id": "gone", "account_id": long })],
        "done",
        false,
    ));

    let run = world.sync().await;

    assert_eq!(run.status, SyncRunStatus::Partial);
    assert!(
        run.detail
            .as_deref()
            .is_some_and(|d| d.contains("2 skipped")),
        "both records without an account are counted"
    );
    let stored = world.transactions(checking).await;
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].sources[0].provider_txn_id, "good");
    let connection = world.connection().await;
    assert_eq!(connection.provider_state.as_deref(), Some("done"));
    assert_eq!(connection.last_error.as_deref(), Some(NO_ACCOUNT));
}

/// A record last seen in an unusable form is not applied in the stale form of
/// an earlier page, even when that last form names no account of its own.
#[tokio::test]
async fn an_unusable_last_form_beats_an_earlier_one_without_naming_an_account() {
    let world = world().await;
    let checking = world.bound("acct-checking").await;
    let long = over_long_id();
    world.script.page(page(
        &[
            record("stale", "acct-checking", "1"),
            record("good", "acct-checking", "1"),
        ],
        &[],
        &[],
        "next",
        true,
    ));
    world.script.page(page(
        &[],
        &[],
        &[json!({ "transaction_id": "stale", "account_id": long })],
        "done",
        false,
    ));

    let run = world.sync().await;

    assert_eq!(run.status, SyncRunStatus::Partial);
    assert!(
        run.detail
            .as_deref()
            .is_some_and(|d| d.contains("1 skipped")),
        "the record is counted once"
    );
    let stored = world.transactions(checking).await;
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].sources[0].provider_txn_id, "good");
    assert_eq!(
        world.connection().await.provider_state.as_deref(),
        Some("done")
    );
}

/// More accounts than any Item has fail the fetch before a page is asked for,
/// and exactly the limit does not.
#[tokio::test]
async fn too_many_accounts_fail_the_fetch() {
    let world = world().await;
    let checking = world.bound("acct-checking").await;
    first_sync(&world).await;
    let accounts = |count: usize| format!(r#"{{"accounts":[{}]}}"#, vec!["{}"; count].join(","));
    world
        .script
        .always(GET_ACCOUNTS, StatusCode::OK, accounts(MAX_ACCOUNTS + 1));
    let before = world.script.cursors().len();

    let run = world.sync().await;

    assert_eq!(run.status, SyncRunStatus::Failed);
    assert_eq!(run.detail.as_deref(), Some(TOO_MANY_ACCOUNTS));
    assert_eq!(world.script.cursors().len(), before, "no page is asked for");
    let connection = world.connection().await;
    assert_eq!(connection.provider_state.as_deref(), Some("saved"));
    assert_eq!(connection.status, ConnectionStatus::Error);
    assert_eq!(world.transactions(checking).await.len(), 1);

    world
        .script
        .always(GET_ACCOUNTS, StatusCode::OK, accounts(MAX_ACCOUNTS));
    world.script.page(page(&[], &[], &[], "done", false));
    let run = world.sync().await;
    assert_eq!(run.status, SyncRunStatus::Partial);
    assert_eq!(
        world.connection().await.last_error.as_deref(),
        Some(NO_ACCOUNT_ID),
        "accounts without an id are reported once, however many there are"
    );
}

/// An account whose id is over-long is reported as having no usable id, and
/// the others sync.
#[tokio::test]
async fn an_over_long_id_on_an_account_is_reported() {
    let world = world().await;
    let good = world.bound("good").await;
    let long = over_long_id();
    world.script.always(
        GET_ACCOUNTS,
        StatusCode::OK,
        accounts_answer(&[
            account_json("good", "depository", &json!(1)),
            account_json(&long, "depository", &json!(1)),
        ]),
    );
    world.script.page(page(
        &[record("kept", "good", "1")],
        &[],
        &[],
        "done",
        false,
    ));

    let run = world.sync().await;

    assert_eq!(run.status, SyncRunStatus::Partial);
    assert_eq!(world.transactions(good).await.len(), 1);
    assert_eq!(
        world.connection().await.last_error.as_deref(),
        Some(NO_ACCOUNT_ID)
    );
}

/// A long description or merchant is cut to the transaction limits as it
/// always was: only ids, which cannot be cut, make a record unusable.
#[tokio::test]
async fn long_texts_are_cut_and_the_record_kept() {
    let world = world().await;
    let checking = world.bound("acct-checking").await;
    let mut long = with_field("long-text", "name", &"n".repeat(4096));
    long["merchant_name"] = json!("m".repeat(4096));
    world.script.page(page(&[long], &[], &[], "done", false));

    let run = world.sync().await;

    assert_eq!(run.status, SyncRunStatus::Succeeded);
    let stored = world.transactions(checking).await;
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].raw_description.as_str().chars().count(), 500);
    assert_eq!(
        stored[0]
            .payee
            .as_ref()
            .map(|payee| payee.as_str().chars().count()),
        Some(200)
    );
}

async fn wait_finished(world: &World, run: coffer_domain::ids::SyncRunId) -> SyncRunRecord {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let record = world.server.store.get_sync_run(run).await.unwrap();
            if record.finished_at.is_some() {
                return record;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap()
}

/// An upstream that never answers runs out of the fetch's time: its run fails
/// with a fixed text, nothing is applied, the cursor stays, and the worker goes
/// on to the next connection.
#[tokio::test]
async fn a_silent_upstream_fails_its_run_and_the_next_connection_runs() {
    let mut world = world().await;
    let checking = world.bound("acct-checking").await;
    first_sync(&world).await;
    world.server.limit_fetch_time(Duration::from_millis(300));
    let other = world
        .server
        .seed_connection(
            ProviderKind::SimpleFin,
            "Other bank",
            world.alice.member,
            "https://user:password@bridge.example/simplefin",
        )
        .await;
    world.fake.push(Ok(FetchResult {
        accounts: Vec::new(),
        connection_errors: Vec::new(),
        skipped: 0,
        cursor: None,
    }));
    world.script.page(page(
        &[record("late", "acct-checking", "1")],
        &[],
        &[],
        "late",
        false,
    ));
    world.script.go_silent(SYNC);
    world.server.clock.advance(SignedDuration::from_secs(1));
    let silent = world.queue(world.connection).await;
    world.server.clock.advance(SignedDuration::from_secs(1));
    let next = world.queue(other).await;

    let (stop, stopped) = oneshot::channel::<()>();
    let scheduler = scheduler::spawn(world.server.app_state(), async {
        let _ = stopped.await;
    });
    let failed = wait_finished(&world, silent).await;
    let followed = wait_finished(&world, next).await;
    stop.send(()).unwrap();
    scheduler.await.unwrap();

    assert_eq!(failed.status, SyncRunStatus::Failed);
    assert_eq!(failed.detail.as_deref(), Some(TOO_SLOW));
    let connection = world.connection().await;
    assert_eq!(connection.provider_state.as_deref(), Some("saved"));
    assert_eq!(connection.status, ConnectionStatus::Error);
    assert_eq!(connection.last_error.as_deref(), Some(TOO_SLOW));
    assert_eq!(world.transactions(checking).await.len(), 1);
    assert_eq!(
        connection.last_attempt_at,
        Some(world.server.clock.now()),
        "the attempt counts, so the schedule does not retry it at once"
    );
    assert_eq!(followed.status, SyncRunStatus::Succeeded);
}
