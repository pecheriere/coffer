//! The household page's emergency fund and forecast settings cards: each form goes through
//! the ledger call the API makes, so what it saves reads back the same through the API.
//! The forecast settings card is in `forecast_settings`.

mod forecast_settings;

use axum::http::{StatusCode, header};
use coffer_domain::identity::Role;
use coffer_domain::money::Currency;
use coffer_domain::plan::EmergencyFundTarget;
use coffer_server::ledger::settings::HouseholdSettings;
use coffer_server::testkit::browser::{Browser, Reply};
use coffer_store::{AuditEvent, AuditFilter, PageRequest};
use secrecy::ExposeSecret as _;

use crate::forecast::{World, world};
use crate::pages::forecast::{section, text_of};

const HOUSEHOLD: &str = "/admin/household";
const SET_FUND: &str = "/admin/household/emergency-fund";

/// A browser signed in as `world`'s admin, alice.
async fn alice(world: &World) -> Browser {
    let session = world
        .server
        .sessions
        .create(world.alice.member)
        .await
        .unwrap();
    let mut browser = Browser::new(&world.server);
    browser.cookie = Some(session.expose_secret().to_owned());
    browser
}

/// A browser signed in as bob, a member.
async fn bob(world: &World) -> Browser {
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

/// Posts `fields` to `action` with `browser`'s token prepended.
async fn post(browser: &mut Browser, action: &str, fields: &[(&str, &str)]) -> Reply {
    let csrf = browser.get("/account").await.csrf();
    let mut all = vec![("csrf", csrf.as_str())];
    all.extend_from_slice(fields);
    browser.post_form(action, &all).await
}

/// The household's settings as alice reads them through the ledger the API calls.
async fn settings(world: &World) -> HouseholdSettings {
    world
        .server
        .ledger
        .household_settings(&world.alice)
        .await
        .unwrap()
}

/// `currency`'s emergency fund, if it has one.
async fn fund(world: &World, currency: &str) -> Option<EmergencyFundTarget> {
    let currency = Currency::parse(currency).unwrap();
    settings(world)
        .await
        .emergency_funds
        .unwrap()
        .into_iter()
        .find(|fund| fund.currency == currency)
        .map(|fund| fund.target)
}

/// Every audit entry about the household's settings.
async fn household_events(world: &World) -> Vec<AuditEvent> {
    world
        .server
        .store
        .list_audit(
            &AuditFilter {
                entity_type: Some("household".into()),
                ..AuditFilter::default()
            },
            &PageRequest::default(),
        )
        .await
        .unwrap_or_else(|_| panic!("reading the log failed"))
        .items
}

/// The emergency fund card's words, without tags.
async fn fund_card(browser: &mut Browser) -> String {
    let reply = browser.get(HOUSEHOLD).await;
    assert_eq!(reply.status, StatusCode::OK);
    text_of(section(&reply.body, "emergency-fund"))
}

fn fixed(currency: &str, amount: &str) -> [(&'static str, String); 4] {
    [
        ("currency", currency.to_owned()),
        ("kind", "fixed".to_owned()),
        ("amount", amount.to_owned()),
        ("months", String::new()),
    ]
}

/// `fields` as the borrowed pairs a form takes.
fn borrowed<'a>(fields: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
    fields
        .iter()
        .map(|(name, value)| (*name, value.as_str()))
        .collect()
}

#[tokio::test]
async fn the_card_offers_every_currency_with_the_display_one_chosen() {
    let world = world().await;
    world
        .settings(&serde_json::json!({"display_currency": "EUR"}))
        .await;
    let mut admin = alice(&world).await;

    let body = admin.get(HOUSEHOLD).await.body;
    let card = section(&body, "emergency-fund");

    assert!(card.contains("<h2>Emergency fund</h2>"));
    assert!(card.contains(&format!("action=\"{SET_FUND}\"")));
    assert!(card.contains("<option value=\"EUR\" selected>EUR</option>"));
    assert!(card.contains("<option value=\"JPY\">JPY</option>"));
    assert!(card.contains("inputmode=\"decimal\""));
    assert!(card.contains("min=\"1\" max=\"60\""));
}

#[tokio::test]
async fn admin_sets_a_fixed_target() {
    let world = world().await;
    let mut admin = alice(&world).await;

    let reply = post(&mut admin, SET_FUND, &borrowed(&fixed("USD", "10000"))).await;

    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    assert_eq!(
        reply.header(header::LOCATION),
        "/admin/household#emergency-fund"
    );
    assert_eq!(
        fund(&world, "USD").await,
        Some(EmergencyFundTarget::Fixed {
            minor_units: 1_000_000
        })
    );
    assert!(fund_card(&mut admin).await.contains("USD: 10,000.00"));
}

#[tokio::test]
async fn admin_sets_a_months_target() {
    let world = world().await;
    let mut admin = alice(&world).await;

    let reply = post(
        &mut admin,
        SET_FUND,
        &[
            ("currency", "USD"),
            ("kind", "months"),
            ("amount", ""),
            ("months", "3"),
        ],
    )
    .await;

    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    assert_eq!(
        fund(&world, "USD").await,
        Some(EmergencyFundTarget::MonthsOfOutflows { months: 3 })
    );
    assert!(
        fund_card(&mut admin)
            .await
            .contains("USD: 3 months of planned spending")
    );
}

#[tokio::test]
async fn setting_one_again_replaces_it() {
    let world = world().await;
    let mut admin = alice(&world).await;
    post(&mut admin, SET_FUND, &borrowed(&fixed("USD", "10000"))).await;

    post(&mut admin, SET_FUND, &borrowed(&fixed("USD", "250.50"))).await;

    assert_eq!(
        fund(&world, "USD").await,
        Some(EmergencyFundTarget::Fixed {
            minor_units: 25_050
        })
    );
    let card = fund_card(&mut admin).await;
    assert!(card.contains("USD: 250.50"));
    assert!(!card.contains("10,000.00"));
}

#[tokio::test]
async fn only_the_chosen_kind_counts() {
    let world = world().await;
    let mut admin = alice(&world).await;

    let months = post(
        &mut admin,
        SET_FUND,
        &[
            ("currency", "USD"),
            ("kind", "months"),
            ("amount", "500"),
            ("months", "3"),
        ],
    )
    .await;
    let amount = post(
        &mut admin,
        SET_FUND,
        &[
            ("currency", "EUR"),
            ("kind", "fixed"),
            ("amount", "500"),
            ("months", "not a number"),
        ],
    )
    .await;

    assert_eq!(months.status, StatusCode::SEE_OTHER);
    assert_eq!(amount.status, StatusCode::SEE_OTHER);
    assert_eq!(
        fund(&world, "USD").await,
        Some(EmergencyFundTarget::MonthsOfOutflows { months: 3 })
    );
    assert_eq!(
        fund(&world, "EUR").await,
        Some(EmergencyFundTarget::Fixed {
            minor_units: 50_000
        })
    );
}

#[tokio::test]
async fn amounts_follow_the_currency() {
    let world = world().await;
    let mut admin = alice(&world).await;

    for (currency, typed) in [
        ("USD", "100.005"),
        ("USD", "-5"),
        ("USD", "1e3"),
        ("USD", ""),
        ("JPY", "1000.5"),
    ] {
        let reply = post(&mut admin, SET_FUND, &borrowed(&fixed(currency, typed))).await;
        assert_eq!(
            reply.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{currency} {typed}"
        );
        assert!(
            reply
                .body
                .contains("no more decimals than its currency has"),
            "{currency} {typed}"
        );
        if typed.contains('.') {
            assert!(!reply.body.contains(typed), "never echoed: {typed}");
        }
    }
    assert_eq!(fund(&world, "USD").await, None);
    assert_eq!(fund(&world, "JPY").await, None);

    let reply = post(&mut admin, SET_FUND, &borrowed(&fixed("JPY", "1000"))).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    assert_eq!(
        fund(&world, "JPY").await,
        Some(EmergencyFundTarget::Fixed { minor_units: 1000 })
    );
}

#[tokio::test]
async fn months_out_of_range_and_unknown_choices_are_refused() {
    let world = world().await;
    let mut admin = alice(&world).await;

    for fields in [
        [
            ("currency", "USD"),
            ("kind", "months"),
            ("amount", ""),
            ("months", "0"),
        ],
        [
            ("currency", "USD"),
            ("kind", "months"),
            ("amount", ""),
            ("months", "61"),
        ],
        [
            ("currency", "USD"),
            ("kind", "months"),
            ("amount", ""),
            ("months", ""),
        ],
        [
            ("currency", "USD"),
            ("kind", "percent"),
            ("amount", "5"),
            ("months", "3"),
        ],
        [
            ("currency", "XXX"),
            ("kind", "fixed"),
            ("amount", "5"),
            ("months", ""),
        ],
    ] {
        let reply = post(&mut admin, SET_FUND, &fields).await;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY, "{fields:?}");
    }
    assert_eq!(fund(&world, "USD").await, None);
    assert!(household_events(&world).await.is_empty(), "nothing audited");
}

#[tokio::test]
async fn removing_twice_says_it_is_gone() {
    let world = world().await;
    let mut admin = alice(&world).await;
    post(&mut admin, SET_FUND, &borrowed(&fixed("USD", "10000"))).await;
    let remove = "/admin/household/emergency-fund/USD/remove";
    assert!(
        admin
            .get(HOUSEHOLD)
            .await
            .body
            .contains(&format!("action=\"{remove}\""))
    );

    let first = post(&mut admin, remove, &[]).await;
    let again = post(&mut admin, remove, &[]).await;

    assert_eq!(first.status, StatusCode::SEE_OTHER);
    assert_eq!(
        first.header(header::LOCATION),
        "/admin/household#emergency-fund"
    );
    assert_eq!(fund(&world, "USD").await, None);
    assert_eq!(again.status, StatusCode::NOT_FOUND);
    assert!(
        again
            .body
            .contains("That emergency fund was already removed.")
    );
    assert!(!fund_card(&mut admin).await.contains("USD:"));
}

#[tokio::test]
async fn removing_an_unknown_currency_is_refused() {
    let world = world().await;
    let mut admin = alice(&world).await;

    let reply = post(
        &mut admin,
        "/admin/household/emergency-fund/XXX/remove",
        &[],
    )
    .await;

    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!reply.body.contains("XXX"), "never echoed");
}

#[tokio::test]
async fn a_member_cannot_post() {
    let world = world().await;
    let mut admin = alice(&world).await;
    post(&mut admin, SET_FUND, &borrowed(&fixed("EUR", "10"))).await;
    let before = household_events(&world).await.len();
    let mut member = bob(&world).await;

    let set = post(&mut member, SET_FUND, &borrowed(&fixed("USD", "10000"))).await;
    let remove = post(
        &mut member,
        "/admin/household/emergency-fund/EUR/remove",
        &[],
    )
    .await;

    assert_eq!(set.status, StatusCode::FORBIDDEN);
    assert_eq!(remove.status, StatusCode::FORBIDDEN);
    assert_eq!(fund(&world, "USD").await, None);
    assert!(fund(&world, "EUR").await.is_some());
    assert_eq!(household_events(&world).await.len(), before);
}

#[tokio::test]
async fn csrf_is_required() {
    let world = world().await;
    let mut admin = alice(&world).await;
    post(&mut admin, SET_FUND, &borrowed(&fixed("EUR", "10"))).await;

    let set = fixed("USD", "10000");
    for csrf in [None, Some("wrong")] {
        let token: Vec<(&str, &str)> = csrf.map(|token| ("csrf", token)).into_iter().collect();
        let mut fields = token.clone();
        fields.extend(borrowed(&set));
        let reply = admin.post_form(SET_FUND, &fields).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{csrf:?}");
        let reply = admin
            .post_form("/admin/household/emergency-fund/EUR/remove", &token)
            .await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{csrf:?}");
    }
    assert_eq!(fund(&world, "USD").await, None);
    assert!(fund(&world, "EUR").await.is_some());
}
