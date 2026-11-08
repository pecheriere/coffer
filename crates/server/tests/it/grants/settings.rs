//! The household's settings under grants (spec §8.9 R2): an emergency fund is a figure of
//! the whole household's plan, so below `all` at `transactions` the list reads as `null`,
//! in the same shape, on a read and in a patch's answer alike.

use super::{World, grant, on, world};
use axum::http::{Method, StatusCode};
use coffer_domain::identity::Scope;
use coffer_domain::money::Currency;
use coffer_domain::plan::EmergencyFundTarget;
use coffer_domain::policy::{AccountLevel, GrantTarget, TokenGrant};
use coffer_server::secrets::SecretToken;
use serde_json::{Value, json};

const SETTINGS: &str = "/api/v1/household/settings";

fn euro_fund() -> EmergencyFundTarget {
    EmergencyFundTarget::Fixed {
        minor_units: 2_500_000,
    }
}

/// The list the settings answer with `euro_fund` set.
fn euro_funds() -> Value {
    json!([{"currency": "EUR", "target": {"kind": "fixed", "minor_units": 2_500_000}}])
}

fn all(level: AccountLevel) -> TokenGrant {
    grant(GrantTarget::All, level)
}

/// The settings as stored here, with `emergency_funds` in their place.
fn settings_with(emergency_funds: &Value) -> Value {
    json!({
        "emergency_funds": emergency_funds,
        "include_income": true,
        "default_horizon_months": 6,
        "stale_after_days": 7,
        "display_currency": null,
        "timezone": "UTC",
        "timezone_setting": null,
    })
}

impl World {
    /// Sets values other than the defaults as alice's session, so that a read can't pass by
    /// showing the defaults, and `target` as the euro's emergency fund if there is one.
    async fn set_settings(&self, target: Option<EmergencyFundTarget>) {
        self.server
            .ledger
            .update_forecast_settings(
                &self.alice,
                serde_json::from_value(json!({
                    "include_income": true,
                    "default_horizon_months": 6,
                }))
                .unwrap(),
            )
            .await
            .unwrap();
        if let Some(target) = target {
            self.server
                .ledger
                .set_emergency_fund(&self.alice, Currency::parse("EUR").unwrap(), target)
                .await
                .unwrap();
        }
    }

    async fn patch_settings(&self, token: &SecretToken, body: &Value) -> (StatusCode, String) {
        self.send(Method::PATCH, SETTINGS, token, Some(body), None)
            .await
    }
}

#[tokio::test]
async fn below_all_at_transactions_emergency_funds_read_as_none() {
    let world = world().await;
    let visible = world.account("Checking").await;
    for target in [
        None,
        Some(euro_fund()),
        Some(EmergencyFundTarget::MonthsOfOutflows { months: 4 }),
    ] {
        world.set_settings(target).await;
        for grants in [
            vec![on(visible, AccountLevel::Balances)],
            vec![on(visible, AccountLevel::Write)],
            vec![all(AccountLevel::Balances)],
            vec![
                all(AccountLevel::Balances),
                on(visible, AccountLevel::Write),
            ],
        ] {
            let (_, token) = world.token(Scope::Write, grants).await;

            let (status, body) = world.get(SETTINGS, &token).await;

            assert_eq!(status, StatusCode::OK);
            assert_eq!(body, settings_with(&Value::Null));
        }
    }
}

/// A token with `all` at `transactions` can have the forecast the emergency funds are part
/// of, so it reads them, as uniform access does.
#[tokio::test]
async fn with_the_forecasts_access_emergency_funds_read_as_they_are() {
    let world = world().await;
    let visible = world.account("Checking").await;
    world.set_settings(Some(euro_fund())).await;
    for grants in [
        Vec::new(),
        vec![all(AccountLevel::Transactions)],
        vec![
            all(AccountLevel::Transactions),
            on(visible, AccountLevel::Balances),
        ],
    ] {
        let (_, token) = world.token(Scope::Read, grants).await;

        let (status, body) = world.get(SETTINGS, &token).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, settings_with(&euro_funds()));
    }
}

/// Byte for byte what a household with no emergency fund answers: nothing says one is held
/// back.
#[tokio::test]
async fn a_withheld_emergency_fund_answers_like_one_never_set() {
    let with_fund = world().await;
    let (_, token) = with_fund
        .token(Scope::Read, vec![all(AccountLevel::Balances)])
        .await;
    with_fund.set_settings(Some(euro_fund())).await;
    let without_fund = world().await;
    let (_, other_token) = without_fund
        .token(Scope::Read, vec![all(AccountLevel::Balances)])
        .await;
    without_fund.set_settings(None).await;

    let withheld = with_fund
        .send(Method::GET, SETTINGS, &token, None, None)
        .await;
    let never_set = without_fund
        .send(Method::GET, SETTINGS, &other_token, None, None)
        .await;

    assert_eq!(withheld.0, StatusCode::OK);
    assert!(withheld == never_set, "the same answer, byte for byte");
}

/// An `admin` token restricted by grants may change settings. Whatever it sends, a patch's
/// answer says no more of the emergency funds than its read does.
#[tokio::test]
async fn a_restricted_patch_answers_without_the_emergency_funds() {
    let world = world().await;
    let visible = world.account("Checking").await;
    world.set_settings(Some(euro_fund())).await;
    let (_, token) = world
        .token(Scope::Admin, vec![on(visible, AccountLevel::Write)])
        .await;

    for body in [json!({}), json!({"stale_after_days": 9})] {
        let (status, text) = world.patch_settings(&token, &body).await;
        assert_eq!(status, StatusCode::OK);
        let answer: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(answer["emergency_funds"], Value::Null);
        assert_eq!(answer["include_income"], true);
    }
    let stored = world
        .server
        .store
        .emergency_funds()
        .await
        .unwrap()
        .get(&Currency::parse("EUR").unwrap())
        .copied();
    assert_eq!(stored, Some(euro_fund()), "the emergency fund stays");
}

#[tokio::test]
async fn with_the_forecasts_access_a_patch_answers_with_the_emergency_funds() {
    let world = world().await;
    world.set_settings(Some(euro_fund())).await;
    let (_, token) = world
        .token(Scope::Admin, vec![all(AccountLevel::Transactions)])
        .await;

    let (status, text) = world.patch_settings(&token, &json!({})).await;

    assert_eq!(status, StatusCode::OK);
    let answer: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(answer, settings_with(&euro_funds()));
}
