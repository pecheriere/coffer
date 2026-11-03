//! The FX endpoints (spec §8.10 N2): which rates are stored depends on the currencies in
//! use, so they span every account and need `all` at `balances`.

use axum::http::{Method, StatusCode};
use coffer_domain::fx::FxFetchStatus;
use coffer_domain::identity::Scope;
use coffer_domain::policy::AccountLevel::{Balances, Transactions, Write};
use coffer_domain::policy::TokenGrant;
use coffer_server::secrets::SecretToken;
use serde_json::{Value, json};

use super::{TODAY, World, all, on, world};

const COVERAGE: &str = "/api/v1/fx/coverage";
const RATES: &str = "/api/v1/fx/rates?currency=USD&from=2026-10-26&to=2026-10-30";
const REFUSAL: &str = "this action needs balances access to every account";

impl World {
    async fn on_both_routes(&self, token: &SecretToken) -> [(StatusCode, String); 2] {
        [
            self.send(Method::GET, COVERAGE, token, None).await,
            self.send(Method::GET, RATES, token, None).await,
        ]
    }

    /// Rates and a failure on record.
    async fn with_rates(&self) {
        self.rates("USD", &[("2026-10-26", "1.1188"), ("2026-10-30", "1.1172")])
            .await;
        self.fetch_failed("THB", FxFetchStatus::NotPublished, None)
            .await;
    }
}

fn parsed(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

#[tokio::test]
async fn coverage_and_rates_for_a_full_token() {
    let world = world().await;
    world.with_rates().await;
    let token = world.token(Scope::Read, Vec::new()).await;

    let [(coverage_status, coverage), (rates_status, rates)] = world.on_both_routes(&token).await;

    assert_eq!(coverage_status, StatusCode::OK);
    let at = format!("{TODAY}T12:00:00Z");
    assert_eq!(
        parsed(&coverage),
        json!({"items": [
            {"currency": "THB", "status": "not_published", "first_on": null, "last_on": null,
             "last_attempt_at": at, "last_success_at": null, "last_error": null},
            {"currency": "USD", "status": "ready", "first_on": "2026-10-26",
             "last_on": "2026-10-30", "last_attempt_at": at, "last_success_at": at,
             "last_error": null},
        ], "manual": []})
    );
    assert_eq!(rates_status, StatusCode::OK);
    assert_eq!(
        parsed(&rates),
        json!({"currency": "USD", "items": [
            {"on": "2026-10-30", "rate": "1.1172", "manual": false},
            {"on": "2026-10-26", "rate": "1.1188", "manual": false},
        ]})
    );
}

#[tokio::test]
async fn all_at_balances_may_read() {
    let world = world().await;
    world.with_rates().await;
    let token = world.token(Scope::Read, vec![all(Balances)]).await;

    for (status, _) in world.on_both_routes(&token).await {
        assert_eq!(status, StatusCode::OK);
    }
}

#[tokio::test]
async fn account_grants_are_refused_alike() {
    let bare = world().await;
    let held = world().await;
    let only = bare.holding("Visible", "USD").await;
    let visible = held.holding("Visible", "USD").await;
    let hidden = held.holding("Hidden", "JPY").await;
    held.balance(hidden, "2026-10-26", 7_777_777).await;
    held.with_rates().await;

    let cases = |one| -> [(Scope, Vec<TokenGrant>); 3] {
        [
            (Scope::Read, vec![on(one, Balances)]),
            (Scope::Read, vec![on(one, Transactions)]),
            (Scope::Admin, vec![on(one, Write)]),
        ]
    };
    for ((scope, grants), (other_scope, other_grants)) in
        cases(only).into_iter().zip(cases(visible))
    {
        let for_bare = bare.token(scope, grants).await;
        let for_held = held.token(other_scope, other_grants).await;
        let refused = bare.on_both_routes(&for_bare).await;
        let also = held.on_both_routes(&for_held).await;

        assert!(
            refused == also,
            "byte-identical with and without what is hidden"
        );
        for (status, text) in &also {
            assert_eq!(*status, StatusCode::FORBIDDEN);
            assert_eq!(parsed(text)["detail"], REFUSAL);
        }
    }
}

/// Whether a range is good is not asked of a token that may not read at all.
#[tokio::test]
async fn a_refused_token_learns_nothing_of_its_query() {
    let world = world().await;
    let visible = world.holding("Visible", "USD").await;
    let token = world.sees_only(visible).await;

    let (status, text) = world
        .send(
            Method::GET,
            "/api/v1/fx/rates?currency=USD&from=2020-01-01&to=2026-10-30",
            &token,
            None,
        )
        .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(parsed(&text)["detail"], REFUSAL);
}

#[tokio::test]
async fn range_too_long_is_422() {
    let world = world().await;
    // From 2024-02-06 to 2026-11-01 is 1,000 days, both ends counted.
    let longest = "/api/v1/fx/rates?currency=USD&from=2024-02-06&to=2026-11-01";
    let beyond = "/api/v1/fx/rates?currency=USD&from=2024-02-05&to=2026-11-01";

    let (status, _) = world.send(Method::GET, longest, &world.admin, None).await;
    assert_eq!(status, StatusCode::OK);

    let (status, text) = world.send(Method::GET, beyond, &world.admin, None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(parsed(&text)["detail"], "a range covers at most 1000 days");
}

#[tokio::test]
async fn inverted_range_is_422() {
    let world = world().await;

    let (status, _) = world
        .send(
            Method::GET,
            "/api/v1/fx/rates?currency=USD&from=2026-10-30&to=2026-10-26",
            &world.admin,
            None,
        )
        .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn unknown_currency_is_400() {
    let world = world().await;

    let (status, text) = world
        .send(
            Method::GET,
            "/api/v1/fx/rates?currency=ZZQ&from=2026-10-26&to=2026-10-30",
            &world.admin,
            None,
        )
        .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(!text.contains("ZZQ"), "the refusal does not echo");
}

#[tokio::test]
async fn a_datetime_is_400() {
    let world = world().await;

    let (status, _) = world
        .send(
            Method::GET,
            "/api/v1/fx/rates?currency=USD&from=2026-10-26T00:00:00Z&to=2026-10-30",
            &world.admin,
            None,
        )
        .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}
