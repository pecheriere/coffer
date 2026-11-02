//! Rates through the API: missing ones are said, old ones are said, and what can't be
//! converted is never dropped (spec §8.10 N5).

use std::collections::HashMap;

use axum::http::StatusCode;
use coffer_domain::accounts::AccountKind;
use coffer_domain::fx::{FxErrorCode, FxFetchStatus};
use coffer_domain::identity::{Actor, Role, Via};
use coffer_domain::ids::{AccountId, MemberId};
use coffer_domain::money::Currency;
use coffer_server::cli;
use coffer_server::config::Config;
use coffer_server::ledger::accounts::NewAccountInput;
use coffer_server::testkit::TestServer;
use serde_json::{Value, json};

use super::{
    TODAY, World, holder_point, money, of_kind, point, rate_missing, usd, world, world_on,
};

const ONE_DAY: &str = "from=2026-11-02&to=2026-11-02&interval=day";

/// Display USD with its rate, and a checking account holding `minor` THB since 10-26,
/// whose rates nothing has fetched.
async fn with_baht(world: &World, minor: i64) -> AccountId {
    world.display("USD").await;
    world.rates("USD", &[("2026-10-26", "1.1188")]).await;
    let baht = world.holding("Baht", "THB").await;
    world.balance(baht, "2026-10-26", minor).await;
    baht
}

#[tokio::test]
async fn a_missing_rate_is_reported_not_omitted() {
    let world = world().await;
    let baht = with_baht(&world, 50_000).await;

    let report = world.report(ONE_DAY).await;
    let today = point(&report, TODAY);
    assert_eq!(today["total"], Value::Null);
    assert_eq!(today["by_liquidity"][0]["amount"], Value::Null);
    assert_eq!(today["by_currency"], json!([money(50_000, "THB")]));
    assert_eq!(today["unconverted"], json!([baht]));
    assert_eq!(report["status"], "provisional");
    assert_eq!(
        *rate_missing(&report, "THB"),
        json!({"kind": "rate_missing", "currency": "THB", "reason": "not_fetched_yet",
               "first_on": TODAY, "last_on": TODAY})
    );

    world
        .fetch_failed("THB", FxFetchStatus::NotPublished, None)
        .await;
    let report = world.report(ONE_DAY).await;
    assert_eq!(rate_missing(&report, "THB")["reason"], "not_published");

    world
        .fetch_failed("THB", FxFetchStatus::Failing, Some(FxErrorCode::Network))
        .await;
    let report = world.report(ONE_DAY).await;
    assert_eq!(rate_missing(&report, "THB")["reason"], "fetch_failing");
}

#[tokio::test]
async fn a_server_with_no_provider_says_so() {
    let world = world_on(TestServer::with_env(&[("COFFER_FX_PROVIDER", "none")]).await).await;
    with_baht(&world, 50_000).await;

    let report = world.report(ONE_DAY).await;

    assert_eq!(rate_missing(&report, "THB")["reason"], "provider_off");
}

/// What production builds, not the testkit's copy: `open_state` with this configuration
/// answers a rate missing for a holding of THB, on the system clock's day.
async fn reason_in_production(vars: &[(&str, &str)]) -> Value {
    let dir = tempfile::tempdir().unwrap();
    // `open_state` refuses a data directory others can read.
    #[cfg(unix)]
    std::fs::set_permissions(
        dir.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();
    let mut env = HashMap::from([
        ("COFFER_PUBLIC_URL", "https://coffer.example".to_owned()),
        ("COFFER_DATA_DIR", dir.path().to_str().unwrap().to_owned()),
    ]);
    env.extend(vars.iter().map(|&(name, value)| (name, value.to_owned())));
    let config = Config::from_env(|name| env.get(name).cloned()).unwrap();
    let state = cli::open_state(config).await.unwrap();
    let alice = Actor {
        member: MemberId::new(),
        role: Role::Admin,
        via: Via::Session,
    };
    let today = state
        .clock
        .now()
        .to_zoned(state.config.timezone.clone())
        .date();
    state
        .ledger
        .update_forecast_settings(
            &alice,
            serde_json::from_value(json!({"display_currency": "USD"})).unwrap(),
        )
        .await
        .unwrap();
    let mut tx = state.store.begin().await.unwrap();
    tx.rebuild_net_worth(today).await.unwrap();
    tx.commit().await.unwrap();
    let holder = state
        .ledger
        .create_account(
            &alice,
            NewAccountInput {
                name: "Baht".to_owned(),
                kind: AccountKind::Checking,
                currency: Currency::parse("THB").unwrap(),
                liquidity: None,
                include_in_net_worth: Some(true),
                include_in_forecast: Some(true),
                belongs_to: None,
                beneficiary: None,
                opening_balance: None,
            },
            None,
        )
        .await
        .unwrap();
    state
        .ledger
        .set_manual_balance(&alice, holder.id, today, 50_000)
        .await
        .unwrap();
    let report = state
        .ledger
        .net_worth(
            &alice,
            serde_json::from_value(json!({"interval": "day", "from": today.to_string()})).unwrap(),
        )
        .await
        .unwrap();
    let report = serde_json::to_value(report).unwrap();
    rate_missing(&report, "THB")["reason"].clone()
}

#[tokio::test]
async fn production_says_when_no_provider_is_configured() {
    let reason = reason_in_production(&[("COFFER_FX_PROVIDER", "none")]).await;

    assert_eq!(reason, "provider_off");
}

#[tokio::test]
async fn production_fetches_rates_by_default() {
    let reason = reason_in_production(&[]).await;

    assert_eq!(reason, "not_fetched_yet");
}

#[tokio::test]
async fn a_day_before_the_first_rate_is_none_that_early() {
    let world = world().await;
    let baht = with_baht(&world, 50_000).await;
    world.rates("THB", &[("2026-10-29", "36.5")]).await;

    let report = world
        .report("from=2026-10-27&to=2026-10-29&interval=day")
        .await;

    assert_eq!(
        *rate_missing(&report, "THB"),
        json!({"kind": "rate_missing", "currency": "THB", "reason": "none_that_early",
               "first_on": "2026-10-27", "last_on": "2026-10-28"})
    );
    assert_eq!(
        holder_point(&report, baht, "2026-10-29")["conversion"],
        "converted"
    );
}

/// Rates are held, so a failing fetch can't be why one this early is missing.
#[tokio::test]
async fn a_failing_fetch_does_not_explain_a_day_before_the_first_rate() {
    let world = world().await;
    with_baht(&world, 50_000).await;
    world.rates("THB", &[("2026-10-29", "36.5")]).await;
    world
        .fetch_failed("THB", FxFetchStatus::Failing, Some(FxErrorCode::Network))
        .await;

    let report = world
        .report("from=2026-10-27&to=2026-10-29&interval=day")
        .await;

    assert_eq!(rate_missing(&report, "THB")["reason"], "none_that_early");
}

#[tokio::test]
async fn a_discontinued_currency_says_so() {
    let world = world().await;
    world.display("USD").await;
    world
        .rates("USD", &[("2022-03-01", "1.1"), ("2026-10-30", "1.1172")])
        .await;
    world.rates("RUB", &[("2022-03-01", "117.2")]).await;
    let rouble = world.holding("Rouble", "RUB").await;
    world.balance(rouble, "2022-01-03", 117_200).await;

    let report = world.report(ONE_DAY).await;

    assert_ne!(
        point(&report, TODAY)["total"],
        Value::Null,
        "still an amount"
    );
    assert_eq!(
        of_kind(&report, "cautions", "rate_stale"),
        [
            &json!({"kind": "rate_stale", "currency": "RUB", "on": TODAY,
                 "rate_on": "2022-03-01", "age_days": 1_707})
        ]
    );
    assert_eq!(report["status"], "provisional");
}

#[tokio::test]
async fn weekend_points_report_friday() {
    let world = world().await;
    let golden = world.golden().await;

    let report = world
        .report("from=2026-10-31&to=2026-11-01&interval=day")
        .await;

    for on in ["2026-10-31", "2026-11-01"] {
        assert_eq!(
            holder_point(&report, golden.livret, on)["rate_on"],
            "2026-10-30"
        );
    }
    assert_eq!(report["status"], "firm");
}

#[tokio::test]
async fn a_display_currency_with_no_rates_reports_every_foreign_account() {
    let world = world().await;
    let golden = world.golden().await;

    world.display("THB").await;

    let report = world.report(ONE_DAY).await;
    let today = point(&report, TODAY);
    assert_eq!(today["total"], Value::Null);
    assert_eq!(
        today["unconverted"],
        json!([golden.checking, golden.livret, golden.card])
    );
    assert_eq!(rate_missing(&report, "THB")["reason"], "not_fetched_yet");
    assert_eq!(
        of_kind(&report, "cautions", "rate_missing").len(),
        1,
        "the rate lacking is the display currency's alone"
    );
}

#[tokio::test]
async fn display_currency_not_set_has_sums_only() {
    let world = world().await;
    let holder = world.holding("Checking", "USD").await;
    world.balance(holder, "2026-10-26", 700).await;

    let report = world.report(ONE_DAY).await;

    assert_eq!(report["display_currency"], Value::Null);
    assert_eq!(report["status"], "unavailable");
    assert_eq!(
        report["blockers"],
        json!([{"kind": "display_currency_not_set"}])
    );
    let today = point(&report, TODAY);
    assert_eq!(today["total"], Value::Null);
    assert_eq!(today["by_currency"], json!([usd(700)]));
    assert_eq!(today["unconverted"], json!([]));
}

#[tokio::test]
async fn amount_too_large_is_a_caution_not_a_failure() {
    let world = world().await;
    world.display("JPY").await;
    world.rates("JPY", &[("2026-10-26", "175.68")]).await;
    let big = world.holding("Big", "EUR").await;
    world.balance(big, "2026-10-26", i64::MAX).await;

    let report = world.report(ONE_DAY).await;

    let today = point(&report, TODAY);
    assert_eq!(today["total"], Value::Null);
    assert_eq!(today["unconverted"], json!([big]));
    assert_eq!(
        of_kind(&report, "cautions", "amount_too_large"),
        [&json!({"kind": "amount_too_large", "account": big, "on": TODAY})]
    );
}

#[tokio::test]
async fn a_total_too_large_is_422() {
    let world = world().await;
    world.display("USD").await;
    for name in ["One", "Two"] {
        let holder = world.holding(name, "USD").await;
        world.balance(holder, "2026-10-26", i64::MAX).await;
    }

    let (status, text) = world.report_as(&world.admin, ONE_DAY).await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let body: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        body["detail"],
        "the report's totals are too large to represent"
    );
}
