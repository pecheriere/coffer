//! Plan versus actual reads each month's plan at the version in force then (money redesign
//! §10.3): a rise made in October leaves September as it was.

use axum::http::Method;
use serde_json::json;

use super::{ITEMS, monthly, noon, row, usd, world};

#[tokio::test]
async fn plan_vs_actual_reads_last_months_version() {
    let world = world().await;
    let home = world.expense("Home").await;
    world.server.clock.set(noon("2026-10-15"));
    let rent = world
        .item(&monthly("Rent", "outflow", 100_000, home, "2026-01-10"))
        .await;
    let uri = format!("{ITEMS}/{}", rent.as_str().unwrap());
    world
        .admin_send(Method::PATCH, &uri, &json!({"amount": usd(120_000)}))
        .await;
    world.server.clock.set(noon(super::TODAY));

    let report = world.report("from=2026-09&months=3").await;

    assert_eq!(row(&report, "2026-09", home)["planned"], usd(100_000));
    assert_eq!(row(&report, "2026-10", home)["planned"], usd(120_000));
    assert_eq!(row(&report, "2026-11", home)["planned"], usd(120_000));
}
