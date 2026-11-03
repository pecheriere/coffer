//! The household's time zone in its settings (spec §8.7), through the real router.
//!
//! At [`AT`] Auckland is on 2026-11-01 and Los Angeles on 2026-10-31: another day and
//! another month, which is the most a change of zone can do.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

mod answers;
mod page;
mod settings;
mod table;

use axum::http::Method;
use coffer_server::testkit::TestServer;
use jiff::Timestamp;
use serde_json::{Value, json};

use crate::net_worth::{World, unbuilt, world_on};

const SETTINGS: &str = "/api/v1/household/settings";
const AUCKLAND: &str = "Pacific/Auckland";
const LOS_ANGELES: &str = "America/Los_Angeles";
/// Auckland's day at [`AT`].
const AUCKLAND_DAY: &str = "2026-11-01";
/// Los Angeles' day at [`AT`].
const LOS_ANGELES_DAY: &str = "2026-10-31";

fn at() -> Timestamp {
    "2026-11-01T01:00:00Z".parse().unwrap()
}

/// A household whose server says `default_zone`, at [`AT`], with a table that ends on
/// `through`.
async fn world_at(default_zone: &str, through: &str) -> World {
    let server = TestServer::with_env(&[("COFFER_TIMEZONE", default_zone)]).await;
    let world = world_on(server).await;
    world.server.clock.set(at());
    world.rebuild(through).await;
    world
}

/// As [`world_at`], before the table's first rebuild.
async fn unbuilt_at(default_zone: &str) -> World {
    let server = TestServer::with_env(&[("COFFER_TIMEZONE", default_zone)]).await;
    let world = unbuilt(server).await;
    world.server.clock.set(at());
    world
}

/// Sets the zone as an admin, or clears it with `null`; the answer.
async fn set_zone(world: &World, zone: Value) -> Value {
    world
        .admin_send(Method::PATCH, SETTINGS, &json!({"timezone": zone}))
        .await
}
