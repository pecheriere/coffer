//! Records a run cannot use, undated records, sanitized text and balance dates.

use super::{World, amount, fetched, provider_account, txn, world};
use coffer_domain::accounts::BalanceSource;
use coffer_domain::sync::SyncRunStatus;
use coffer_server::sync::provider::ProviderError;
use coffer_store::ConnectionStatus;
use jiff::{SignedDuration, Span};

/// An amount with more decimals than the currency allows skips that record
/// only; a known record re-reported that way is still reported, so it does
/// not vanish.
#[tokio::test]
async fn unrepresentable_record_skipped_not_the_account() {
    let world = world().await;
    let account = world.account("Checking", "USD").await;
    world.bound("acct-1", "USD", account).await;
    let start = world.days_ago(90);
    let day = world.days_ago(1);
    let mut pending = txn("p-1", day, "-4.00");
    pending.pending = true;
    world.fake.push(Ok(fetched(vec![provider_account(
        "acct-1",
        "USD",
        start,
        vec![pending.clone()],
    )])));
    assert_eq!(world.sync().await.status, SyncRunStatus::Succeeded);

    let mut garbled = pending;
    garbled.amount = amount("-4.001");
    world.fake.push(Ok(fetched(vec![provider_account(
        "acct-1",
        "USD",
        start,
        vec![
            txn("t-1", day, "-1.00"),
            txn("t-2", day, "-2.005"),
            garbled,
            txn("t-3", day, "-3.00"),
        ],
    )])));
    let run = world.sync().await;

    assert_eq!(run.status, SyncRunStatus::Partial);
    assert!(
        run.detail.as_deref().unwrap().contains("2 skipped"),
        "both unrepresentable records are skipped"
    );
    assert_eq!(world.provider_account("acct-1").await.error, None);
    let mut amounts: Vec<i64> = world
        .transactions(account)
        .await
        .iter()
        .map(|txn| txn.amount_minor)
        .collect();
    amounts.sort_unstable();
    assert_eq!(
        amounts,
        vec![-400, -300, -100],
        "the good records are applied and the known pending one is kept"
    );
}

/// A pending record without a date is dated today when first seen and
/// keeps that day; a posted record without one is skipped.
#[tokio::test]
async fn undated_records_are_dated_or_skipped() {
    let world = world().await;
    let account = world.account("Checking", "USD").await;
    world.bound("acct-1", "USD", account).await;
    let start = world.days_ago(90);
    let mut pending = txn("p-1", start, "-4.00");
    pending.posted_on = None;
    pending.pending = true;
    let mut posted = txn("t-1", start, "-1.00");
    posted.posted_on = None;
    let report = |world: &World| {
        world.fake.push(Ok(fetched(vec![provider_account(
            "acct-1",
            "USD",
            start,
            vec![pending.clone(), posted.clone()],
        )])));
    };

    report(&world);
    let first = world.sync().await;
    let first_day = world.today();
    world.server.clock.advance(SignedDuration::from_hours(48));
    report(&world);
    world.sync().await;

    assert!(
        first.detail.as_deref().unwrap().contains("1 skipped"),
        "the posted record without a date is skipped"
    );
    let transactions = world.transactions(account).await;
    assert_eq!(transactions.len(), 1);
    assert!(transactions[0].pending);
    assert_eq!(transactions[0].posted_on, first_day);
}

#[tokio::test]
async fn provider_error_message_sanitized() {
    let world = world().await;
    world.fake.push(Err(ProviderError::Unavailable(
        "failed for https://u:p@bridge.example/x\u{7}".into(),
    )));
    let run = world.sync().await;
    assert_eq!(run.status, SyncRunStatus::Failed);
    let connection = world
        .server
        .store
        .get_connection(world.connection)
        .await
        .unwrap();
    assert_eq!(connection.status, ConnectionStatus::Error);
    assert_eq!(
        connection.last_error.as_deref(),
        Some("failed for https://***@bridge.example/x")
    );
    assert_eq!(
        run.detail.as_deref(),
        Some("failed for https://***@bridge.example/x")
    );
}

#[tokio::test]
async fn balance_snapshot_uses_provider_balance_date() {
    let world = world().await;
    let account = world.account("Checking", "USD").await;
    world.bound("acct-1", "USD", account).await;
    let described = world.days_ago(2);
    let mut data = provider_account("acct-1", "USD", world.days_ago(90), Vec::new());
    data.balance = Some((described, amount("1234.56")));
    world.fake.push(Ok(fetched(vec![data])));
    world.sync().await;

    let snapshot = world
        .server
        .store
        .latest_balance(account)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        snapshot.on, described,
        "dated by the day the balance describes"
    );
    assert_eq!(snapshot.balance_minor, 123_456);
    assert_eq!(snapshot.source, BalanceSource::Sync);
    let record = world.provider_account("acct-1").await;
    let raw = record.last_balance.unwrap();
    assert_eq!((raw.on, raw.amount_raw.as_str()), (described, "1234.56"));

    // A balance dated after the household's today is dated today.
    let tomorrow = world.today().checked_add(Span::new().days(1)).unwrap();
    let mut data = provider_account("acct-1", "USD", world.days_ago(90), Vec::new());
    data.balance = Some((tomorrow, amount("99")));
    world.fake.push(Ok(fetched(vec![data])));
    world.sync().await;
    let snapshot = world
        .server
        .store
        .latest_balance(account)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (snapshot.on, snapshot.balance_minor),
        (world.today(), 9_900)
    );
}
