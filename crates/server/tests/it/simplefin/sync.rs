//! Syncing from recorded fixtures: state, dates, amounts and error mapping.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use super::{
    ACCOUNTS_ERRORS, ACCOUNTS_FIRST, ACCOUNTS_POSTED, Reply, World, account_set, world,
    world_with_env,
};
use axum::http::StatusCode;
use coffer_domain::accounts::AccountKind;
use coffer_domain::ids::CategoryId;
use coffer_domain::sync::SyncRunStatus;
use coffer_domain::time::IanaZone;
use coffer_domain::transactions::{CategoryKind, SplitInput};
use coffer_server::ledger::categories::{CategoryGroupInput, CategoryInput};
use coffer_server::ledger::transactions::TransactionPatch;
use coffer_store::{AuditFilter, ConnectionStatus, PageRequest};
use jiff::civil::date;
use jiff::tz::TimeZone;
use jiff::{Span, Timestamp};
use serde_json::Value;
use serde_json::json;

async fn category(world: &World) -> CategoryId {
    let group = world
        .server
        .ledger
        .create_category_group(
            &world.alice,
            CategoryGroupInput {
                name: "Everyday".into(),
                position: None,
            },
            None,
        )
        .await
        .unwrap();
    world
        .server
        .ledger
        .create_category(
            &world.alice,
            CategoryInput {
                group: group.id,
                name: "Coffee".into(),
                kind: CategoryKind::Expense,
                position: None,
            },
            None,
        )
        .await
        .unwrap()
        .id
}

#[tokio::test]
async fn sync_from_fixtures_then_posted_keeps_state() {
    let world = world().await;
    let connection = world.seeded().await;
    world.script.bridge_reply(Reply::ok(ACCOUNTS_FIRST));
    assert_eq!(
        world.sync(connection).await.status,
        SyncRunStatus::Succeeded
    );
    let checking = world.bind("ACT-0001", AccountKind::Checking).await;
    let card = world.bind("ACT-0002", AccountKind::CreditCard).await;
    assert_eq!(world.transactions(card).await.len(), 1);
    let first = world.transactions(checking).await;
    assert_eq!(first.len(), 3);
    let pending = first.iter().find(|txn| txn.pending).unwrap();
    assert_eq!(pending.amount_minor, -1825);
    // Pending: dated by `transacted_at` (2027-01-14T20:45Z).
    assert_eq!(pending.posted_on, date(2027, 1, 14));
    let coffee = category(&world).await;
    world
        .server
        .ledger
        .update_transaction(
            &world.alice,
            pending.id,
            TransactionPatch {
                splits: Some(vec![SplitInput {
                    category: coffee,
                    minor_units: -1825,
                    goal: None,
                }]),
                ..TransactionPatch::default()
            },
        )
        .await
        .unwrap();

    world.script.bridge_reply(Reply::ok(ACCOUNTS_POSTED));
    assert_eq!(
        world.sync(connection).await.status,
        SyncRunStatus::Succeeded
    );

    let after = world.transactions(checking).await;
    assert_eq!(after.len(), 3);
    let posted = after.iter().find(|txn| txn.id == pending.id).unwrap();
    assert!(!posted.pending);
    assert_eq!(posted.posted_on, date(2027, 1, 15));
    assert_eq!(posted.splits.len(), 1);
    assert_eq!(posted.splits[0].category, coffee);
    assert!(after.iter().all(|txn| !txn.pending));
}

#[tokio::test]
async fn posted_date_uses_household_timezone() {
    let world = world_with_env(&[("COFFER_TIMEZONE", "America/Los_Angeles")]).await;
    let connection = world.seeded().await;
    // 2026-10-31T06:30Z is still 2026-10-30 in Los Angeles.
    world.script.bridge_reply(Reply::ok(&account_set(&json!([{
        "id": "TRN-0001", "posted": 1_793_428_200, "amount": "-5.00",
        "description": "PARKING METER",
    }]))));

    world.sync(connection).await;
    let account = world.bind("ACT-0001", AccountKind::Checking).await;

    let transactions = world.transactions(account).await;
    assert_eq!(transactions.len(), 1);
    assert_eq!(transactions[0].posted_on, date(2026, 10, 30));
}

/// Chooses `zone` as the household's, leaving the daily table alone.
async fn choose_zone(world: &World, zone: Option<&str>) {
    let zone = zone.map(|name| IanaZone::parse(name).unwrap());
    let mut tx = world.server.store.begin().await.unwrap();
    tx.set_household_zone(zone.as_ref(), date(2099, 1, 1))
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

/// 2026-10-31T06:30Z: 2026-10-30 in Los Angeles, 2026-10-31 in Auckland.
const LATE_EVENING: i64 = 1_793_428_200;
/// A day later.
const NEXT_EVENING: i64 = LATE_EVENING + 86_400;

fn record(id: &str, posted: i64) -> Value {
    json!({"id": id, "posted": posted, "amount": "-5.00", "description": "PARKING METER"})
}

/// The overlap of a sync's window reports the same record again, and the new zone puts its
/// instant on another day. That is not a change, and must not move the money.
#[tokio::test]
async fn a_record_reported_again_keeps_its_day_after_a_change_of_zone() {
    let world = world_with_env(&[("COFFER_TIMEZONE", "America/Los_Angeles")]).await;
    let connection = world.seeded().await;
    world
        .script
        .bridge_reply(Reply::ok(&account_set(&json!([record(
            "TRN-OLD",
            LATE_EVENING
        )]))));
    world.sync(connection).await;
    let account = world.bind("ACT-0001", AccountKind::Checking).await;
    assert_eq!(
        world.transactions(account).await[0].posted_on,
        date(2026, 10, 30)
    );

    // Three changes over it: the record stays where the first zone put it.
    for zone in [Some("Pacific/Auckland"), Some("Asia/Tokyo"), None] {
        choose_zone(&world, zone).await;
        world
            .script
            .bridge_reply(Reply::ok(&account_set(&json!([record(
                "TRN-OLD",
                LATE_EVENING
            )]))));
        world.sync(connection).await;
        let kept = world.transactions(account).await;
        assert_eq!(kept.len(), 1, "{zone:?}");
        assert_eq!(kept[0].posted_on, date(2026, 10, 30), "{zone:?}");
    }
}

/// A record that isn't known takes the zone of its day; once dated it is kept in that zone.
#[tokio::test]
async fn a_new_record_uses_the_new_zone_and_keeps_it() {
    let world = world_with_env(&[("COFFER_TIMEZONE", "America/Los_Angeles")]).await;
    let connection = world.seeded().await;
    choose_zone(&world, Some("Pacific/Auckland")).await;
    world
        .script
        .bridge_reply(Reply::ok(&account_set(&json!([record(
            "TRN-NEW",
            LATE_EVENING
        )]))));
    world.sync(connection).await;
    let account = world.bind("ACT-0001", AccountKind::Checking).await;
    assert_eq!(
        world.transactions(account).await[0].posted_on,
        date(2026, 10, 31)
    );

    choose_zone(&world, None).await;
    world
        .script
        .bridge_reply(Reply::ok(&account_set(&json!([record(
            "TRN-NEW",
            LATE_EVENING
        )]))));
    world.sync(connection).await;

    assert_eq!(
        world.transactions(account).await[0].posted_on,
        date(2026, 10, 31)
    );
}

/// A provider moving a record's instant is a real change, and takes the zone now in force.
#[tokio::test]
async fn a_record_whose_instant_moved_takes_the_new_zone() {
    let world = world_with_env(&[("COFFER_TIMEZONE", "America/Los_Angeles")]).await;
    let connection = world.seeded().await;
    world
        .script
        .bridge_reply(Reply::ok(&account_set(&json!([record(
            "TRN-MOVED",
            LATE_EVENING
        )]))));
    world.sync(connection).await;
    let account = world.bind("ACT-0001", AccountKind::Checking).await;

    choose_zone(&world, Some("Pacific/Auckland")).await;
    world
        .script
        .bridge_reply(Reply::ok(&account_set(&json!([record(
            "TRN-MOVED",
            NEXT_EVENING
        )]))));
    world.sync(connection).await;

    assert_eq!(
        world.transactions(account).await[0].posted_on,
        date(2026, 11, 1)
    );
}

/// A pending record that posts after a change of zone: its posting is new and takes the new
/// zone, and the day it was transacted hasn't moved.
#[tokio::test]
async fn a_pending_record_that_posts_keeps_the_day_it_was_transacted() {
    let world = world_with_env(&[("COFFER_TIMEZONE", "America/Los_Angeles")]).await;
    let connection = world.seeded().await;
    let pending = json!([{
        "id": "TRN-P", "posted": 0, "transacted_at": LATE_EVENING, "pending": true,
        "amount": "-5.00", "description": "PARKING METER",
    }]);
    let posted = json!([{
        "id": "TRN-P", "posted": NEXT_EVENING, "transacted_at": LATE_EVENING,
        "amount": "-5.00", "description": "PARKING METER",
    }]);
    world.script.bridge_reply(Reply::ok(&account_set(&pending)));
    world.sync(connection).await;
    let account = world.bind("ACT-0001", AccountKind::Checking).await;
    choose_zone(&world, Some("Pacific/Auckland")).await;

    for _ in 0..2 {
        world.script.bridge_reply(Reply::ok(&account_set(&posted)));
        world.sync(connection).await;

        let transaction = world.transactions(account).await.remove(0);
        assert!(!transaction.pending);
        assert_eq!(transaction.posted_on, date(2026, 11, 1), "in Auckland");
        assert_eq!(
            transaction.transacted_on,
            Some(date(2026, 10, 30)),
            "in Los Angeles"
        );
    }
}

/// Each instant has a different day in UTC, so counting either end in UTC moves
/// the limit by a day: (zone, now, the hour there of the reported instant).
const PLACES: [(&str, &str, i8); 2] = [
    // Still the 14th in Los Angeles, and 20:00 there is the next day in UTC.
    ("America/Los_Angeles", "2027-02-15T03:00:00Z", 20),
    // Already the 16th in Auckland, and 08:00 there is the day before in UTC.
    ("Pacific/Auckland", "2027-02-15T12:00:00Z", 8),
];

/// The age of a `balance-date` is counted in the household's days at both ends:
/// the instant becomes a day there, and today is today there.
#[tokio::test]
async fn balance_age_is_counted_in_the_household_time_zone() {
    for (zone, now, hour) in PLACES {
        for (age, written) in [(31, true), (32, false)] {
            let world = world_with_env(&[("COFFER_TIMEZONE", zone)]).await;
            let now: Timestamp = now.parse().unwrap();
            world.server.clock.set(now);
            let connection = world.seeded().await;
            let household = TimeZone::get(zone).unwrap();
            let today = now.to_zoned(household.clone()).date();
            world
                .script
                .bridge_reply(Reply::ok(&balance_dated(now.as_second(), "12.34")));
            world.sync(connection).await;
            let account = world.bind("ACT-0001", AccountKind::Checking).await;
            let day = today.checked_sub(Span::new().days(age)).unwrap();
            let moment = day.at(hour, 0, 0, 0).to_zoned(household).unwrap();
            world.script.bridge_reply(Reply::ok(&balance_dated(
                moment.timestamp().as_second(),
                "56.78",
            )));

            world.sync(connection).await;

            let mut tx = world.server.store.begin().await.unwrap();
            let snapshot = tx.balance_on(account, day).await.unwrap();
            drop(tx);
            assert_eq!(snapshot.is_some(), written, "{zone}, {age} days");
            assert_eq!(
                world.provider_account("ACT-0001").await.old_balance_refused,
                !written,
                "{zone}, {age} days"
            );
        }
    }
}

/// The epoch's `balance-date`, on an account with nothing yet: the amount lands on
/// the household's day 31 days back, and repeating the report changes nothing.
#[tokio::test]
async fn a_date_from_1970_lands_at_the_edge_in_the_household_time_zone() {
    for (zone, now, _) in PLACES {
        let world = world_with_env(&[("COFFER_TIMEZONE", zone)]).await;
        let now: Timestamp = now.parse().unwrap();
        world.server.clock.set(now);
        let connection = world.seeded().await;
        let today = now.to_zoned(TimeZone::get(zone).unwrap()).date();
        let edge = today.checked_sub(Span::new().days(31)).unwrap();
        world
            .script
            .bridge_reply(Reply::ok(&balance_dated(0, "12.34")));
        world.sync(connection).await;

        let account = world.bind("ACT-0001", AccountKind::Checking).await;

        let latest = world.server.store.latest_balance(account).await.unwrap();
        assert_eq!(latest.unwrap().on, edge, "{zone}");
        world
            .script
            .bridge_reply(Reply::ok(&balance_dated(0, "12.34")));
        world.sync(connection).await;
        let latest = world.server.store.latest_balance(account).await.unwrap();
        assert_eq!(latest.unwrap().on, edge, "{zone}");
        assert!(
            !world.provider_account("ACT-0001").await.old_balance_refused,
            "{zone}"
        );
    }
}

/// One account whose balance is dated `seconds` after the epoch.
fn balance_dated(seconds: i64, balance: &str) -> String {
    json!({
        "errors": [],
        "accounts": [{
            "id": "ACT-0001",
            "name": "Checking",
            "currency": "USD",
            "balance": balance,
            "balance-date": seconds,
            "transactions": [],
        }],
    })
    .to_string()
}

#[tokio::test]
async fn errors_fixture_reports_connection_and_account_errors() {
    let world = world().await;
    let connection = world.seeded().await;
    world.script.bridge_reply(Reply::ok(ACCOUNTS_ERRORS));

    let run = world.sync(connection).await;

    assert_eq!(run.status, SyncRunStatus::Partial);
    let record = world.server.store.get_connection(connection).await.unwrap();
    assert_eq!(
        record.last_error.as_deref(),
        Some("Example Bank: the connection needs attention")
    );
    let points = world.provider_account("ACT-0003").await;
    assert!(points.error.is_some());
    assert_eq!(points.currency_raw, "https://example.com/points");
    let savings = world.provider_account("ACT-0004").await;
    assert_eq!(savings.name, "Savings");
    assert_eq!(
        savings.error, None,
        "a record without an amount is skipped, not the account's error"
    );
    assert!(savings.last_balance.is_some());
    assert!(
        run.detail.as_deref().unwrap().contains("1 skipped"),
        "the record without an amount is skipped"
    );
}

/// An account whose own fields are unusable (here its balance) is the
/// account's error, and its name is still recorded.
#[tokio::test]
async fn unreadable_account_is_its_error() {
    let world = world().await;
    let connection = world.seeded().await;
    world.script.bridge_reply(Reply::ok(
        &json!({
            "errors": [],
            "accounts": [{
                "id": "ACT-0009", "name": "Brokerage", "currency": "USD",
                "balance": "lots", "balance-date": 1_799_992_800, "transactions": [],
            }],
        })
        .to_string(),
    ));

    let run = world.sync(connection).await;

    assert_eq!(run.status, SyncRunStatus::Partial);
    let unreadable = world.provider_account("ACT-0009").await;
    assert_eq!(unreadable.name, "Brokerage");
    assert!(unreadable.error.is_some());
}

/// A record without an amount, with an unparsable amount, a timestamp out
/// of range or no id is skipped; the account's other records and its
/// balance are applied.
#[tokio::test]
async fn unusable_records_skipped_others_applied() {
    let world = world().await;
    let connection = world.seeded().await;
    world.script.bridge_reply(Reply::ok(&account_set(&json!([
        { "id": "TRN-0001", "posted": 1_799_745_300, "amount": "-5.00",
          "description": "PARKING METER" },
        { "id": "TRN-0002", "posted": 1_799_745_300, "description": "NO AMOUNT" },
        { "id": "TRN-0003", "posted": 1_799_745_300, "amount": "12,50",
          "description": "BAD AMOUNT" },
        { "id": "TRN-0004", "posted": i64::MAX, "amount": "-1.00",
          "description": "FAR FUTURE" },
        { "posted": 1_799_745_300, "amount": "-2.00", "description": "NO ID" },
        { "id": "TRN-0005", "posted": 1_799_745_300, "amount": "-7.25",
          "description": "BOOKSHOP" },
    ]))));

    let run = world.sync(connection).await;

    assert_eq!(run.status, SyncRunStatus::Partial);
    assert!(
        run.detail.as_deref().unwrap().contains("4 skipped"),
        "every unusable record is counted as skipped"
    );
    let held = world.provider_account("ACT-0001").await;
    assert_eq!(held.error, None, "the account itself is not in error");
    assert!(held.last_balance.is_some(), "the balance is applied");
    let account = world.bind("ACT-0001", AccountKind::Checking).await;
    let mut amounts: Vec<i64> = world
        .transactions(account)
        .await
        .iter()
        .map(|txn| txn.amount_minor)
        .collect();
    amounts.sort_unstable();
    assert_eq!(amounts, vec![-725, -500]);
}

#[tokio::test]
async fn reauth_on_403() {
    let world = world().await;
    let connection = world.seeded().await;
    world
        .script
        .bridge_reply(Reply::status(StatusCode::FORBIDDEN, "access revoked"));

    let run = world.sync(connection).await;

    assert_eq!(run.status, SyncRunStatus::Failed);
    let record = world.server.store.get_connection(connection).await.unwrap();
    assert_eq!(record.status, ConnectionStatus::ReauthRequired);
}

#[tokio::test]
async fn rate_limited_on_429() {
    let world = world().await;
    let connection = world.seeded().await;
    world
        .script
        .bridge_reply(Reply::status(StatusCode::TOO_MANY_REQUESTS, "slow down"));

    let run = world.sync(connection).await;

    assert_eq!(run.status, SyncRunStatus::Failed);
    assert_eq!(
        run.detail.as_deref(),
        Some("the provider is rate limiting requests")
    );
    let record = world.server.store.get_connection(connection).await.unwrap();
    assert_eq!(record.status, ConnectionStatus::Error);
}

#[tokio::test]
async fn subscription_inactive_on_402() {
    let world = world().await;
    let connection = world.seeded().await;
    world
        .script
        .bridge_reply(Reply::status(StatusCode::PAYMENT_REQUIRED, ""));

    let run = world.sync(connection).await;

    assert_eq!(
        run.detail.as_deref(),
        Some("SimpleFin subscription inactive")
    );
}

/// A pending record the bridge gives no date for is dated the day it is
/// first seen and keeps that day: a later sync does not rewrite it.
#[tokio::test]
async fn undated_pending_record_keeps_its_first_day() {
    let world = world().await;
    let connection = world.seeded().await;
    let undated = account_set(&json!([{
        "id": "TRN-0001", "posted": 0, "amount": "-5.00", "description": "PARKING METER",
    }]));
    world.script.bridge_reply(Reply::ok(&undated));
    world.sync(connection).await;
    let account = world.bind("ACT-0001", AccountKind::Checking).await;
    let first_day = world.transactions(account).await[0].posted_on;

    world
        .server
        .clock
        .advance(jiff::SignedDuration::from_hours(48));
    world.script.bridge_reply(Reply::ok(&undated));
    assert_eq!(
        world.sync(connection).await.status,
        SyncRunStatus::Succeeded
    );

    let transactions = world.transactions(account).await;
    assert_eq!(transactions.len(), 1);
    assert_eq!(transactions[0].posted_on, first_day);
    let updates = world
        .server
        .store
        .list_audit(&AuditFilter::default(), &PageRequest::default())
        .await
        .unwrap()
        .items
        .iter()
        .filter(|event| event.action == "transaction.imported_update")
        .count();
    assert_eq!(updates, 0, "nothing changed, so nothing is audited");
}

#[tokio::test]
async fn amounts_parsed_exactly() {
    let world = world().await;
    let connection = world.seeded().await;
    world.script.bridge_reply(Reply::ok(&account_set(&json!([{
        "id": "TRN-0001", "posted": 1_799_745_300, "amount": "-1234567.89",
        "description": "LARGE TRANSFER", "extra": { "category": "transfer" },
    }]))));

    world.sync(connection).await;
    let account = world.bind("ACT-0001", AccountKind::Checking).await;

    let transactions = world.transactions(account).await;
    assert_eq!(transactions.len(), 1);
    assert_eq!(transactions[0].amount_minor, -123_456_789);
    let held = world.provider_account("ACT-0001").await;
    assert_eq!(held.last_balance.unwrap().amount_raw, "-1234567.89");
}

/// A number where the protocol has a decimal string makes that account an
/// error; it is never read as a float.
#[tokio::test]
async fn numeric_amount_is_skipped() {
    let world = world().await;
    let connection = world.seeded().await;
    world.script.bridge_reply(Reply::ok(&account_set(&json!([{
        "id": "TRN-0001", "posted": 1_799_745_300, "amount": -12.5,
        "description": "FLOAT AMOUNT",
    }]))));

    let run = world.sync(connection).await;

    // A JSON number is never read as an amount: the record is skipped.
    assert_eq!(run.status, SyncRunStatus::Partial);
    assert!(run.detail.as_deref().unwrap().contains("1 skipped"));
    let held = world.provider_account("ACT-0001").await;
    assert_eq!(held.error, None);
    let mut tx = world.server.store.begin().await.unwrap();
    assert_eq!(tx.held_sources(held.id).await.unwrap().len(), 0);
    drop(tx);
    assert_eq!(
        world
            .server
            .store
            .list_provider_accounts(None)
            .await
            .unwrap()
            .len(),
        1
    );
}
