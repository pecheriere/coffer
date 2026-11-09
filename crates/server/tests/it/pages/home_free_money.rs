//! Home's free money card through the real router: it shows what `GET /api/v1/free-money`
//! answers, the answer first and its cautions after it. Reading free money is refused below
//! the forecast's access, which no browser session falls short of: that case is the unit
//! test of `readable` in `http::pages::home`.

use axum::http::StatusCode;
use coffer_domain::identity::Role;
use coffer_server::testkit::browser::Browser;
use serde_json::json;

use super::forecast::{signed_in, text_of};
use crate::forecast::{World, outflow, world};

const HOUSEHOLD_LINK: &str = "href=\"/admin/household#emergency-fund\"";
const CHOOSE: &str = "Choose a display currency on the Household page to pick which one leads.";

/// Home as alice, which must answer 200; the text of its free money card.
async fn free_card(world: &World) -> (String, String) {
    let reply = signed_in(world).await.get("/").await;
    assert_eq!(reply.status, StatusCode::OK);
    let start = reply.body.find("id=\"free-money\"").unwrap();
    let card = &reply.body[start..];
    let end = card.find("</section>").unwrap();
    (reply.body.clone(), card[..end].to_owned())
}

async fn rent(world: &World, minor_units: i64) {
    let home = world.expense("Rent").await;
    let schedule =
        json!({"type": "recurring", "every": 1, "unit": "months", "anchor_on": "2026-11-20"});
    world
        .item(&outflow("Rent", minor_units, home, &schedule))
        .await;
}

async fn member(world: &World) -> Browser {
    let invite = world
        .server
        .identity
        .create_invite(&world.alice, Role::Member, None)
        .await
        .unwrap();
    let mut bob = Browser::new(&world.server);
    let mut key = world.server.authenticator();
    bob.enroll(&mut key, &invite, "bob", "Bob").await;
    bob
}

/// Where `needles` occur in `text`, which must hold them all, in that order.
fn in_order(text: &str, needles: &[&str]) {
    let mut from = 0;
    for needle in needles {
        let Some(at) = text[from..].find(needle) else {
            panic!("{needle:?} is missing or out of order in {text}");
        };
        from += at + needle.len();
    }
}

#[tokio::test]
async fn home_leads_with_free_money() {
    let world = world().await;
    world.funded("Checking", 1_000_000).await;
    rent(&world, 200_000).await;
    world.emergency_fund(300_000).await;

    let (body, card) = free_card(&world).await;
    let words = text_of(&card);

    assert!(words.contains("5,000.00 USD free"), "{words}");
    in_order(
        &words,
        &[
            "Cash available 10,000.00 USD",
            "Must pay this month 2,000.00 USD",
            "Essentials 0.00 USD",
            "Flexible 0.00 USD",
            "Emergency fund 3,000.00 USD held, of 3,000.00 USD",
            "Free money 5,000.00 USD",
        ],
    );
    assert!(
        body.find("id=\"free-money\"").unwrap() < body.find("id=\"to-check\"").unwrap(),
        "free money comes first"
    );
    assert_eq!(card.matches("<svg").count(), 1, "one bar");
    assert!(!card.contains("style="), "no inline style");
}

#[tokio::test]
async fn the_breakdown_links_to_the_forecast() {
    let world = world().await;
    world.funded("Checking", 1_000_000).await;
    let (_, card) = free_card(&world).await;
    assert!(card.contains("href=\"/forecast\">How this is worked out<"));
}

#[tokio::test]
async fn home_words_a_shortfall() {
    let world = world().await;
    world.funded("Checking", 100_000).await;
    rent(&world, 200_000).await;
    world.emergency_fund(0).await;

    let (_, card) = free_card(&world).await;
    let words = text_of(&card);

    assert!(
        words.contains("You're 1,000.00 USD short of what's spoken for"),
        "{words}"
    );
    assert!(words.contains("Free money 0.00 USD"), "{words}");
    assert!(!words.contains(" -"), "no negative number: {words}");
    assert!(card.contains("class=\"hero-number answer-short\""));
}

#[tokio::test]
async fn the_display_currency_leads() {
    let world = world().await;
    world.funded("Checking", 1_000_000).await;
    let euros = world
        .account_with("Euros", "checking", "EUR", json!({}))
        .await;
    world
        .balance(euros, crate::forecast::BALANCE_ON, 42_000)
        .await;
    world.emergency_fund(0).await;
    world
        .emergency_fund_of("EUR", &json!({"kind": "fixed", "minor_units": 0}))
        .await;

    let (_, card) = free_card(&world).await;
    let words = text_of(&card);
    assert!(
        words.find("420.00 EUR free").unwrap() < words.find("10,000.00 USD free").unwrap(),
        "without a display currency the first by code leads: {words}"
    );
    assert!(words.contains(CHOOSE));
    let member = member(&world).await.get("/").await;
    assert!(!text_of(&member.body).contains(CHOOSE), "only for an admin");

    world.settings(&json!({"display_currency": "USD"})).await;
    let (_, card) = free_card(&world).await;
    let words = text_of(&card);
    let lead = card.find("class=\"hero-number\">").unwrap();
    assert!(
        card[lead..].contains(">10,000.00 USD free<"),
        "the hero is the display currency's: {words}"
    );
    assert!(words.contains("EUR: 420.00 EUR free"), "{words}");
    assert!(!words.contains(CHOOSE), "a display currency is chosen");
}

#[tokio::test]
async fn a_blocked_currency_says_why() {
    let world = world().await;
    world.funded("Checking", 1_000_000).await;
    world.emergency_fund(0).await;
    // The account has no balance, which is what blocks its currency.
    world
        .account_with("Euros", "checking", "EUR", json!({}))
        .await;
    world
        .emergency_fund_of("EUR", &json!({"kind": "fixed", "minor_units": 0}))
        .await;
    world.settings(&json!({"display_currency": "USD"})).await;

    let (_, card) = free_card(&world).await;
    let words = text_of(&card);

    assert!(
        words.contains("EUR: No number yet: Euros has no balance yet"),
        "{words}"
    );
    assert!(!words.contains("0.00 EUR"), "never a zero for it: {words}");
}

#[tokio::test]
async fn a_blocked_lead_currency_says_why() {
    let world = world().await;
    world.emergency_fund(0).await;
    world
        .account_with("Checking", "checking", "USD", json!({}))
        .await;
    world.settings(&json!({"display_currency": "USD"})).await;

    let (_, card) = free_card(&world).await;
    let words = text_of(&card);

    assert!(
        words.contains("USD: No number yet: Checking has no balance yet"),
        "{words}"
    );
    assert!(!card.contains("hero-number"), "no big number: {words}");
    assert!(!words.contains("0.00 USD free"), "never a zero: {words}");
}

#[tokio::test]
async fn each_blocked_currency_lists_only_its_own_accounts() {
    let world = world().await;
    world.emergency_fund(0).await;
    world
        .account_with("Checking", "checking", "USD", json!({}))
        .await;
    world
        .account_with("Euros", "checking", "EUR", json!({}))
        .await;
    world
        .emergency_fund_of("EUR", &json!({"kind": "fixed", "minor_units": 0}))
        .await;
    world.settings(&json!({"display_currency": "USD"})).await;

    let (_, card) = free_card(&world).await;
    let words = text_of(&card);

    assert!(
        words.contains("USD: No number yet: Checking has no balance yet"),
        "{words}"
    );
    assert!(
        words.contains("EUR: No number yet: Euros has no balance yet"),
        "{words}"
    );
    let usd = words.find("USD: No number yet").unwrap();
    let eur = words.find("EUR: No number yet").unwrap();
    assert!(
        !words[usd..eur].contains("Euros"),
        "the USD line names no euro account: {words}"
    );
    assert!(
        !words[eur..].contains("Checking has no balance"),
        "the EUR line names no dollar account: {words}"
    );
}

#[tokio::test]
async fn a_stale_balance_caution_names_its_account() {
    let world = world().await;
    world.emergency_fund(0).await;
    let old = world
        .account_with("Old savings", "savings", "USD", json!({}))
        .await;
    world.balance(old, "2026-10-01", 500_000).await;

    let (_, card) = free_card(&world).await;
    let words = text_of(&card);

    assert!(
        words.contains("The balance of Old savings is"),
        "the caution names the account: {words}"
    );
    assert!(!words.contains("an account"), "{words}");
}

#[tokio::test]
async fn no_emergency_fund_is_worth_knowing() {
    let world = world().await;
    world.funded("Checking", 1_000_000).await;

    let (_, card) = free_card(&world).await;
    let words = text_of(&card);
    let caution = words.find("No emergency fund is set for USD").unwrap();
    assert!(words.find("Worth knowing").unwrap() < caution);
    assert!(
        words.find("Free money 10,000.00 USD").unwrap() < words.find("Worth knowing").unwrap(),
        "the answer comes before the caution: {words}"
    );
    assert!(card.contains(HOUSEHOLD_LINK));

    let member = member(&world).await.get("/").await;
    assert!(text_of(&member.body).contains("An admin can set one on the Household page."));
    assert!(!member.body.contains(HOUSEHOLD_LINK));
}
