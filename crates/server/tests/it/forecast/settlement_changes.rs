//! A settlement doesn't outlive the facts it was made on (spec §8.9 R9a): changing what an
//! item is owed or when removes its settlements from the household's current month on, so
//! nobody is taken to have said what they didn't.

use axum::http::{Method, StatusCode};
use coffer_server::testkit::TestServer;
use serde_json::{Value, json};

use super::settled::holding;
use super::{ITEMS, World, line, monthly, one_off_on, outflow, usd, world_on};

impl World {
    /// Patches `item`, which must succeed; returns the item.
    async fn patched(&self, item: &Value, patch: &Value) -> Value {
        let uri = format!("{ITEMS}/{}", item.as_str().unwrap());
        self.admin_send(Method::PATCH, &uri, patch).await
    }

    /// The whole of `item` as the admin token reads it.
    async fn read_item(&self, item: &Value) -> Value {
        let uri = format!("{ITEMS}/{}", item.as_str().unwrap());
        let (status, text) = self.send(Method::GET, &uri, &self.admin, None).await;
        assert_eq!(status, StatusCode::OK);
        serde_json::from_str(&text).unwrap()
    }
}

/// What the forecast's `month` plans to pay.
fn outflows_in<'a>(forecast: &'a Value, month: &str) -> &'a Value {
    let found = forecast["months"]
        .as_array()
        .unwrap()
        .iter()
        .find(|bucket| bucket["month"] == month)
        .unwrap();
    &found["lines"][0]["outflows"]
}

/// 100.00 a month on the 20th, with December settled, leaves 200.00 of the window's three.
/// Nobody said 5,000.00 was paid, so at 5,000.00 all three count: 15,000.00 of 10,000.00.
#[tokio::test]
async fn a_new_amount_ends_the_settlement() {
    let (world, _) = holding().await;
    let housing = world.expense("Housing").await;
    let rent = world
        .item(&outflow("Rent", 10_000, housing, &monthly("2026-01-20")))
        .await;
    world.settle(&rent, "2026-12", "paid").await;
    assert_eq!(world.forecast().await["surplus"], usd(980_000));

    let answer = world.patched(&rent, &json!({"amount": usd(500_000)})).await;

    assert_eq!(answer["settlements"], json!([]), "the answer shows it");
    let forecast = world.forecast().await;
    assert_eq!(forecast["surplus"], usd(-500_000));
    assert_eq!(*outflows_in(&forecast, "2026-12"), usd(500_000));
    assert_eq!(forecast["settled"], json!([]));
}

/// A one-off settled for December, moved to January: it counts there. Moved back, nobody
/// has said anything of it since, so it still counts, and the settlement doesn't return.
#[tokio::test]
async fn a_one_off_moved_away_and_back_stays_counted() {
    let (world, _) = holding().await;
    let home = world.expense("Home").await;
    let boiler = world
        .item(&outflow("Boiler", 50_000, home, &one_off_on("2026-12-10")))
        .await;
    world.settle(&boiler, "2026-12", "paid").await;
    assert_eq!(world.forecast().await["surplus"], usd(1_000_000));

    world
        .patched(&boiler, &json!({"schedule": one_off_on("2027-01-10")}))
        .await;
    let moved = world.forecast().await;
    assert_eq!(moved["surplus"], usd(950_000));
    assert_eq!(*outflows_in(&moved, "2027-01"), usd(50_000));

    world
        .patched(&boiler, &json!({"schedule": one_off_on("2026-12-10")}))
        .await;
    let back = world.forecast().await;
    assert_eq!(back["surplus"], usd(950_000));
    assert_eq!(*outflows_in(&back, "2026-12"), usd(50_000));
    assert_eq!(back["settled"], json!([]));
    assert_eq!(world.read_item(&boiler).await["settlements"], json!([]));
}

/// A monthly bill settled for December, made weekly from 2026-11-16: 3 in November, 4 in
/// December, 4 in January and 2 before 2027-02-15, 13 at 100.00. A month's single payment
/// said nothing of four.
#[tokio::test]
async fn a_new_cadence_ends_the_settlement() {
    let (world, _) = holding().await;
    let home = world.expense("Home").await;
    let cleaner = world
        .item(&outflow("Cleaner", 10_000, home, &monthly("2026-01-20")))
        .await;
    world.settle(&cleaner, "2026-12", "skipped").await;

    world
        .patched(
            &cleaner,
            &json!({"schedule": {
                "type": "recurring", "every": 1, "unit": "weeks", "anchor_on": "2026-11-16",
            }}),
        )
        .await;

    let forecast = world.forecast().await;
    assert_eq!(line(&forecast, "Cleaner")["occurrences"], 13);
    assert_eq!(forecast["surplus"], usd(870_000));
    assert_eq!(*outflows_in(&forecast, "2026-12"), usd(40_000));
    assert_eq!(forecast["settled"], json!([]));
}

/// 02:00 UTC on New Year's Day is still 31 December in Los Angeles, so the household's
/// month is 2026-12: 2028-01 is thirteen months off, where in UTC it would be twelve.
async fn new_years_eve_in_los_angeles() -> (World, Value) {
    let server = TestServer::with_env(&[("COFFER_TIMEZONE", "America/Los_Angeles")]).await;
    let world = world_on(server).await;
    world.floor(0).await;
    let home = world.expense("Home").await;
    let item = world
        .item(&outflow("Rent", 10_000, home, &monthly("2020-01-20")))
        .await;
    world
        .server
        .clock
        .set("2027-01-01T02:00:00Z".parse().unwrap());
    (world, item)
}

#[tokio::test]
async fn the_window_is_counted_from_the_households_month() {
    let (world, item) = new_years_eve_in_los_angeles().await;
    let uri = |month: &str| format!("{ITEMS}/{}/settlements/{month}", item.as_str().unwrap());
    let body = json!({"kind": "paid"});
    let status_of = async |month: &str| {
        world
            .send(Method::PUT, &uri(month), &world.admin, Some(&body))
            .await
            .0
    };

    assert_eq!(status_of("2025-12").await, StatusCode::OK, "twelve back");
    assert_eq!(status_of("2027-12").await, StatusCode::OK, "twelve on");
    assert_eq!(
        status_of("2028-01").await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "thirteen on"
    );
}

/// In the household's zone it is still December, so a change on New Year's Eve ends
/// December's settlement, and November's is history.
#[tokio::test]
async fn the_current_month_is_the_households() {
    let (world, item) = new_years_eve_in_los_angeles().await;
    world.settle(&item, "2026-11", "paid").await;
    world.settle(&item, "2026-12", "paid").await;

    let answer = world.patched(&item, &json!({"amount": usd(20_000)})).await;

    let months: Vec<&Value> = answer["settlements"]
        .as_array()
        .unwrap()
        .iter()
        .map(|one| &one["month"])
        .collect();
    assert_eq!(months, [&json!("2026-11")]);
}

/// Choosing a zone moves the household's month at once: from Auckland it is already
/// January, so 2028-01 is twelve months off and December is no longer the current month.
#[tokio::test]
async fn a_chosen_zone_moves_the_month_with_no_restart() {
    let (world, item) = new_years_eve_in_los_angeles().await;
    world.settle(&item, "2026-12", "paid").await;
    world.settle(&item, "2027-01", "paid").await;
    let thirteen_on = format!("{ITEMS}/{}/settlements/2028-01", item.as_str().unwrap());
    let body = json!({"kind": "paid"});
    assert_eq!(
        world
            .send(Method::PUT, &thirteen_on, &world.admin, Some(&body))
            .await
            .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );

    world
        .settings(&json!({"timezone": "Pacific/Auckland"}))
        .await;

    assert_eq!(
        world
            .send(Method::PUT, &thirteen_on, &world.admin, Some(&body))
            .await
            .0,
        StatusCode::OK
    );
    let answer = world.patched(&item, &json!({"amount": usd(20_000)})).await;
    let months: Vec<&Value> = answer["settlements"]
        .as_array()
        .unwrap()
        .iter()
        .map(|one| &one["month"])
        .collect();
    assert_eq!(months, [&json!("2026-12")], "history in the new zone");
}
