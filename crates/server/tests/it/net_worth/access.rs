//! Who sees what of net worth (spec §8.10 N2): the sum of the accounts visible at
//! `balances`, unmarked, and nothing in the answer depends on any other account. The
//! tokens' answers are compared whole, before and after what they can't see changes.

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use coffer_domain::fx::{FxErrorCode, FxFetchStatus};
use coffer_domain::identity::Scope;
use coffer_domain::ids::AccountId;
use coffer_domain::policy::AccountLevel::{Balances, Transactions};
use coffer_server::http::router;
use coffer_server::secrets::SecretToken;
use serde_json::{Value, json};
use tower::ServiceExt as _;

use super::{REPORT, TODAY, World, all, money, of_kind, point, series, usd, world};

const QUERY: &str = "from=2026-10-26&to=2026-11-02&interval=day";

/// Display USD, a visible USD account holding 1,000, and a token seeing only it.
async fn visible_usd(world: &World) -> (AccountId, SecretToken) {
    world.display("USD").await;
    let visible = world.holding("Visible", "USD").await;
    world.balance(visible, "2026-10-26", 100_000).await;
    let token = world.sees_only(visible).await;
    (visible, token)
}

fn parsed(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

#[tokio::test]
async fn a_grant_at_balances_reads_its_sum() {
    let world = world().await;
    let (visible, token) = visible_usd(&world).await;

    let (status, text) = world.report_as(&token, QUERY).await;

    assert_eq!(status, StatusCode::OK);
    let report = parsed(&text);
    assert_eq!(report["status"], "firm");
    assert_eq!(point(&report, TODAY)["total"], usd(100_000));
    assert_eq!(series(&report, visible)["name"], "Visible");
}

/// The sum is of what the token sees, and nothing says there is more.
#[tokio::test]
async fn a_grant_reads_only_its_sum_unmarked() {
    let world = world().await;
    let (_, token) = visible_usd(&world).await;
    let hidden = world.holding("Hidden", "USD").await;
    world.balance(hidden, "2026-10-26", 700).await;

    let (_, restricted) = world.report_as(&token, QUERY).await;
    let full = world.report(QUERY).await;

    assert_eq!(point(&parsed(&restricted), TODAY)["total"], usd(100_000));
    assert_eq!(point(&full, TODAY)["total"], usd(100_700));
    assert_eq!(parsed(&restricted)["status"], "firm");
}

#[tokio::test]
async fn all_at_balances_reads_the_whole_sum() {
    let world = world().await;
    visible_usd(&world).await;
    let other = world.holding("Other", "USD").await;
    world.balance(other, "2026-10-26", 700).await;
    let token = world.token(Scope::Read, vec![all(Balances)]).await;

    let (_, text) = world.report_as(&token, QUERY).await;

    assert_eq!(point(&parsed(&text), TODAY)["total"], usd(100_700));
}

/// The named case: a hidden account in another currency, with its rates fetched, its
/// coverage on record and the visible currency's too, which only it made necessary.
#[tokio::test]
async fn a_hidden_currency_changes_nothing() {
    let world = world().await;
    let (_, token) = visible_usd(&world).await;
    let before = world.report_as(&token, QUERY).await;

    let hidden = world.holding("Hidden", "JPY").await;
    world.balance(hidden, "2026-10-20", 7_777_777).await;
    world.rates("JPY", &[("2026-10-01", "175.68")]).await;
    world.rates("USD", &[("2026-09-01", "1.1188")]).await;

    assert_eq!(before.0, StatusCode::OK);
    assert!(
        world.report_as(&token, QUERY).await == before,
        "byte-identical"
    );
    assert!(!before.1.contains("JPY"));
}

#[tokio::test]
async fn a_hidden_missing_rate_changes_nothing() {
    let world = world().await;
    let (_, token) = visible_usd(&world).await;
    let before = world.report_as(&token, QUERY).await;

    let hidden = world.holding("Hidden", "THB").await;
    world.balance(hidden, "2026-10-26", 50_000).await;
    world
        .fetch_failed("THB", FxFetchStatus::Failing, Some(FxErrorCode::Network))
        .await;

    assert_eq!(
        point(&world.report(QUERY).await, TODAY)["total"],
        Value::Null,
        "it would have changed the answer"
    );
    assert!(
        world.report_as(&token, QUERY).await == before,
        "byte-identical"
    );
}

#[tokio::test]
async fn a_hidden_hole_changes_nothing() {
    let world = world().await;
    let (_, token) = visible_usd(&world).await;
    let before = world.report_as(&token, QUERY).await;

    let hidden = world.holding("Hidden", "USD").await;
    world.balance(hidden, "2026-10-26", 700).await;
    world
        .server
        .store
        .execute_unchecked_for_tests(
            "DELETE FROM account_daily_values WHERE account_id = ? AND on_date = '2026-11-02'",
            &hidden.to_string(),
        )
        .await
        .unwrap();

    let full = world.report(QUERY).await;
    assert_eq!(of_kind(&full, "cautions", "daily_value_missing").len(), 1);
    assert!(
        world.report_as(&token, QUERY).await == before,
        "byte-identical"
    );
}

/// A sum past the limit is 422 for a token whose sum it is, and for no other.
#[tokio::test]
async fn a_hidden_total_too_large_changes_nothing() {
    let world = world().await;
    let (_, token) = visible_usd(&world).await;
    let before = world.report_as(&token, QUERY).await;

    for name in ["Huge", "Vast"] {
        let hidden = world.holding(name, "USD").await;
        world.balance(hidden, "2026-10-26", i64::MAX).await;
    }

    let (full, _) = world.report_as(&world.admin, QUERY).await;
    assert_eq!(full, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        world.report_as(&token, QUERY).await == before,
        "byte-identical"
    );
}

/// Everything about a hidden account that could reach an answer: an earlier first day,
/// closing, owing, a positive liability and an old balance.
#[tokio::test]
async fn a_hidden_account_s_dates_and_cautions_change_nothing() {
    let world = world().await;
    let (_, token) = visible_usd(&world).await;
    let before = world.report_as(&token, QUERY).await;

    let early = world.holding("Early", "USD").await;
    world.balance(early, "1999-01-04", 5).await;
    let closed = world.holding("Closed", "USD").await;
    world.balance(closed, "2026-10-26", -5).await;
    world.patch_holding(closed, &json!({"closed": true})).await;
    let wrong_sign = world
        .holding_with(
            "Wrong",
            "credit_card",
            "USD",
            json!({"liquidity": "short_term_liability"}),
        )
        .await;
    world.balance(wrong_sign, "2026-10-26", 5).await;

    assert!(
        world.report_as(&token, QUERY).await == before,
        "byte-identical"
    );
}

/// What counts for the token is decided by what it sees: with nothing of its own
/// counted, it has no number, whatever else counts.
#[tokio::test]
async fn nothing_counted_that_it_sees_is_no_number() {
    let world = world().await;
    world.display("USD").await;
    let excluded = world
        .holding_with(
            "Off",
            "checking",
            "USD",
            json!({"include_in_net_worth": false}),
        )
        .await;
    world.balance(excluded, "2026-10-26", 5).await;
    let token = world.sees_only(excluded).await;
    let before = world.report_as(&token, QUERY).await;

    let hidden = world.holding("Hidden", "USD").await;
    world.balance(hidden, "2026-10-26", 700).await;

    let report = parsed(&before.1);
    assert_eq!(report["blockers"], json!([{"kind": "no_counted_accounts"}]));
    assert_eq!(report["points"], json!([]));
    assert!(
        world.report_as(&token, QUERY).await == before,
        "byte-identical"
    );
}

/// A visible account's own missing rate is named, whatever the token.
#[tokio::test]
async fn a_visible_missing_rate_is_named() {
    let world = world().await;
    world.display("USD").await;
    world.rates("USD", &[("2026-10-26", "1.1188")]).await;
    let baht = world.holding("Baht", "THB").await;
    world.balance(baht, "2026-10-26", 50_000).await;
    let token = world.sees_only(baht).await;

    let (_, text) = world.report_as(&token, QUERY).await;

    let report = parsed(&text);
    let today = point(&report, TODAY);
    assert_eq!(today["total"], Value::Null);
    assert_eq!(today["by_currency"], json!([money(50_000, "THB")]));
    assert_eq!(today["unconverted"], json!([baht]));
    assert_eq!(of_kind(&report, "cautions", "rate_missing").len(), 1);
}

/// A visible hole is named too, and doesn't name a hidden one beside it.
#[tokio::test]
async fn a_visible_hole_is_named_alone() {
    let world = world().await;
    let (visible, token) = visible_usd(&world).await;
    let hidden = world.holding("Hidden", "USD").await;
    world.balance(hidden, "2026-10-26", 700).await;
    for holder in [visible, hidden] {
        world
            .server
            .store
            .execute_unchecked_for_tests(
                "DELETE FROM account_daily_values WHERE account_id = ? AND on_date = '2026-11-02'",
                &holder.to_string(),
            )
            .await
            .unwrap();
    }

    let (_, text) = world.report_as(&token, QUERY).await;

    let report = parsed(&text);
    let today = point(&report, TODAY);
    assert_eq!(today["total"], Value::Null);
    assert_eq!(today["unconverted"], json!([visible]));
    assert_eq!(
        of_kind(&report, "cautions", "daily_value_missing"),
        [&json!({"kind": "daily_value_missing", "account": visible,
                 "first_on": TODAY, "last_on": TODAY})]
    );
}

/// The refusal of a range is the same for every token, so it can't depend on what each
/// sees.
#[tokio::test]
async fn a_refused_range_reads_the_same_for_every_token() {
    let world = world().await;
    let (_, token) = visible_usd(&world).await;
    let wide = world.token(Scope::Read, vec![all(Transactions)]).await;
    let query = "from=2020-01-01&to=2026-11-02&interval=day";

    let restricted = world.report_as(&token, query).await;

    assert_eq!(restricted.0, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        world.report_as(&wide, query).await == restricted,
        "byte-identical"
    );
}

#[tokio::test]
async fn no_token_is_401() {
    let world = world().await;

    for uri in [
        REPORT,
        "/api/v1/fx/coverage",
        "/api/v1/fx/rates?currency=USD&from=2026-10-26&to=2026-10-30",
    ] {
        let request = Request::builder()
            .method(Method::GET)
            .uri(uri)
            .body(Body::empty())
            .unwrap();
        let response = router(world.server.app_state())
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}
