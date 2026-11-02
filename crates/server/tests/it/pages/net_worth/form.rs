//! The display currency form on the household page.

use axum::http::{StatusCode, header};
use coffer_server::testkit::browser::{Browser, Reply};
use serde_json::Value;

use super::{PAGE, THREE_MONTHS, alice, bob};
use crate::net_worth::{Golden, world};
use crate::pages::forecast::{section, text_of};

const HOUSEHOLD: &str = "/admin/household";
const SET_CURRENCY: &str = "/admin/household/display-currency";

/// Posts `fields` to the display currency form with `browser`'s token prepended.
async fn set_currency(browser: &mut Browser, fields: &[(&str, &str)]) -> Reply {
    let csrf = browser.get("/account").await.csrf();
    let mut all = vec![("csrf", csrf.as_str())];
    all.extend_from_slice(fields);
    browser.post_form(SET_CURRENCY, &all).await
}

fn has_entry(events: &[coffer_store::AuditEvent], action: &str) -> bool {
    events.iter().any(|event| event.action == action)
}

#[tokio::test]
async fn admin_sets_the_display_currency() {
    let world = world().await;
    let Golden { .. } = world.golden().await;
    let mut admin = alice(&world).await;
    let before = world.settings_events().await.len();

    let form = admin.get(HOUSEHOLD).await.body;
    assert!(form.contains(&format!("action=\"{SET_CURRENCY}\"")));
    assert!(
        form.contains("<option value=\"USD\" selected>"),
        "the current one"
    );
    assert!(form.contains("recomputes"), "says what changing it does");

    let reply = set_currency(&mut admin, &[("display_currency", "EUR")]).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    assert_eq!(reply.header(header::LOCATION), HOUSEHOLD);

    let events = world.settings_events().await;
    assert_eq!(events.len(), before + 1, "audited once");
    assert!(
        has_entry(&events, "household.settings"),
        "by the same service"
    );
    let report = world.report(THREE_MONTHS).await;
    assert_eq!(report["display_currency"], "EUR", "the report now converts");
    assert_eq!(report["points"][4]["total"]["minor_units"], 565_449);
    assert!(
        admin
            .get(HOUSEHOLD)
            .await
            .body
            .contains("<option value=\"EUR\" selected>")
    );
    assert!(text_of(section(&admin.get(PAGE).await.body, "answer")).contains("EUR"));
}

#[tokio::test]
async fn member_is_refused_whatever_is_sent() {
    let world = world().await;
    let mut member = bob(&world).await;
    let before = world.settings_events().await.len();

    for fields in [
        &[("display_currency", "USD")][..],
        &[("display_currency", "<b>nope</b>")][..],
        &[("display_currency", "USD"), ("extra", "1")][..],
        &[][..],
    ] {
        let reply = set_currency(&mut member, fields).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{fields:?}");
        assert!(!reply.body.contains("nope"), "does not echo");
    }

    assert_eq!(world.settings_events().await.len(), before);
    let report = world.report(THREE_MONTHS).await;
    assert_eq!(report["display_currency"], Value::Null, "nothing changed");
    assert_eq!(
        member.get(HOUSEHOLD).await.status,
        StatusCode::FORBIDDEN,
        "no form"
    );
}

#[tokio::test]
async fn forged_form_fails_whole() {
    let world = world().await;
    let mut admin = alice(&world).await;
    let before = world.settings_events().await.len();

    for fields in [
        &[("display_currency", "ZZZ")][..],
        &[("display_currency", "<b>nope</b>")][..],
        &[("display_currency", "")][..],
        &[("display_currency", "usd")][..],
        &[][..],
        &[("display_currency", "USD"), ("extra", "1")][..],
        &[("display_currency", "USD"), ("floor", "0")][..],
    ] {
        let reply = set_currency(&mut admin, fields).await;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY, "{fields:?}");
        assert!(reply.header(header::CONTENT_TYPE).starts_with("text/html"));
        assert!(!reply.body.contains("nope"), "does not echo");
    }

    assert_eq!(world.settings_events().await.len(), before, "not audited");
    let report = world.report(THREE_MONTHS).await;
    assert_eq!(report["display_currency"], Value::Null, "nothing changed");
}

#[tokio::test]
async fn csrf_is_required() {
    let world = world().await;
    let mut admin = alice(&world).await;
    let before = world.settings_events().await.len();

    for fields in [
        &[("display_currency", "USD")][..],
        &[("csrf", "wrong"), ("display_currency", "USD")][..],
    ] {
        let reply = admin.post_form(SET_CURRENCY, fields).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{fields:?}");
    }

    assert_eq!(world.settings_events().await.len(), before, "not audited");
    let report = world.report(THREE_MONTHS).await;
    assert_eq!(report["display_currency"], Value::Null, "nothing changed");
}

/// Nothing chosen yet: the browser won't send the empty choice, so it can't land on a
/// bare refusal.
#[tokio::test]
async fn the_empty_choice_cannot_be_sent() {
    let world = world().await;

    let form = alice(&world).await.get(HOUSEHOLD).await.body;

    assert!(form.contains("<option value=\"\" selected>Choose a currency"));
    assert!(form.contains("name=\"display_currency\" required"));
}
