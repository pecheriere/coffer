//! How old a balance a provider may report (issue 80): one rule for bound and
//! unbound accounts. A refused report writes nothing and says so on the provider
//! account, and everything else of the sync still lands.

use super::kept_balances::{FEED, balance_of, signed_in_world};
use super::{World, reported, txn, world};
use axum::http::{Method, StatusCode};
use coffer_domain::accounts::AccountKind;
use coffer_domain::identity::Scope;
use coffer_domain::ids::AccountId;
use serde_json::json;

const NOTE: &str =
    "the provider reported a balance dated more than 31 days ago, so it was left out";

impl World {
    /// An account bound to the feed, with a balance reported yesterday.
    async fn bound_feed(&self) -> AccountId {
        self.report(1, "10.00", "USD").await;
        let target = self.account("Checking", "USD").await;
        self.bind_feed(target).await;
        target
    }

    async fn feed_note(&self) -> Option<String> {
        self.provider_account(FEED).await.error
    }
}

#[tokio::test]
async fn a_bound_account_takes_a_balance_31_days_old() {
    let world = world().await;
    let target = world.bound_feed().await;

    world.report(31, "20.00", "USD").await;

    assert_eq!(
        world.history(target).await,
        [world.sync_row(1, 1000), world.sync_row(31, 2000)]
    );
    assert_eq!(world.feed_note().await, None);
}

#[tokio::test]
async fn a_bound_account_refuses_a_balance_32_days_old_and_says_so() {
    let world = world().await;
    let target = world.bound_feed().await;

    world.report(32, "20.00", "USD").await;

    assert_eq!(world.history(target).await, [world.sync_row(1, 1000)]);
    assert_eq!(world.feed_note().await.as_deref(), Some(NOTE));
}

/// The attack the bound is for: rewriting a day long past.
#[tokio::test]
async fn a_provider_cannot_overwrite_an_old_day() {
    let world = world().await;
    let target = world.bound_feed().await;
    world.manual(target, world.day(100), 777).await;

    world.report(100, "20.00", "USD").await;

    let history = world.history(target).await;
    assert_eq!(
        history[1],
        (world.day(100).to_string(), 777, "manual".to_owned())
    );
}

/// A member's balance is no provider report: it may be dated any day, and the
/// bound doesn't touch it.
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
    assert_eq!(
        world.history(target).await[1],
        (day.to_string(), 777, "manual".to_owned())
    );
}

#[tokio::test]
async fn a_refused_balance_does_not_stop_the_accounts_transactions() {
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
    assert_eq!(world.feed_note().await.as_deref(), Some(NOTE));
    assert_eq!(
        world.provider_account(FEED).await.last_applied_at,
        Some(world.now()),
        "the next window starts from this sync"
    );
}

#[tokio::test]
async fn a_refused_balance_leaves_other_accounts_alone() {
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
    assert_eq!(world.provider_account("acct-2").await.error, None);
}

#[tokio::test]
async fn a_balance_in_range_lands_and_clears_the_note() {
    let world = world().await;
    let target = world.bound_feed().await;
    world.report(32, "20.00", "USD").await;

    world.report(0, "30.00", "USD").await;

    assert_eq!(world.history(target).await[0], world.sync_row(0, 3000));
    assert_eq!(world.feed_note().await, None);
}

/// A new account whose only balance is too old binds with no balance, and gets
/// one from the first sync that reports one in range.
#[tokio::test]
async fn a_first_report_too_old_still_binds_with_no_balance() {
    let world = world().await;
    world.report(40, "20.00", "USD").await;
    assert_eq!(world.kept(world.connection).await, []);
    assert_eq!(world.feed_note().await.as_deref(), Some(NOTE));
    let target = world.account("Checking", "USD").await;

    world.bind_feed(target).await;

    assert_eq!(world.history(target).await, []);

    world.report(0, "30.00", "USD").await;

    assert_eq!(world.history(target).await, [world.sync_row(0, 3000)]);
    assert_eq!(world.feed_note().await, None);
}

#[tokio::test]
async fn an_account_created_for_a_first_report_too_old_has_no_balance() {
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
    assert_eq!(linked["account"]["latest_balance"], json!(null));
}

/// What a member and an agent see: the fixed words, in the place every other
/// problem with the account shows.
#[tokio::test]
async fn the_refusal_shows_on_the_connections_page_and_in_the_api() {
    let (world, mut browser) = signed_in_world().await;
    world.report(32, "20.00", "USD").await;

    let reply = browser.get("/connections").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(reply.body.contains(&format!("Error: {NOTE}")));

    let token = world.token(Scope::Read).await;
    let (status, accounts) = world
        .call(Method::GET, "/api/v1/provider-accounts", &token, None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(accounts["items"][0]["error"], json!(NOTE));
}

/// An agent reading the forecast is told the balance may be out of date.
#[tokio::test]
async fn the_refusal_reaches_the_forecast_of_a_counted_account() {
    let world = world().await;
    let target = world.bound_feed().await;
    world.report(32, "20.00", "USD").await;
    let token = world.token(Scope::Read).await;

    let (status, forecast) = world
        .call(Method::GET, "/api/v1/forecast", &token, None)
        .await;

    assert_eq!(status, StatusCode::OK);
    let cautions = forecast.to_string();
    assert!(cautions.contains("account_feed_problem"));
    assert!(cautions.contains("provider_error"));
    assert!(cautions.contains(&target.to_string()));
}
