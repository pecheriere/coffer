//! Settling a month from the forecast page: a bill the page asks about can be settled from
//! it, a settled item says so in plain words and can only be undone, and the page offers no
//! write action with nothing to settle.

use axum::http::StatusCode;
use coffer_domain::identity::Role;
use coffer_server::testkit::browser::Browser;
use serde_json::json;

use super::{PAGE, page, section, signed_in, text_of, words};
use crate::forecast::answer::{Golden, golden};
use crate::forecast::outflow;
use crate::forecast::settled::id_of;

/// The worked example with its water bill settled: the page says so in plain words, and
/// what it shows is still what the API answers.
#[tokio::test]
async fn a_settled_item_is_shown_in_plain_words() {
    let Golden { world, .. } = golden().await;
    let water = id_of(&world.forecast().await, "Water");
    world.settle(&water, "2026-11", "paid").await;

    let body = page(&world).await.body;
    let answer = text_of(section(&body, "answer"));

    assert!(answer.contains("38,480.00 USD"));
    assert!(answer.contains("Firm:"));
    assert!(
        answer.contains("Water: 120.00 USD for November 2026 isn't counted, marked paid."),
        "says what was settled"
    );
}

/// All a settled item has to press is its undo.
#[tokio::test]
async fn a_settled_item_can_only_be_undone() {
    let Golden { world, .. } = golden().await;
    let water = id_of(&world.forecast().await, "Water");
    world.settle(&water, "2026-11", "skipped").await;

    let reply = page(&world).await;
    let main = &reply.body[reply.body.find("<main").unwrap()..];

    assert!(text_of(main).contains("marked skipped"));
    assert_eq!(main.matches("<form").count(), 1, "the undo");
    assert_eq!(main.matches("<button").count(), 1, "the undo");
    assert!(main.contains("Undo Water for November 2026"));
}

/// A settled item's name is the household's text, so it stays text.
#[tokio::test]
async fn a_settled_items_name_is_escaped() {
    const MARKUP: &str = "<script>alert(1)</script>";
    let (world, _) = crate::forecast::settled::holding().await;
    let home = world.expense("Home").await;
    let item = world
        .item(&outflow(
            MARKUP,
            1_000,
            home,
            &json!({"type": "one_off", "on": "2026-11-20"}),
        ))
        .await;
    world.settle(&item, "2026-11", "paid").await;

    let body = page(&world).await.body;

    assert!(!body.contains(MARKUP), "the markup stays text");
    assert!(
        section(&body, "answer").contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
        "and is shown"
    );
}

#[tokio::test]
async fn nothing_settled_says_nothing_of_it() {
    let Golden { world, .. } = golden().await;

    let answer = text_of(section(&page(&world).await.body, "answer"));

    assert!(
        !answer.contains("Settled"),
        "no heading with nothing under it"
    );
}

#[tokio::test]
async fn with_nothing_to_settle_the_page_offers_no_write_action() {
    let (world, _) = crate::forecast::settled::holding().await;

    let body = page(&world).await.body;

    assert_eq!(body.matches("<form").count(), 1, "only sign-out");
    assert!(body.contains("<form method=\"post\" action=\"/logout\""));
    assert_eq!(body.matches("<button").count(), 1, "only sign-out");
    assert_eq!(body.matches("<input").count(), 1, "only its token");
    let posted = signed_in(&world).await.post_form(PAGE, &[]).await;
    assert_eq!(posted.status, StatusCode::METHOD_NOT_ALLOWED);
}

/// The buttons beside a caution settle what it asks about, as the API would: the caution goes, and
/// the page says what was settled.
#[tokio::test]
async fn a_bill_the_page_asks_about_can_be_settled_from_it() {
    let Golden { world, .. } = golden().await;
    let water = id_of(&world.forecast().await, "Water");
    let mut browser = signed_in(&world).await;
    let reply = browser.get(PAGE).await;
    let action = format!(
        "/forecast/items/{}/settlements/2026-11",
        water.as_str().unwrap()
    );
    assert!(reply.body.contains(&action), "the form is there");
    let token = reply
        .body
        .split(&format!("action=\"{action}\""))
        .nth(1)
        .and_then(|form| form.split("value=\"").nth(1))
        .and_then(|rest| rest.split('"').next())
        .unwrap()
        .to_owned();

    let refused = browser
        .post_form(&action, &[("csrf", "wrong"), ("kind", "paid")])
        .await;
    assert_ne!(refused.status, StatusCode::SEE_OTHER, "a forged form fails");
    assert_eq!(world.forecast().await["status"], "provisional");

    let posted = browser
        .post_form(&action, &[("csrf", token.as_str()), ("kind", "paid")])
        .await;
    assert_eq!(posted.status, StatusCode::SEE_OTHER);

    assert_eq!(world.forecast().await["status"], "firm");
    assert!(words(&world).await.contains("isn't counted, marked paid"));
}

/// A settlement is the whole month, so the button says so, and undoing it puts all of it back. A
/// member who isn't an admin has them too.
#[tokio::test]
async fn settling_says_all_of_the_month_and_can_be_undone_by_a_member() {
    let Golden { world, .. } = golden().await;
    let invite = world
        .server
        .identity
        .create_invite(&world.alice, Role::Member, None)
        .await
        .unwrap();
    let mut bob = Browser::new(&world.server);
    let mut key = world.server.authenticator();
    bob.enroll(&mut key, &invite, "bob", "Bob").await;
    let water = id_of(&world.forecast().await, "Water");
    let settle = format!(
        "/forecast/items/{}/settlements/2026-11",
        water.as_str().unwrap()
    );
    let token = |body: &str, action: &str| {
        body.split(&format!("action=\"{action}\""))
            .nth(1)
            .and_then(|form| form.split("value=\"").nth(1))
            .and_then(|rest| rest.split('"').next())
            .unwrap()
            .to_owned()
    };

    let before = bob.get(PAGE).await;
    assert!(
        before
            .body
            .contains("Mark Water paid for all of November 2026")
    );
    assert!(before.body.contains("Skip Water for all of November 2026"));
    let csrf = token(&before.body, &settle);
    let posted = bob
        .post_form(&settle, &[("csrf", csrf.as_str()), ("kind", "paid")])
        .await;
    assert_eq!(posted.status, StatusCode::SEE_OTHER);
    assert_eq!(world.forecast().await["status"], "firm");

    let undo = format!("{settle}/undo");
    let settled = bob.get(PAGE).await;
    assert!(settled.body.contains("Undo Water for November 2026"));
    let csrf = token(&settled.body, &undo);
    let forged = bob.post_form(&undo, &[("csrf", "wrong")]).await;
    assert_ne!(forged.status, StatusCode::SEE_OTHER, "a forged form fails");
    let undone = bob.post_form(&undo, &[("csrf", csrf.as_str())]).await;
    assert_eq!(undone.status, StatusCode::SEE_OTHER);
    let forecast = world.forecast().await;
    assert_eq!(forecast["status"], "provisional");
    assert_eq!(forecast["settled"], json!([]));
}

/// The sentence says how, for a person and for an agent.
#[tokio::test]
async fn a_bill_unpaid_from_last_month_says_how_to_settle_it() {
    let (world, _) = crate::forecast::settled::holding().await;
    let home = world.expense("Home").await;
    let item = world
        .item(&outflow(
            "Boiler",
            1_000,
            home,
            &json!({"type": "one_off", "on": "2026-10-20"}),
        ))
        .await;

    let said = words(&world).await;

    assert!(said.contains("Boiler was due on 20 October 2026"));
    assert!(said.contains(&format!(
        "PUT /api/v1/plan-items/{}/settlements/2026-10",
        item.as_str().unwrap()
    )));
    assert!(said.contains("Mark Boiler paid for all of October 2026"));
    assert!(said.contains("Settling covers all of October 2026"));
}
