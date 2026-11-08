//! Tiers on planned items (money redesign §4.1): the default each kind takes, the rules a
//! sent tier must keep, and how a patch keeps, changes, drops or restores one.

use axum::http::{Method, StatusCode};
use coffer_domain::audit::{AuditValue, FieldChange};
use coffer_domain::transactions::CategoryKind;
use coffer_store::AuditEvent;
use serde_json::{Value, json};

use super::{goal, item_uri, one_off, recurring, with, world};

const UNPROCESSABLE: StatusCode = StatusCode::UNPROCESSABLE_ENTITY;

fn tier_change(event: &AuditEvent) -> Option<&FieldChange> {
    event
        .changes
        .iter()
        .find_map(|(name, change)| (name == "tier").then_some(change))
}

fn text(value: &str) -> AuditValue {
    AuditValue::Text(value.to_owned())
}

#[tokio::test]
async fn new_items_take_their_default() {
    let world = world().await;
    let income = world.category(CategoryKind::Income).await.1;
    let pay = with(
        with(recurring(income), "direction", json!("inflow")),
        "name",
        json!("Pay"),
    );
    for (body, expected, what) in [
        (recurring(world.expense), json!("must_pay"), "a bill"),
        (
            with(recurring(world.expense), "spending", json!("budget")),
            json!("flexible"),
            "a budget",
        ),
        (one_off(world.expense), json!("must_pay"), "a one-off"),
        (goal(world.expense), json!("saving_toward"), "a goal"),
        (pay, Value::Null, "income"),
    ] {
        let id = world.create(&body).await;
        let (status, read) = world
            .call(Method::GET, &item_uri(&id), &world.admin, None)
            .await;
        assert_eq!(status, StatusCode::OK);
        assert!(read.get("tier").is_some(), "the tier is always answered");
        assert_eq!(read["tier"], expected, "{what} takes its default");
    }

    let created = world.audit().await;
    assert_eq!(
        tier_change(&created[0]).unwrap().after,
        AuditValue::Null,
        "the create entry records income's missing tier"
    );
    assert_eq!(
        tier_change(&created[1]).unwrap().after,
        text("saving_toward"),
        "the create entry records the tier"
    );
}

#[tokio::test]
async fn a_chosen_tier_is_kept() {
    let world = world().await;
    let body = with(
        with(recurring(world.expense), "spending", json!("budget")),
        "tier",
        json!("essentials"),
    );
    let id = world.create(&body).await;
    let (_, read) = world
        .call(Method::GET, &item_uri(&id), &world.admin, None)
        .await;
    assert_eq!(read["tier"], json!("essentials"));
}

#[tokio::test]
async fn refusals_are_422_with_their_message() {
    let world = world().await;
    let income = world.category(CategoryKind::Income).await.1;
    let pay = with(recurring(income), "direction", json!("inflow"));
    for (body, message) in [
        (
            with(pay.clone(), "tier", json!("flexible")),
            "income has no tier",
        ),
        (
            with(goal(world.expense), "tier", json!("must_pay")),
            "a goal is always saving toward",
        ),
        (
            with(recurring(world.expense), "tier", json!("saving_toward")),
            "only a goal is saving toward",
        ),
    ] {
        assert_eq!(world.refused(&body, UNPROCESSABLE).await, message);
    }
    let refused = world
        .refused(
            &with(recurring(world.expense), "tier", json!("emergency_fund")),
            StatusCode::BAD_REQUEST,
        )
        .await;
    assert!(
        !refused.contains("emergency_fund"),
        "an unknown tier is refused without echoing it"
    );

    let bill = world.create(&recurring(world.expense)).await;
    assert_eq!(
        world
            .patch_refused(&bill, &json!({"tier": "saving_toward"}), UNPROCESSABLE)
            .await,
        "only a goal is saving toward"
    );
    let pay = world.create(&pay).await;
    assert_eq!(
        world
            .patch_refused(&pay, &json!({"tier": "essentials"}), UNPROCESSABLE)
            .await,
        "income has no tier"
    );
    let saving = world.create(&goal(world.expense)).await;
    assert_eq!(
        world
            .patch_refused(&saving, &json!({"tier": "flexible"}), UNPROCESSABLE)
            .await,
        "a goal is always saving toward"
    );
}

#[tokio::test]
async fn a_patch_changes_only_what_it_sends() {
    let world = world().await;
    let id = world
        .create(&with(recurring(world.expense), "spending", json!("budget")))
        .await;

    let after = world.patch(&id, &json!({"name": "Groceries"})).await;
    assert_eq!(after["tier"], json!("flexible"), "an absent tier stays");
    assert!(
        tier_change(&world.audit().await[0]).is_none(),
        "an unchanged tier isn't audited"
    );

    let after = world.patch(&id, &json!({"tier": "essentials"})).await;
    assert_eq!(after["tier"], json!("essentials"), "a sent tier changes");
    let latest = &world.audit().await[0];
    assert_eq!(latest.action, "planned_item.update");
    let change = tier_change(latest).unwrap();
    assert_eq!(change.before, text("flexible"));
    assert_eq!(change.after, text("essentials"));
    assert_eq!(latest.changes.iter().count(), 1, "only the tier changed");

    let audited = world.audit().await.len();
    world.patch(&id, &json!({"tier": "essentials"})).await;
    assert_eq!(
        world.audit().await.len(),
        audited,
        "the same tier again writes nothing"
    );
}

#[tokio::test]
async fn becoming_income_drops_the_tier() {
    let world = world().await;
    let income = world.category(CategoryKind::Income).await.1;
    let id = world
        .create(&with(recurring(world.expense), "tier", json!("essentials")))
        .await;

    let after = world
        .patch(&id, &json!({"direction": "inflow", "category": income}))
        .await;
    assert_eq!(after["tier"], Value::Null, "income has no tier");
    let change = tier_change(&world.audit().await[0]).unwrap().clone();
    assert_eq!(change.before, text("essentials"));
    assert_eq!(change.after, AuditValue::Null);

    let id = world.create(&recurring(world.expense)).await;
    assert_eq!(
        world
            .patch_refused(
                &id,
                &json!({"direction": "inflow", "category": income, "tier": "flexible"}),
                UNPROCESSABLE,
            )
            .await,
        "income has no tier",
        "a tier sent with the change to income is refused"
    );
}

#[tokio::test]
async fn becoming_an_outflow_takes_the_default() {
    let world = world().await;
    let income = world.category(CategoryKind::Income).await.1;
    let pay = with(recurring(income), "direction", json!("inflow"));
    let id = world.create(&pay).await;

    let after = world
        .patch(
            &id,
            &json!({"direction": "outflow", "category": world.expense, "spending": "budget"}),
        )
        .await;
    assert_eq!(after["tier"], json!("flexible"), "a budget's default");
    let change = tier_change(&world.audit().await[0]).unwrap().clone();
    assert_eq!(change.before, AuditValue::Null);
    assert_eq!(change.after, text("flexible"));

    let id = world.create(&pay).await;
    let after = world
        .patch(
            &id,
            &json!({"direction": "outflow", "category": world.expense, "tier": "essentials"}),
        )
        .await;
    assert_eq!(after["tier"], json!("essentials"), "or the one sent");
}

#[tokio::test]
async fn bill_to_budget_keeps_the_tier() {
    let world = world().await;
    let id = world
        .create(&with(recurring(world.expense), "tier", json!("essentials")))
        .await;

    let after = world.patch(&id, &json!({"spending": "budget"})).await;
    assert_eq!(after["spending"], json!("budget"));
    assert_eq!(after["tier"], json!("essentials"), "the tier stays");

    let must_pay = world.create(&recurring(world.expense)).await;
    let after = world.patch(&must_pay, &json!({"spending": "budget"})).await;
    assert_eq!(
        after["tier"],
        json!("must_pay"),
        "even a default tier stays: a bill made a budget is still must pay"
    );
}
