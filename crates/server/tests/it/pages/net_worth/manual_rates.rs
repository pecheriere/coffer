//! Exchange rates entered by hand on the household page, and what the net worth page says
//! of a figure built from one.

use axum::http::{StatusCode, header};
use coffer_server::testkit::browser::{Browser, Reply};

use super::{alice, bob, page};
use crate::net_worth::{World, world};
use crate::pages::forecast::section;

const HOUSEHOLD: &str = "/admin/household";
const ENTER: &str = "/admin/household/manual-rates";
const REMOVE: &str = "/admin/household/manual-rates/remove";
const DAY_REPORT: &str = "from=2026-10-26&to=2026-11-02&interval=day";

/// Posts `fields` to `action` with `browser`'s token prepended.
async fn post(browser: &mut Browser, action: &str, fields: &[(&str, &str)]) -> Reply {
    let csrf = browser.get("/account").await.csrf();
    let mut all = vec![("csrf", csrf.as_str())];
    all.extend_from_slice(fields);
    browser.post_form(action, &all).await
}

const A_RATE: [(&str, &str); 3] = [
    ("currency", "XAF"),
    ("on", "2026-10-26"),
    ("rate", "655.957"),
];
const THAT_DAY: [(&str, &str); 2] = [("currency", "XAF"), ("on", "2026-10-26")];

async fn with_xaf(world: &World) {
    let held = world.holding("Abroad", "XAF").await;
    world.balance(held, "2026-10-26", 655_957).await;
    world.display("EUR").await;
}

#[tokio::test]
async fn an_admin_enters_one_sees_it_and_removes_it() {
    let world = world().await;
    with_xaf(&world).await;
    let mut admin = alice(&world).await;

    let form = admin.get(HOUSEHOLD).await.body;
    assert!(form.contains(&format!("action=\"{ENTER}\"")));
    assert!(form.contains("max=\"2026-11-02\""), "no future day");
    let card = section(&form, "manual-rates");
    assert!(card.contains("<option value=\"USD\""));
    assert!(
        !card.contains("<option value=\"EUR\""),
        "the euro takes no rate"
    );

    let reply = post(&mut admin, ENTER, &A_RATE).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    assert_eq!(reply.header(header::LOCATION), HOUSEHOLD);

    let listed = admin.get(HOUSEHOLD).await.body;
    assert!(listed.contains("655.957 per euro"));
    assert!(listed.contains(&format!("action=\"{REMOVE}\"")));
    let report = world.report(DAY_REPORT).await;
    assert_eq!(report["points"][4]["total"]["minor_units"], 100_000);

    let reply = post(&mut admin, REMOVE, &THAT_DAY).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    assert!(!admin.get(HOUSEHOLD).await.body.contains("655.957 per euro"));
    let report = world.report(DAY_REPORT).await;
    assert!(report["points"][4]["total"].is_null());
}

#[tokio::test]
async fn the_net_worth_page_says_a_rate_was_entered_by_hand() {
    let world = world().await;
    with_xaf(&world).await;
    let mut admin = alice(&world).await;
    post(&mut admin, ENTER, &A_RATE).await;

    let body = page(&world, "").await.body;

    assert!(
        body.contains("was entered by hand, not published"),
        "the caution"
    );
    assert!(body.contains("(a rate entered by hand)"), "beside a figure");
    assert!(body.contains(", entered by hand"), "beside an account");
}

#[tokio::test]
async fn a_member_is_refused_whatever_is_sent() {
    let world = world().await;
    let mut member = bob(&world).await;
    let before = world.audit_count().await;

    for action in [ENTER, REMOVE] {
        for fields in [&A_RATE[..], &[("rate", "<b>nope</b>")][..], &[][..]] {
            let reply = post(&mut member, action, fields).await;
            assert_eq!(reply.status, StatusCode::FORBIDDEN);
            assert!(!reply.body.contains("nope"), "does not echo");
        }
    }

    assert_eq!(world.audit_count().await, before);
}

#[tokio::test]
async fn a_wrong_form_fails_whole_with_fixed_text() {
    let world = world().await;
    world.display("USD").await;
    let mut admin = alice(&world).await;
    let before = world.audit_count().await;

    for fields in [
        &[("currency", "ZZZ"), ("on", "2026-10-26"), ("rate", "1")][..],
        &[("currency", "EUR"), ("on", "2026-10-26"), ("rate", "1")][..],
        &[("currency", "USD"), ("on", "2026-10-26"), ("rate", "1")][..],
        &[("currency", "XAF"), ("on", "2026-11-03"), ("rate", "1")][..],
        &[("currency", "XAF"), ("on", "<b>nope</b>"), ("rate", "1")][..],
        &[("currency", "XAF"), ("on", "2026-10-26"), ("rate", "0")][..],
        &[
            ("currency", "XAF"),
            ("on", "2026-10-26"),
            ("rate", "<b>nope</b>"),
        ][..],
        &[
            ("currency", "XAF"),
            ("on", "2026-10-26"),
            ("rate", "1"),
            ("extra", "1"),
        ][..],
        &[][..],
    ] {
        let reply = post(&mut admin, ENTER, fields).await;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY, "{fields:?}");
        assert!(reply.header(header::CONTENT_TYPE).starts_with("text/html"));
        assert!(!reply.body.contains("nope"), "does not echo");
    }

    assert_eq!(world.audit_count().await, before);
}

#[tokio::test]
async fn a_forged_form_without_the_token_is_refused() {
    let world = world().await;
    let mut admin = alice(&world).await;

    let reply = admin.post_form(ENTER, &A_RATE).await;

    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert_eq!(world.server.store.manual_coverage().await.unwrap(), []);
}
