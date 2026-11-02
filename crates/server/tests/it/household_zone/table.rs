//! The daily table across a change of zone: never a gap, a day twice or a day to come.

use std::sync::Arc;

use coffer_domain::ids::AccountId;
use coffer_server::fx::FxService;
use jiff::SignedDuration;
use jiff::civil::Date;
use serde_json::{Value, json};

use super::{AUCKLAND, AUCKLAND_DAY, LOS_ANGELES, LOS_ANGELES_DAY, set_zone, unbuilt_at, world_at};
use crate::net_worth::World;

fn day(text: &str) -> Date {
    text.parse().unwrap()
}

/// The job that fills the table, on `world`'s server.
fn job(world: &World) -> Arc<FxService> {
    Arc::new(FxService::new(
        world.server.store.clone(),
        world.server.clock.clone(),
        world.server.config.default_timezone.clone(),
        None,
    ))
}

async fn through(world: &World) -> Option<Date> {
    world
        .server
        .store
        .net_worth_state()
        .await
        .unwrap()
        .filled_through
}

/// `account`'s days and balances in the table, oldest first.
async fn row_days(world: &World, account: AccountId) -> Vec<(Date, i64)> {
    world
        .rows()
        .await
        .into_iter()
        .filter(|row| row.account == account)
        .map(|row| (row.on, row.balance_minor))
        .collect()
}

/// Each day once, from the first to `last`, with no day missing.
fn assert_every_day_once(rows: &[(Date, i64)], first: &str, last: &str) {
    assert_eq!(rows.first().map(|row| row.0), Some(day(first)));
    assert_eq!(rows.last().map(|row| row.0), Some(day(last)));
    for pair in rows.windows(2) {
        assert_eq!(pair[0].0.tomorrow().unwrap(), pair[1].0, "one day apart");
    }
}

/// Today moves back across a day and a month: the table ends today at once, the balance
/// dated the later day stays, and that day comes back when it arrives, once.
#[tokio::test]
async fn moving_today_back_leaves_no_day_to_come_and_no_gap() {
    let world = world_at(AUCKLAND, AUCKLAND_DAY).await;
    let account = world.holding("Checking", "USD").await;
    world.balance(account, "2026-10-29", 100).await;
    world.balance(account, AUCKLAND_DAY, 200).await;
    assert_every_day_once(&row_days(&world, account).await, "2026-10-29", AUCKLAND_DAY);

    set_zone(&world, json!(LOS_ANGELES)).await;

    assert_eq!(through(&world).await, Some(day(LOS_ANGELES_DAY)));
    let rows = row_days(&world, account).await;
    assert_every_day_once(&rows, "2026-10-29", LOS_ANGELES_DAY);
    assert_eq!(
        rows.last().unwrap().1,
        100,
        "the day before the later balance"
    );
    let (_, text) = world
        .send(
            axum::http::Method::GET,
            &format!("/api/v1/accounts/{account}/balances"),
            &world.admin,
            None,
        )
        .await;
    assert!(text.contains(AUCKLAND_DAY), "the balance itself stays");

    let job = job(&world);
    job.keep_table_current().await.unwrap();
    assert_eq!(
        through(&world).await,
        Some(day(LOS_ANGELES_DAY)),
        "the job agrees"
    );

    world.server.clock.advance(SignedDuration::from_hours(24));
    job.keep_table_current().await.unwrap();

    assert_eq!(through(&world).await, Some(day(AUCKLAND_DAY)));
    let rows = row_days(&world, account).await;
    assert_every_day_once(&rows, "2026-10-29", AUCKLAND_DAY);
    assert_eq!(
        rows.last().unwrap().1,
        200,
        "the later balance, on its own day"
    );
}

/// Today moving forward finds a table that is right as far as it goes: nothing is
/// rebuilt, and the job adds the day as it does at any midnight.
#[tokio::test]
async fn moving_today_forward_rebuilds_nothing() {
    let world = world_at(LOS_ANGELES, LOS_ANGELES_DAY).await;
    let account = world.holding("Checking", "USD").await;
    world.balance(account, "2026-10-29", 100).await;

    set_zone(&world, json!(AUCKLAND)).await;

    assert_eq!(through(&world).await, Some(day(LOS_ANGELES_DAY)));
    job(&world).keep_table_current().await.unwrap();
    assert_eq!(through(&world).await, Some(day(AUCKLAND_DAY)));
    assert_every_day_once(&row_days(&world, account).await, "2026-10-29", AUCKLAND_DAY);
}

/// There and back on one day: the table ends on the day that is today each time.
#[tokio::test]
async fn there_and_back_ends_where_it_began() {
    let world = world_at(AUCKLAND, AUCKLAND_DAY).await;
    let account = world.holding("Checking", "USD").await;
    world.balance(account, "2026-10-29", 100).await;
    let job = job(&world);

    set_zone(&world, json!(LOS_ANGELES)).await;
    job.keep_table_current().await.unwrap();
    set_zone(&world, Value::Null).await;
    job.keep_table_current().await.unwrap();

    assert_eq!(through(&world).await, Some(day(AUCKLAND_DAY)));
    assert_every_day_once(&row_days(&world, account).await, "2026-10-29", AUCKLAND_DAY);
}

/// A table still waiting for its rebuild is left to the job, which uses the new zone.
#[tokio::test]
async fn a_rebuild_still_to_come_uses_the_new_zone() {
    let world = unbuilt_at(AUCKLAND).await;

    set_zone(&world, json!(LOS_ANGELES)).await;

    let state = world.server.store.net_worth_state().await.unwrap();
    assert!(state.rebuild_needed);
    job(&world).keep_table_current().await.unwrap();
    assert_eq!(through(&world).await, Some(day(LOS_ANGELES_DAY)));
}

/// A balance dated a day that is now still to come is out of the report until it comes,
/// and nothing fails meanwhile.
#[tokio::test]
async fn reports_answer_with_a_balance_on_a_day_still_to_come() {
    let world = world_at(AUCKLAND, AUCKLAND_DAY).await;
    let account = world.holding("Checking", "USD").await;
    world.balance(account, "2026-10-29", 100).await;
    world.balance(account, AUCKLAND_DAY, 200).await;

    set_zone(&world, json!(LOS_ANGELES)).await;

    let report = world.report("").await;
    assert_eq!(report["to"], LOS_ANGELES_DAY);
    assert_eq!(report["through"], LOS_ANGELES_DAY);
    let (status, text) = world
        .send(
            axum::http::Method::GET,
            "/api/v1/forecast",
            &world.admin,
            None,
        )
        .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    let forecast: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(forecast["as_of"], LOS_ANGELES_DAY);
    let accounts = forecast["accounts"].as_array().unwrap();
    assert_eq!(
        accounts[0]["balance"]["minor_units"], 100,
        "the balance as of today"
    );
}
