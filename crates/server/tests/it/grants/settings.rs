//! The household's forecast settings under grants (spec §8.9 R2): a safety floor is
//! a figure of the whole household's plan, so below `all` at `transactions` it reads
//! as none, in the same shape, on a read and in a patch's answer alike.

use super::{World, grant, on, world};
use axum::http::{Method, StatusCode};
use coffer_domain::identity::Scope;
use coffer_domain::policy::{AccountLevel, GrantTarget, TokenGrant};
use coffer_server::secrets::SecretToken;
use serde_json::{Value, json};

const SETTINGS: &str = "/api/v1/household/settings";
fn fixed_floor() -> Value {
    json!({"kind": "fixed", "amount": {"minor_units": 2_500_000, "currency": "EUR"}})
}

fn all(level: AccountLevel) -> TokenGrant {
    grant(GrantTarget::All, level)
}

/// The settings as stored here, with `floor` in their place.
fn settings_with(floor: &Value) -> Value {
    json!({
        "floor": floor,
        "include_income": true,
        "default_horizon_months": 6,
        "stale_after_days": 7,
        "display_currency": null,
        "timezone": "UTC",
        "timezone_setting": null,
    })
}

impl World {
    /// Sets the household's settings as alice's session: `floor`, and values other
    /// than the defaults, so that a read can't pass by showing the defaults.
    async fn set_settings(&self, floor: Value) {
        self.server
            .ledger
            .update_forecast_settings(
                &self.alice,
                serde_json::from_value(json!({
                    "floor": floor,
                    "include_income": true,
                    "default_horizon_months": 6,
                }))
                .unwrap(),
            )
            .await
            .unwrap();
    }

    async fn patch_settings(&self, token: &SecretToken, body: &Value) -> (StatusCode, String) {
        self.send(Method::PATCH, SETTINGS, token, Some(body), None)
            .await
    }

    /// The floor as alice's session reads it.
    async fn stored_floor(&self) -> Value {
        let settings = self
            .server
            .ledger
            .household_settings(&self.alice)
            .await
            .unwrap();
        serde_json::to_value(settings).unwrap()["floor"].clone()
    }
}

#[tokio::test]
async fn below_all_at_transactions_a_floor_reads_as_none() {
    let world = world().await;
    let visible = world.account("Checking").await;
    for floor in [
        fixed_floor(),
        json!({"kind": "months_of_outflows", "months": 4}),
    ] {
        world.set_settings(floor).await;
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

/// A token with `all` at `transactions` can have the forecast the floor is part of,
/// so it reads the floor, as uniform access does.
#[tokio::test]
async fn with_the_forecasts_access_a_floor_reads_as_it_is() {
    let world = world().await;
    let visible = world.account("Checking").await;
    world.set_settings(fixed_floor()).await;
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
        assert_eq!(body, settings_with(&fixed_floor()));
    }
}

/// Byte for byte what a household with no floor answers: nothing says one is held back.
#[tokio::test]
async fn a_withheld_floor_answers_like_a_floor_never_set() {
    let with_floor = world().await;
    let (_, token) = with_floor
        .token(Scope::Read, vec![all(AccountLevel::Balances)])
        .await;
    with_floor.set_settings(fixed_floor()).await;
    let without_floor = world().await;
    let (_, other_token) = without_floor
        .token(Scope::Read, vec![all(AccountLevel::Balances)])
        .await;
    without_floor
        .server
        .ledger
        .update_forecast_settings(
            &without_floor.alice,
            serde_json::from_value(json!({"include_income": true, "default_horizon_months": 6}))
                .unwrap(),
        )
        .await
        .unwrap();

    let withheld = with_floor
        .send(Method::GET, SETTINGS, &token, None, None)
        .await;
    let never_set = without_floor
        .send(Method::GET, SETTINGS, &other_token, None, None)
        .await;

    assert_eq!(withheld.0, StatusCode::OK);
    assert!(withheld == never_set, "the same answer, byte for byte");
}

/// An `admin` token restricted by grants may change settings. Whatever it sends, a
/// patch's answer says no more of the floor than its read does.
#[tokio::test]
async fn a_restricted_patch_answers_without_the_floor() {
    let world = world().await;
    let visible = world.account("Checking").await;
    world.set_settings(fixed_floor()).await;
    let (_, token) = world
        .token(Scope::Admin, vec![on(visible, AccountLevel::Write)])
        .await;

    for body in [json!({}), json!({"stale_after_days": 9})] {
        let (status, text) = world.patch_settings(&token, &body).await;
        assert_eq!(status, StatusCode::OK);
        let answer: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(answer["floor"], Value::Null);
        assert_eq!(answer["include_income"], true);
    }
    assert_eq!(world.stored_floor().await, fixed_floor(), "the floor stays");
}

/// It may replace the floor, and the answer doesn't confirm a guess: the floor it
/// names and any other answer alike, though only one writes.
#[tokio::test]
async fn a_restricted_patch_cannot_confirm_a_guess_at_the_floor() {
    let world = world().await;
    let visible = world.account("Checking").await;
    world.set_settings(fixed_floor()).await;
    let (_, token) = world
        .token(Scope::Admin, vec![on(visible, AccountLevel::Write)])
        .await;
    let guess = json!({"floor": fixed_floor()});

    let right = world.patch_settings(&token, &guess).await;
    let other = json!({"kind": "fixed", "amount": {"minor_units": 1, "currency": "USD"}});
    let wrong = world.patch_settings(&token, &json!({"floor": other})).await;

    assert_eq!(right.0, StatusCode::OK);
    assert!(right == wrong, "the same answer either way");
    let answer: Value = serde_json::from_str(&wrong.1).unwrap();
    assert_eq!(answer, settings_with(&Value::Null));
    assert_eq!(
        world.stored_floor().await,
        other,
        "the floor it sent is kept"
    );
}

#[tokio::test]
async fn with_the_forecasts_access_a_patch_answers_with_the_floor() {
    let world = world().await;
    world.set_settings(fixed_floor()).await;
    let (_, token) = world
        .token(Scope::Admin, vec![all(AccountLevel::Transactions)])
        .await;

    let (status, text) = world.patch_settings(&token, &json!({})).await;

    assert_eq!(status, StatusCode::OK);
    let answer: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(answer, settings_with(&fixed_floor()));
}
