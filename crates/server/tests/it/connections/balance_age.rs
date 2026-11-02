//! How far to believe the day a provider puts on a balance (issue 80). The date
//! may honestly be old, so an old date doesn't lose the amount, but it can't add
//! or change history at or before what the account already has.

use super::kept_balances::{FEED, balance_of, signed_in_world};
use super::{World, reported, txn, world};
use axum::http::{Method, StatusCode};
use coffer_domain::accounts::AccountKind;
use coffer_domain::identity::Scope;
use coffer_domain::ids::AccountId;
use jiff::SignedDuration;
use serde_json::{Value, json};

const NOTE: &str = "The bank sent a balance older than what coffer already has, so it was \
     ignored. The balance kept is still the latest one.";

impl World {
    /// An account bound to the feed, with a balance reported yesterday.
    async fn bound_feed(&self) -> AccountId {
        self.report(1, "10.00", "USD").await;
        let target = self.account("Checking", "USD").await;
        self.bind_feed(target).await;
        target
    }

    /// An account bound to the feed, with no balance at all.
    async fn empty_bound_feed(&self) -> AccountId {
        self.sync_once(vec![reported(
            FEED,
            "USD",
            Some(AccountKind::Checking),
            Vec::new(),
        )])
        .await;
        let target = self.account("Checking", "USD").await;
        self.bind_feed(target).await;
        target
    }

    async fn refused(&self) -> bool {
        self.provider_account(FEED).await.old_balance_refused
    }

    /// The forecast, with the floor it is blocked without.
    async fn forecast(&self) -> Value {
        let admin = self.token(Scope::Admin).await;
        let floor =
            json!({"floor": {"kind": "fixed", "amount": {"minor_units": 0, "currency": "USD"}}});
        let (status, _) = self
            .call(
                Method::PATCH,
                "/api/v1/household/settings",
                &admin,
                Some(floor),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        let token = self.token(Scope::Read).await;
        let (status, forecast) = self
            .call(Method::GET, "/api/v1/forecast", &token, None)
            .await;
        assert_eq!(status, StatusCode::OK);
        forecast
    }

    fn manual_row(&self, ago: i64, minor_units: i64) -> (String, i64, String) {
        (self.day(ago).to_string(), minor_units, "manual".to_owned())
    }
}

// The boundary: within 31 days the date is believed as it always was.

#[tokio::test]
async fn a_balance_31_days_old_is_written_on_its_day() {
    let world = world().await;
    let target = world.bound_feed().await;

    world.report(31, "20.00", "USD").await;

    assert_eq!(
        world.history(target).await,
        [world.sync_row(1, 1000), world.sync_row(31, 2000)]
    );
    assert!(!world.refused().await);
}

// Case 1: an account with no snapshot takes the amount, at the bound's edge.

#[tokio::test]
async fn an_empty_account_takes_an_old_balance_at_the_edge() {
    let world = world().await;
    let target = world.empty_bound_feed().await;

    world.report(32, "20.00", "USD").await;

    assert_eq!(world.history(target).await, [world.sync_row(31, 2000)]);
    assert!(!world.refused().await);
}

/// However old the date, it adds one day of history, not a thousand years.
#[tokio::test]
async fn an_old_date_on_an_empty_account_adds_no_history() {
    let world = world().await;
    let target = world.empty_bound_feed().await;

    world.report(365_000, "20.00", "USD").await;
    world.report(365_000, "20.00", "USD").await;

    assert_eq!(world.history(target).await, [world.sync_row(31, 2000)]);
}

/// A new cash account whose bank last changed its balance 40 days ago: binding
/// gives it that balance, so the household's forecast isn't left unavailable.
#[tokio::test]
async fn a_first_report_40_days_old_gives_the_bind_a_balance() {
    let world = world().await;
    world.report(40, "20.00", "USD").await;
    assert_eq!(world.kept(world.connection).await, []);
    let target = world.account("Checking", "USD").await;

    world.bind_feed(target).await;

    assert_eq!(world.history(target).await, [world.sync_row(31, 2000)]);
    assert!(!world.refused().await);
    let forecast = world.forecast().await;
    assert_ne!(forecast["status"], json!("unavailable"));
}

#[tokio::test]
async fn an_account_created_for_an_old_first_report_has_that_balance() {
    let world = world().await;
    world.report(40, "20.00", "USD").await;
    let feed = world.provider_account(FEED).await;
    let token = world.token(Scope::Write).await;

    let (status, linked) = world
        .call(
            Method::POST,
            &format!("/api/v1/provider-accounts/{}/create-account", feed.id),
            &token,
            Some(json!({})),
        )
        .await;

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        linked["account"]["latest_balance"]["on"],
        json!(world.day(31))
    );
    assert_eq!(
        linked["account"]["latest_balance"]["balance"]["minor_units"],
        json!(2000)
    );
}

// Case 2: a day after the latest snapshot rewrites nothing.

#[tokio::test]
async fn an_old_day_after_the_latest_snapshot_is_written() {
    let world = world().await;
    let target = world.empty_bound_feed().await;
    world.manual(target, world.day(50), 777).await;

    world.report(40, "20.00", "USD").await;

    assert_eq!(
        world.history(target).await,
        [world.sync_row(40, 2000), world.manual_row(50, 777)]
    );
    assert!(!world.refused().await);
}

// Case 3: what the account already holds is a quiet no-op.

/// A dormant account: its balance was reported in time, nothing has changed since,
/// and the bank goes on giving the day it last changed.
#[tokio::test]
async fn a_dormant_accounts_unchanged_report_stays_quiet() {
    let world = world().await;
    let target = world.bound_feed().await;
    let last_changed = world.day(1);
    let before = world.history(target).await;
    let audited = world.audit_actions("balance.set").await.len();

    world
        .server
        .clock
        .advance(SignedDuration::from_hours(40 * 24));
    world
        .sync_once(vec![balance_of(FEED, "USD", last_changed, "10.00")])
        .await;

    assert_eq!(world.history(target).await, before);
    assert!(!world.refused().await);
    assert_eq!(world.audit_actions("balance.set").await.len(), audited);
}

/// A balance taken at the edge is dated later than reported, so its report never
/// matches by day. Going on reporting it must not read as a refusal every sync.
#[tokio::test]
async fn an_account_taken_at_the_edge_stays_quiet_as_days_pass() {
    let world = world().await;
    world.report(40, "20.00", "USD").await;
    let reported_day = world.day(40);
    let target = world.account("Checking", "USD").await;
    world.bind_feed(target).await;
    let before = world.history(target).await;

    world.server.clock.advance(SignedDuration::from_hours(24));
    world
        .sync_once(vec![balance_of(FEED, "USD", reported_day, "20.00")])
        .await;

    assert_eq!(world.history(target).await, before);
    assert!(!world.refused().await);
}

#[tokio::test]
async fn the_same_day_and_amount_as_an_older_snapshot_is_quiet() {
    let world = world().await;
    let target = world.bound_feed().await;
    world.manual(target, world.day(100), 2000).await;

    world.report(100, "20.00", "USD").await;

    assert_eq!(world.history(target).await[1], world.manual_row(100, 2000));
    assert!(!world.refused().await);
}

// Case 4: anything else would add or change history, and is refused.

#[tokio::test]
async fn an_old_day_before_the_latest_snapshot_is_refused() {
    let world = world().await;
    let target = world.bound_feed().await;

    world.report(32, "20.00", "USD").await;

    assert_eq!(world.history(target).await, [world.sync_row(1, 1000)]);
    assert!(world.refused().await);
}

/// The attack the rule is for: rewriting a day long past.
#[tokio::test]
async fn a_provider_cannot_overwrite_an_old_day() {
    let world = world().await;
    let target = world.bound_feed().await;
    world.manual(target, world.day(100), 777).await;

    world.report(100, "20.00", "USD").await;

    assert_eq!(world.history(target).await[1], world.manual_row(100, 777));
    assert!(world.refused().await);
}

#[tokio::test]
async fn a_new_amount_on_the_latest_old_day_is_refused() {
    let world = world().await;
    let target = world.empty_bound_feed().await;
    world.manual(target, world.day(50), 777).await;

    world.report(50, "20.00", "USD").await;

    assert_eq!(world.history(target).await, [world.manual_row(50, 777)]);
    assert!(world.refused().await);
}

/// At the bind too: the balance kept is newer than the last report.
#[tokio::test]
async fn a_bind_refuses_a_last_report_older_than_what_was_kept() {
    let world = world().await;
    world.report(1, "10.00", "USD").await;
    world.report(40, "20.00", "USD").await;
    let target = world.account("Checking", "USD").await;

    world.bind_feed(target).await;

    assert_eq!(world.history(target).await, [world.sync_row(1, 1000)]);
    assert!(world.refused().await);
}

#[tokio::test]
async fn a_refusal_does_not_stop_the_accounts_transactions() {
    let world = world().await;
    let target = world.bound_feed().await;
    let mut data = reported(
        FEED,
        "USD",
        Some(AccountKind::Checking),
        vec![txn("t-1", "-5.00")],
    );
    data.balance = balance_of(FEED, "USD", world.day(32), "20.00").balance;

    world.sync_once(vec![data]).await;

    assert_eq!(world.transactions(target).await.len(), 1);
    assert_eq!(world.history(target).await, [world.sync_row(1, 1000)]);
    assert!(world.refused().await);
    assert_eq!(
        world.provider_account(FEED).await.last_applied_at,
        Some(world.now()),
        "the next window starts from this sync"
    );
}

#[tokio::test]
async fn a_refusal_leaves_other_accounts_alone() {
    let world = world().await;
    let target = world.bound_feed().await;

    world
        .sync_once(vec![
            balance_of(FEED, "USD", world.day(32), "20.00"),
            balance_of("acct-2", "USD", world.day(0), "30.00"),
        ])
        .await;

    assert_eq!(world.history(target).await, [world.sync_row(1, 1000)]);
    assert_eq!(
        world.kept(world.connection).await,
        [(world.day(0).to_string(), 3000)]
    );
    assert!(!world.provider_account("acct-2").await.old_balance_refused);
}

// While unbound, the bound of keeping stands: "later than the latest" would let a
// provider walk a history up from 1970 for the bind to bring in.

#[tokio::test]
async fn an_unbound_account_cannot_walk_a_history_past_the_bound() {
    let world = world().await;
    for ago in [100, 99, 98, 32] {
        world.report(ago, "20.00", "USD").await;
    }
    assert_eq!(world.kept(world.connection).await, []);
    assert!(!world.refused().await);
    let target = world.account("Checking", "USD").await;

    world.bind_feed(target).await;

    assert_eq!(world.history(target).await, [world.sync_row(31, 2000)]);
}

// Clearing the note.

#[tokio::test]
async fn a_newer_balance_from_a_sync_clears_the_note() {
    let world = world().await;
    let target = world.bound_feed().await;
    world.report(32, "20.00", "USD").await;

    world.report(0, "30.00", "USD").await;

    assert_eq!(world.history(target).await[0], world.sync_row(0, 3000));
    assert!(!world.refused().await);
}

#[tokio::test]
async fn a_newer_balance_set_by_hand_clears_the_note() {
    let world = world().await;
    let target = world.bound_feed().await;
    world.report(32, "20.00", "USD").await;
    assert!(world.refused().await);

    world.manual(target, world.today(), 2000).await;

    assert!(!world.refused().await);
    // The bank goes on reporting that amount from its old day: nothing to say.
    world.report(32, "20.00", "USD").await;
    assert!(!world.refused().await);
    // A different amount is still a report older than what coffer has.
    world.report(32, "25.00", "USD").await;
    assert!(world.refused().await);
}

#[tokio::test]
async fn a_balance_set_by_hand_on_an_older_day_leaves_the_note() {
    let world = world().await;
    let target = world.bound_feed().await;
    world.report(32, "20.00", "USD").await;

    world.manual(target, world.day(60), 2000).await;

    assert!(world.refused().await);
}

/// A member's balance is no provider report: it may be dated any day.
#[tokio::test]
async fn a_member_can_still_set_an_old_balance_by_hand() {
    let world = world().await;
    let target = world.bound_feed().await;
    let token = world.token(Scope::Write).await;
    let day = world.day(400);

    let (status, _) = world
        .call(
            Method::PUT,
            &format!("/api/v1/accounts/{target}/balances/{day}"),
            &token,
            Some(json!({ "minor_units": 777 })),
        )
        .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(world.history(target).await[1], world.manual_row(400, 777));
}

// What people see.

/// The fixed words, apart from the account's errors.
#[tokio::test]
async fn the_note_shows_on_the_connections_page_and_in_the_api() {
    let (world, mut browser) = signed_in_world().await;
    world.bound_feed().await;
    world.report(32, "20.00", "USD").await;

    let reply = browser.get("/connections").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(reply.body.contains(NOTE));
    assert!(!reply.body.contains("Error: "));

    let token = world.token(Scope::Read).await;
    let (status, accounts) = world
        .call(Method::GET, "/api/v1/provider-accounts", &token, None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(accounts["items"][0]["old_balance_note"], json!(NOTE));
    assert_eq!(accounts["items"][0]["error"], json!(null));

    let (status, connections) = world
        .call(Method::GET, "/api/v1/connections", &token, None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let listed = &connections["items"][0]["accounts"][0];
    assert_eq!(listed["old_balance_note"], json!(NOTE));
    assert_eq!(listed["error"], json!(null));
}

/// coffer refused the date, so nothing says the bank reported an error, and the
/// refusal alone moves no forecast.
#[tokio::test]
async fn a_refusal_raises_nothing_in_the_forecast() {
    let world = world().await;
    world.bound_feed().await;
    let before = world.forecast().await;

    world.report(32, "20.00", "USD").await;

    let after = world.forecast().await;
    assert!(world.refused().await);
    assert_eq!(after["status"], json!("firm"));
    assert_eq!(after, before);
}

/// A dormant account's only signal is the staleness of its balance, as for any
/// balance that old.
#[tokio::test]
async fn a_dormant_account_shows_only_as_stale_in_the_forecast() {
    let world = world().await;
    world.report(40, "20.00", "USD").await;
    let target = world.account("Checking", "USD").await;
    world.bind_feed(target).await;

    let forecast = world.forecast().await;

    assert_eq!(forecast["status"], json!("provisional"));
    let words = forecast.to_string();
    assert!(words.contains("balance_stale"));
    assert!(!words.contains("balance_missing"));
    assert!(!words.contains("account_feed_problem"));
}
