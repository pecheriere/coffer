//! A rate entered by hand against the household's day: what counts as today is the zone
//! chosen now, with no restart, and a rate on a day the table hasn't reached waits for it.

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use coffer_domain::fx::RateLeg;
use coffer_server::fx::FxService;
use serde_json::json;

use super::{AUCKLAND, AUCKLAND_DAY, LOS_ANGELES, LOS_ANGELES_DAY, set_zone, world_at};
use crate::net_worth::World;

async fn enter(world: &World, currency: &str, on: &str) -> StatusCode {
    world
        .send(
            Method::PUT,
            &format!("/api/v1/fx/manual-rates/{currency}/{on}"),
            &world.admin,
            Some(&json!({"rate": "655.957"})),
        )
        .await
        .0
}

#[tokio::test]
async fn a_rate_s_last_day_follows_the_zone_chosen_now() {
    let world = world_at(LOS_ANGELES, LOS_ANGELES_DAY).await;

    assert_eq!(enter(&world, "XAF", LOS_ANGELES_DAY).await, StatusCode::OK);
    assert_eq!(
        enter(&world, "XAF", AUCKLAND_DAY).await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "tomorrow here"
    );

    set_zone(&world, json!(AUCKLAND)).await;
    assert_eq!(enter(&world, "XAF", AUCKLAND_DAY).await, StatusCode::OK);

    set_zone(&world, json!(LOS_ANGELES)).await;
    assert_eq!(
        enter(&world, "XOF", AUCKLAND_DAY).await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "and back, with no restart"
    );
}

/// The table runs to yesterday when the zone moves on: the rate writes no row past it, and
/// the job filling the next day uses it.
#[tokio::test]
async fn a_rate_on_a_day_the_table_has_not_reached_applies_when_it_does() {
    let world = world_at(LOS_ANGELES, LOS_ANGELES_DAY).await;
    let held = world.holding("Abroad", "XAF").await;
    world.balance(held, LOS_ANGELES_DAY, 655_957).await;
    world.display("EUR").await;
    set_zone(&world, json!(AUCKLAND)).await;

    assert_eq!(enter(&world, "XAF", AUCKLAND_DAY).await, StatusCode::OK);

    let job = Arc::new(FxService::new(
        world.server.store.clone(),
        world.server.clock.clone(),
        world.server.config.default_timezone.clone(),
        None,
    ));
    job.keep_table_current().await.unwrap();
    let rows = world.server.store.daily_values_for_tests().await.unwrap();
    let on_the_day = rows
        .iter()
        .find(|row| row.account == held && row.on.to_string() == AUCKLAND_DAY)
        .unwrap();
    assert_eq!(on_the_day.display_minor, Some(100_000));
    assert_eq!(on_the_day.manual_rate_leg, Some(RateLeg::Held));
}
