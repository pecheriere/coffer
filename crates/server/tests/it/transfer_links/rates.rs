//! A link across currencies is checked against the day's exchange rate (spec §8.12 T8): the
//! answer says what it found, and never changes what the link moves.

use axum::http::{Method, StatusCode};
use serde_json::{Value, json};

use super::{LINKS, World, world};

impl World {
    /// The rate of `currency` on `on` as an admin enters one: units per euro.
    async fn rate(&self, currency: &str, on: &str, rate: &str) {
        self.admin_send(
            Method::PUT,
            &format!("/api/v1/fx/manual-rates/{currency}/{on}"),
            &json!({"rate": rate}),
        )
        .await;
    }

    /// `usd_minor` USD out of checking and `eur_minor` EUR into a euro account, both on
    /// 2026-10-05, linked as the admin token; returns the link as it was made.
    async fn dollars_to_euros(&self, usd_minor: i64, eur_minor: i64) -> Value {
        let euros = self.account("Euros", "checking", "EUR").await;
        let out = self.manual(self.checking, "2026-10-05", -usd_minor).await;
        let into = self.manual(euros, "2026-10-05", eur_minor).await;
        self.link(&out, &into).await
    }
}

/// Legs in one currency have nothing to compare at a rate.
#[tokio::test]
async fn legs_in_one_currency_have_no_rate_check() {
    let world = world().await;
    let (outflow, inflow) = world.pair().await;

    let link = world.link(&outflow, &inflow).await;

    assert_eq!(link["rate_check"], Value::Null);
}

/// -1,250.00 USD against +12.50 EUR is far outside any spread: the answer names both legs, what
/// the outflow comes to in euros at 1.1172 USD to the euro, and the rate's date.
#[tokio::test]
async fn a_slipped_decimal_is_a_mismatch_naming_both_legs_and_the_rate() {
    let world = world().await;
    world.rate("USD", "2026-10-05", "1.1172").await;

    let link = world.dollars_to_euros(125_000, 1_250).await;

    assert_eq!(
        link["rate_check"],
        json!({
            "status": "mismatch",
            "outflow": {"minor_units": -125_000, "currency": "USD"},
            "inflow": {"minor_units": 1_250, "currency": "EUR"},
            "expected_inflow": {"minor_units": 111_887, "currency": "EUR"},
            "rate_on": "2026-10-05",
            "manual_rate": true,
        })
    );
    assert_eq!(link["difference"], Value::Null, "the numbers are as before");
}

/// A bank's spread and a fee are inside the tolerance.
#[tokio::test]
async fn a_spread_inside_five_percent_agrees() {
    let world = world().await;
    world.rate("USD", "2026-10-05", "1.1172").await;

    // 1,117.20 USD is exactly 1,000.00 EUR; 1,030.00 is 3% more.
    let link = world.dollars_to_euros(111_720, 103_000).await;

    assert_eq!(link["rate_check"], json!({"status": "agrees"}));
}

/// With no rate on or before the day, nothing is said either way, and reading the link again
/// says the same.
#[tokio::test]
async fn without_a_rate_the_link_could_not_be_checked() {
    let world = world().await;
    world.rate("USD", "2026-10-06", "1.1172").await;

    let made = world.dollars_to_euros(125_000, 1_250).await;
    let (status, read) = world
        .send(
            Method::GET,
            &format!("{LINKS}/{}", made["id"].as_str().unwrap()),
            &world.admin,
            None,
        )
        .await;

    assert_eq!(made["rate_check"], json!({"status": "not_checked"}));
    assert_eq!(status, StatusCode::OK);
    assert_eq!(read["rate_check"], made["rate_check"]);
}

/// The rate of an earlier day carries forward, as everywhere else rates are used.
#[tokio::test]
async fn an_earlier_rate_is_carried_forward() {
    let world = world().await;
    world.rate("USD", "2026-10-02", "1.1172").await;

    let link = world.dollars_to_euros(125_000, 1_250).await;

    assert_eq!(link["rate_check"]["status"], "mismatch");
    assert_eq!(link["rate_check"]["rate_on"], "2026-10-02");
}
