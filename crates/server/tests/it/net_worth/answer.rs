//! The answer itself: Task 1's golden household through the API, the range, and what
//! applies at read. No test here restricts a token: `access.rs` does.

use axum::http::{Method, StatusCode};
use coffer_server::testkit::TestServer;
use serde_json::{Value, json};

use super::{
    Golden, TODAY, days, holder_point, money, of_kind, point, series, unbuilt, usd, world, world_on,
};

const GOLDEN_WEEK: &str = "from=2026-10-26&to=2026-11-01&interval=day";

/// The `total` of each of `report`'s points, oldest first.
fn totals(report: &Value) -> Vec<i64> {
    report["points"]
        .as_array()
        .unwrap()
        .iter()
        .map(|point| point["total"]["minor_units"].as_i64().unwrap())
        .collect()
}

#[tokio::test]
async fn the_api_matches_the_golden_week() {
    let world = world().await;
    let Golden {
        livret, card, side, ..
    } = world.golden().await;

    let report = world.report(GOLDEN_WEEK).await;

    assert_eq!(report["status"], "firm");
    assert_eq!(report["display_currency"], "USD");
    assert_eq!(report["through"], TODAY);
    assert_eq!(
        totals(&report),
        [
            611_880, 611_500, 581_330, 581_300, 631_720, 631_720, 631_720
        ]
    );
    let friday = point(&report, "2026-10-30");
    assert_eq!(
        friday["by_liquidity"],
        json!([
            {"liquidity": "cash", "amount": usd(661_720)},
            {"liquidity": "short_term_liability", "amount": usd(-30_000)},
        ])
    );
    assert_eq!(
        friday["by_currency"],
        json!([money(100_000, "EUR"), usd(520_000)])
    );
    assert_eq!(
        holder_point(&report, livret, "2026-10-30")["rate_on"],
        "2026-10-30"
    );
    assert_eq!(
        holder_point(&report, livret, "2026-10-31")["rate_on"],
        "2026-10-30"
    );
    assert_eq!(
        holder_point(&report, card, "2026-10-31")["balance_on"],
        "2026-10-28"
    );
    assert_eq!(series(&report, side)["not_counted"], "excluded_by_flag");
    assert_eq!(series(&report, side)["points"], json!([]));

    let by_week = world
        .report("from=2026-10-26&to=2026-11-01&interval=week")
        .await;
    assert_eq!(days(&by_week), ["2026-11-01"]);
    assert_eq!(totals(&by_week), [631_720]);
    assert_eq!(by_week["points"][0]["partial"], false);

    let by_month = world
        .report("from=2026-10-26&to=2026-11-01&interval=month")
        .await;
    assert_eq!(days(&by_month), ["2026-10-31", "2026-11-01"]);
    assert_eq!(totals(&by_month), [631_720, 631_720]);
    assert_eq!(by_month["points"][0]["partial"], false);
    assert_eq!(by_month["points"][0]["period_start"], "2026-10-26");
    assert_eq!(by_month["points"][1]["partial"], true);
}

#[tokio::test]
async fn the_golden_week_in_euros() {
    let world = world().await;
    let Golden {
        checking,
        card,
        livret,
        ..
    } = world.golden().await;

    world.display("EUR").await;

    let report = world.report(GOLDEN_WEEK).await;
    assert_eq!(report["display_currency"], "EUR");
    let friday = point(&report, "2026-10-30");
    assert_eq!(friday["total"], money(565_449, "EUR"));
    let row = |holder| holder_point(&report, holder, "2026-10-30");
    assert_eq!(row(checking)["amount"], money(492_302, "EUR"));
    assert_eq!(row(card)["amount"], money(-26_853, "EUR"));
    assert_eq!(row(livret)["amount"], money(100_000, "EUR"));
    assert_eq!(row(livret)["conversion"], "same");
}

#[tokio::test]
async fn defaults_are_a_year_by_month() {
    let world = world().await;
    let Golden { checking, .. } = world.golden().await;

    let report = world.report("").await;

    assert_eq!(report["from"], "2025-11-02");
    assert_eq!(report["to"], TODAY);
    assert_eq!(report["interval"], "month");
    assert_eq!(days(&report), ["2026-10-31", TODAY]);
    assert_eq!(point(&report, TODAY)["partial"], true);
    assert_eq!(
        series(&report, checking)["points"]
            .as_array()
            .unwrap()
            .len(),
        2,
        "points of each holder by default"
    );
}

#[tokio::test]
async fn a_default_from_stays_inside_the_years_answered() {
    let world = world().await;

    let report = world.report("to=1970-03-01").await;

    assert_eq!(report["from"], "1970-01-01");
}

#[tokio::test]
async fn account_points_false_keeps_the_summaries() {
    let world = world().await;
    let Golden { checking, .. } = world.golden().await;

    let report = world
        .report(&format!("{GOLDEN_WEEK}&account_points=false"))
        .await;

    assert_eq!(series(&report, checking)["points"], json!([]));
    assert_eq!(totals(&report).len(), 7);
}

#[tokio::test]
async fn today_uses_the_household_time_zone() {
    // At noon UTC on 11-02, Auckland's day is already 11-03 and it has been filled.
    let world =
        world_on(TestServer::with_env(&[("COFFER_TIMEZONE", "Pacific/Auckland")]).await).await;
    world.rebuild("2026-11-03").await;

    let ahead = world.report("").await;
    assert_eq!(ahead["to"], "2026-11-03");

    let behind =
        world_on(TestServer::with_env(&[("COFFER_TIMEZONE", "Pacific/Honolulu")]).await).await;
    behind
        .server
        .clock
        .set("2026-11-02T05:00:00Z".parse().unwrap());
    let report = behind.report("").await;
    assert_eq!(report["to"], "2026-11-01");
}

#[tokio::test]
async fn to_after_through_is_left_out_and_said() {
    let world = world().await;
    world.golden().await;
    world.rebuild("2026-10-30").await;

    let report = world
        .report("from=2026-10-26&to=2026-11-02&interval=day")
        .await;

    assert_eq!(days(&report).last().unwrap(), "2026-10-30");
    assert_eq!(report["through"], "2026-10-30");
    assert_eq!(report["status"], "provisional");
    assert_eq!(
        of_kind(&report, "cautions", "to_date_not_yet_filled"),
        [&json!({"kind": "to_date_not_yet_filled", "through": "2026-10-30"})]
    );
}

/// After a time zone change the table can hold a day later than today. The report
/// values today; reading up to the table's last day instead would look for a period
/// end that was never read, and call it a hole.
#[tokio::test]
async fn a_fill_ahead_of_today_leaves_no_false_hole() {
    let world = world().await;
    world.golden().await;
    world.fill("2026-11-05").await;

    for query in [
        "from=2026-10-01&to=2026-11-30&interval=month",
        "from=2026-10-19&to=2026-11-30&interval=week",
    ] {
        let report = world.report(query).await;

        assert_eq!(days(&report).last().unwrap(), TODAY);
        assert_eq!(
            report["points"].as_array().unwrap().last().unwrap()["total"],
            usd(631_720)
        );
        assert_eq!(
            of_kind(&report, "cautions", "daily_value_missing"),
            Vec::<&Value>::new()
        );
        assert_eq!(report["status"], "firm");
    }
}

#[tokio::test]
async fn until_the_rebuild_there_is_no_number() {
    let world = unbuilt(TestServer::new().await).await;
    world.golden().await;

    let report = world.report(GOLDEN_WEEK).await;

    assert_eq!(report["status"], "unavailable");
    assert_eq!(report["blockers"], json!([{"kind": "rebuilding"}]));
    assert_eq!(report["points"], json!([]));
    assert_eq!(report["accounts"], json!([]));
}

#[tokio::test]
async fn include_in_net_worth_applies_with_no_rebuild() {
    let world = world().await;
    let Golden { side, .. } = world.golden().await;
    let before = world.rows().await;

    world
        .patch_holding(side, &json!({"include_in_net_worth": true}))
        .await;

    assert!(world.rows().await == before, "no row written");
    let report = world.report(GOLDEN_WEEK).await;
    assert_eq!(point(&report, "2026-10-26")["total"], usd(711_780));
    assert_eq!(point(&report, "2026-10-30")["total"], usd(731_620));
}

#[tokio::test]
async fn changing_liquidity_moves_the_breakdown_with_no_rebuild() {
    let world = world().await;
    let Golden { livret, .. } = world.golden().await;
    let before = world.rows().await;

    world
        .patch_holding(livret, &json!({"liquidity": "invested"}))
        .await;

    assert!(world.rows().await == before, "no row written");
    let report = world.report(GOLDEN_WEEK).await;
    let friday = point(&report, "2026-10-30");
    assert_eq!(
        friday["by_liquidity"],
        json!([
            {"liquidity": "cash", "amount": usd(550_000)},
            {"liquidity": "short_term_liability", "amount": usd(-30_000)},
            {"liquidity": "invested", "amount": usd(111_720)},
        ])
    );
    assert_eq!(friday["total"], usd(631_720));
}

/// 12:00 UTC on 11-01 is already 11-02 in Auckland, so that is the day it closed.
#[tokio::test]
async fn a_closed_account_stops_counting() {
    let world =
        world_on(TestServer::with_env(&[("COFFER_TIMEZONE", "Pacific/Auckland")]).await).await;
    world.display("USD").await;
    let kept = world.holding("Kept", "USD").await;
    world.balance(kept, "2026-10-26", 100).await;
    let closing = world.holding("Closing", "USD").await;
    world.balance(closing, "2026-10-26", 20).await;
    world.at("2026-11-01");
    world.patch_holding(closing, &json!({"closed": true})).await;
    world.at(TODAY);

    let report = world
        .report("from=2026-10-31&to=2026-11-02&interval=day")
        .await;

    assert_eq!(series(&report, closing)["closed_on"], "2026-11-02");
    assert_eq!(point(&report, "2026-11-01")["total"], usd(120));
    assert_eq!(point(&report, "2026-11-02")["total"], usd(100));
}

#[tokio::test]
async fn an_empty_household_has_no_number() {
    let world = world().await;
    world.display("USD").await;

    let report = world.report("").await;

    assert_eq!(report["status"], "unavailable");
    assert_eq!(report["blockers"], json!([{"kind": "no_counted_accounts"}]));
    assert_eq!(report["points"], json!([]));
}

#[tokio::test]
async fn too_many_points_is_422() {
    let world = world().await;

    let (status, text) = world
        .report_as(&world.admin, "from=2020-01-01&to=2026-11-02&interval=day")
        .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let body: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        body["detail"],
        "the range has too many points: at most 1000"
    );
}

/// Whether a range is refused says nothing of what the table holds: refused alike while
/// nothing is filled, while a rebuild is needed and with the range's days all filled.
#[tokio::test]
async fn too_many_points_is_refused_whatever_the_table_holds() {
    const LONG: &str = "from=2020-01-01&to=2026-11-02&interval=day";
    let filled = world().await;
    let unbuilt = unbuilt(TestServer::new().await).await;
    let early = world().await;
    early.rebuild("2020-01-05").await;

    let expected = filled.report_as(&filled.admin, LONG).await;
    assert_eq!(expected.0, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        unbuilt.report_as(&unbuilt.admin, LONG).await == expected,
        "byte-identical"
    );
    assert!(
        early.report_as(&early.admin, LONG).await == expected,
        "byte-identical"
    );
}

#[tokio::test]
async fn inverted_range_is_422() {
    let world = world().await;

    for query in [
        "from=2026-10-30&to=2026-10-26",
        // After today, whatever `to` says, there is no range to answer.
        "from=2026-11-03&to=2026-11-10",
    ] {
        let (status, text) = world.report_as(&world.admin, query).await;

        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let body: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(body["detail"], "the range starts after it ends");
    }
}

#[tokio::test]
async fn a_date_outside_the_years_answered_is_422() {
    let world = world().await;

    for query in [
        "from=1969-12-31&to=1970-01-02",
        "from=2026-10-01&to=2201-01-01",
    ] {
        let (status, text) = world.report_as(&world.admin, query).await;

        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let body: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(body["detail"], "dates must fall in the years 1970 to 2200");
    }
}

#[tokio::test]
async fn a_datetime_is_refused() {
    const SENT: &str = "2026-10-26T00:00:00";
    let world = world().await;

    for name in ["from", "to"] {
        let (status, text) = world
            .report_as(&world.admin, &format!("{name}={SENT}"))
            .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(!text.contains(SENT), "the refusal does not echo");
    }
}

#[tokio::test]
async fn an_unknown_interval_is_400() {
    let world = world().await;

    let (status, text) = world.report_as(&world.admin, "interval=fortnight").await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(!text.contains("fortnight"), "the refusal does not echo");
}

#[tokio::test]
async fn reading_writes_nothing() {
    let world = world().await;
    world.golden().await;
    let (rows, log, state) = (
        world.rows().await,
        world.audit_count().await,
        world.server.store.net_worth_state().await.unwrap(),
    );

    world.report(GOLDEN_WEEK).await;
    world
        .send(Method::GET, "/api/v1/fx/coverage", &world.admin, None)
        .await;

    assert!(world.rows().await == rows, "no row written");
    assert_eq!(world.audit_count().await, log);
    assert_eq!(world.server.store.net_worth_state().await.unwrap(), state);
}

#[tokio::test]
async fn a_new_snapshot_shows_at_once() {
    let world = world().await;
    let Golden { checking, .. } = world.golden().await;
    let before = world
        .report("from=2026-11-02&to=2026-11-02&interval=day")
        .await;
    assert_eq!(before["points"][0]["total"], usd(631_720));

    world.balance(checking, TODAY, 600_000).await;

    let after = world
        .report("from=2026-11-02&to=2026-11-02&interval=day")
        .await;
    assert_eq!(after["points"][0]["total"], usd(681_720));
}
