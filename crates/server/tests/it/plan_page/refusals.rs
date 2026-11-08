//! What the plan page refuses: fixed text that never echoes the input, and nothing written.

use axum::http::StatusCode;

use super::{PAGE, admin, audited, items, monthly_bill, post, with};
use crate::forecast::world;

#[tokio::test]
async fn refusals_are_fixed_text_that_never_echo_the_input() {
    let world = world().await;
    let mut admin = admin(&world).await;
    let rent = world.expense("Rent").await.to_string();
    let bill = monthly_bill("Rent", &rent);
    let marker = format!("<b>MARKER</b>{}", "x".repeat(100));

    let cases = [
        (
            with(bill.clone(), "type", "MARKER"),
            "not a kind of schedule the form offers",
        ),
        (
            with(bill.clone(), "amount", "12.345"),
            "no more decimals than its currency has",
        ),
        (
            with(bill.clone(), "amount", "MARKER"),
            "an amount is a number",
        ),
        (
            with(bill.clone(), "currency", "MARKER"),
            "not a currency coffer knows",
        ),
        (
            with(bill.clone(), "direction", "MARKER"),
            "not a direction the form offers",
        ),
        (
            with(bill.clone(), "category", "MARKER"),
            "not a category the form offers",
        ),
        (
            with(bill.clone(), "account_hint", "MARKER"),
            "not an account the form offers",
        ),
        (
            with(bill.clone(), "anchor_on", "MARKER"),
            "the first day is not a real day",
        ),
        (
            with(bill.clone(), "every", "MARKER"),
            "how often is a whole number",
        ),
        (
            with(bill.clone(), "active", "MARKER"),
            "not an answer the form offers",
        ),
        (
            with(bill.clone(), "name", &marker),
            "name must be 1 to 100 characters",
        ),
        (with(bill.clone(), "amount", "0"), "amount must be between"),
        (
            with(bill.clone(), "every", "1000"),
            "every must be 1 to 999",
        ),
        (
            with(bill.clone(), "direction", "inflow"),
            "an inflow needs an income category",
        ),
        (
            with(
                with(with(bill.clone(), "type", "goal"), "start_on", "2027-01-01"),
                "due_on",
                "2026-12-01",
            ),
            "a goal's due date must be after its start date",
        ),
    ];
    for (fields, reason) in &cases {
        let reply = post(&mut admin, PAGE, fields).await;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY, "{reason}");
        assert!(reply.body.contains(reason), "{reason}");
        assert!(!reply.body.contains("MARKER"), "does not echo: {reason}");
    }

    let reply = post(&mut admin, PAGE, &with(bill.clone(), "extra", "1")).await;
    assert_eq!(
        reply.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a forged field fails the form"
    );
    let mut forged = bill.clone();
    forged.push(("csrf", "forged"));
    let reply = admin.post_form(PAGE, &forged).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN, "a forged token");

    assert!(items(&world).await.is_empty(), "nothing written");
    assert_eq!(audited(&world).await.len(), 0, "nothing audited");
}

#[tokio::test]
async fn an_item_that_is_not_there_is_not_found() {
    let world = world().await;
    let mut admin = admin(&world).await;
    let rent = world.expense("Rent").await.to_string();
    let missing = "0190f5f0-0000-7000-8000-000000000000";

    for path in ["MARKER", missing] {
        assert_eq!(
            admin.get(&format!("{PAGE}/{path}")).await.status,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            admin.get(&format!("{PAGE}/{path}/delete")).await.status,
            StatusCode::NOT_FOUND
        );
        let reply = post(
            &mut admin,
            &format!("{PAGE}/{path}"),
            &monthly_bill("Rent", &rent),
        )
        .await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND);
        let reply = post(
            &mut admin,
            &format!("{PAGE}/{path}/active"),
            &[("active", "no")],
        )
        .await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND);
        let reply = post(&mut admin, &format!("{PAGE}/{path}/delete"), &[]).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND);
    }
}
