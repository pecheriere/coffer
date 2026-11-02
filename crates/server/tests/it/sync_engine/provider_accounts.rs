//! Provider accounts in a run: new ones, failures, partial outcomes and
//! reauthorization.

use super::{amount, fetched, provider_account, txn, world};
use coffer_domain::accounts::AccountKind;
use coffer_domain::sync::SyncRunStatus;
use coffer_server::sync::provider::ProviderError;
use coffer_server::testkit::INJECTED_FAILURE_RECORD_ID;
use coffer_store::ConnectionStatus;

#[tokio::test]
async fn new_provider_accounts_appear_unbound_and_hold_records() {
    let world = world().await;
    let start = world.days_ago(88);
    world.fake.push(Ok(fetched(vec![provider_account(
        "acct-new",
        "USD",
        start,
        vec![
            txn("t-1", world.days_ago(3), "-12.50"),
            txn("t-2", world.days_ago(2), "100"),
        ],
    )])));

    let run = world.sync().await;
    assert_eq!(run.status, SyncRunStatus::Succeeded);
    let record = world.provider_account("acct-new").await;
    assert_eq!(record.account, None, "new provider accounts are unbound");
    assert_eq!(record.name, "Account acct-new");
    assert_eq!(record.institution.as_deref(), Some("Example Bank"));
    assert_eq!(record.currency_raw, "USD");
    assert_eq!(record.kind_hint, Some(AccountKind::Checking));
    assert_eq!(record.error, None);

    let mut tx = world.server.store.begin().await.unwrap();
    let held = tx.held_sources(record.id).await.unwrap();
    drop(tx);
    let mut ids: Vec<_> = held
        .iter()
        .map(|source| (source.provider_txn_id.as_str(), source.fields.amount_minor))
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, [("t-1", -1250), ("t-2", 10_000)]);
    assert!(
        held.iter().all(|source| source.transaction.is_none()),
        "held records belong to no transaction"
    );
}

#[tokio::test]
async fn currency_problem_isolated_to_one_account() {
    let world = world().await;
    let usd_b = world.account("B", "USD").await;
    let usd_c = world.account("C", "USD").await;
    world.bound("acct-b", "EUR", usd_b).await;
    world.bound("acct-c", "USD", usd_c).await;
    let start = world.days_ago(88);
    let day = world.days_ago(1);
    world.fake.push(Ok(fetched(vec![
        provider_account(
            "acct-a",
            "https://custom",
            start,
            vec![txn("a-1", day, "-1")],
        ),
        provider_account("acct-b", "EUR", start, vec![txn("b-1", day, "-2")]),
        provider_account("acct-c", "USD", start, vec![txn("c-1", day, "-3")]),
    ])));

    let run = world.sync().await;
    assert_eq!(run.status, SyncRunStatus::Partial);

    let a = world.provider_account("acct-a").await;
    assert!(
        a.error.is_some(),
        "a non-ISO currency is the account's error"
    );
    assert_eq!(a.currency_raw, "https://custom");
    let mut tx = world.server.store.begin().await.unwrap();
    assert_eq!(
        tx.held_sources(a.id).await.unwrap().len(),
        0,
        "no record held"
    );
    drop(tx);

    let b = world.provider_account("acct-b").await;
    assert!(
        b.error.is_some(),
        "a currency other than the bound account's is the account's error"
    );
    assert_eq!(world.transactions(usd_b).await.len(), 0, "no transaction");

    let c = world.provider_account("acct-c").await;
    assert_eq!(c.error, None);
    let committed = world.transactions(usd_c).await;
    assert_eq!(committed.len(), 1);
    assert_eq!(committed[0].amount_minor, -300);

    let connection = world
        .server
        .store
        .get_connection(world.connection)
        .await
        .unwrap();
    assert_eq!(connection.status, ConnectionStatus::Active);
    assert!(connection.last_success_at.is_some());
}

#[tokio::test]
async fn account_failure_rolls_back_only_that_account() {
    let world = world().await;
    let failing = world.account("Failing", "USD").await;
    let healthy = world.account("Healthy", "USD").await;
    world.bound("acct-x", "USD", failing).await;
    world.bound("acct-y", "USD", healthy).await;
    world.server.inject_source_insert_failure().await;
    let start = world.days_ago(88);
    let day = world.days_ago(1);
    let mut x = provider_account(
        "acct-x",
        "USD",
        start,
        vec![
            txn("x-1", day, "-1"),
            txn("x-2", day, "-2"),
            txn(INJECTED_FAILURE_RECORD_ID, day, "-3"),
        ],
    );
    x.balance = Some((day, amount("10")));
    world.fake.push(Ok(fetched(vec![
        x,
        provider_account("acct-y", "USD", start, vec![txn("y-1", day, "-4")]),
    ])));

    let run = world.sync().await;
    assert_eq!(run.status, SyncRunStatus::Partial);
    assert!(
        world.transactions(failing).await.is_empty(),
        "nothing of the failed account is committed"
    );
    assert_eq!(
        world.server.store.latest_balance(failing).await.unwrap(),
        None
    );
    let x = world.provider_account("acct-x").await;
    assert!(x.error.is_some(), "the failure is the account's error");
    assert_eq!(
        x.last_balance, None,
        "the account's balance rolled back too"
    );
    let mut tx = world.server.store.begin().await.unwrap();
    assert_eq!(
        tx.held_sources(x.id).await.unwrap().len(),
        0,
        "no record held"
    );
    for id in ["x-1", "x-2"] {
        assert!(
            tx.find_source(world.connection, id)
                .await
                .unwrap()
                .is_none(),
            "{id}"
        );
    }
    drop(tx);
    assert_eq!(world.transactions(healthy).await.len(), 1);
    assert_eq!(world.provider_account("acct-y").await.error, None);
}

#[tokio::test]
async fn connection_errors_alongside_data_are_partial() {
    let world = world().await;
    let mut result = fetched(vec![provider_account(
        "acct-1",
        "USD",
        world.days_ago(88),
        vec![txn("t-1", world.days_ago(1), "-5")],
    )]);
    result.connection_errors = vec![
        "Connection to Example Bank may need attention\u{1b}[0m".into(),
        "Another institution is slow".into(),
    ];
    world.fake.push(Ok(result));

    let run = world.sync().await;
    assert_eq!(run.status, SyncRunStatus::Partial);
    let connection = world
        .server
        .store
        .get_connection(world.connection)
        .await
        .unwrap();
    assert_eq!(connection.status, ConnectionStatus::Active);
    assert_eq!(
        connection.last_error.as_deref(),
        Some("Connection to Example Bank may need attention[0m; Another institution is slow")
    );
    assert!(connection.last_success_at.is_some());
    let mut tx = world.server.store.begin().await.unwrap();
    let record = world.provider_account("acct-1").await;
    assert_eq!(tx.held_sources(record.id).await.unwrap().len(), 1);
    drop(tx);

    // A clean run clears the error.
    world.fake.push(Ok(fetched(Vec::new())));
    assert_eq!(world.sync().await.status, SyncRunStatus::Succeeded);
    let connection = world
        .server
        .store
        .get_connection(world.connection)
        .await
        .unwrap();
    assert_eq!(connection.last_error, None);
}

#[tokio::test]
async fn reauth_marks_connection() {
    let world = world().await;
    world.fake.push(Err(ProviderError::ReauthRequired));
    let run = world.sync().await;
    assert_eq!(run.status, SyncRunStatus::Failed);
    assert!(run.finished_at.is_some());
    let connection = world
        .server
        .store
        .get_connection(world.connection)
        .await
        .unwrap();
    assert_eq!(connection.status, ConnectionStatus::ReauthRequired);
    assert_eq!(connection.last_success_at, None);
    assert!(connection.last_attempt_at.is_some());
}
