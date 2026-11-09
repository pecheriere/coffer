//! What a change to a planned item does to the months it is settled for (money redesign
//! §10.3): a paid bill stays paid when its values change from this month, and the months
//! after it, settled on the amount they then held, go with an amount change. A new schedule
//! ends them from this month.

use coffer_domain::audit::AuditValue;
use serde_json::json;

use super::{long_running, settling_world};
use crate::plan_items::recurring;

fn five_thousand() -> serde_json::Value {
    json!({"amount": {"minor_units": 5000, "currency": "USD"}})
}

/// This month's settlement stays, a paid bill stays paid; later months go, in one audit entry
/// with the change that ended them. Earlier months are what happened, and stay.
#[tokio::test]
async fn a_new_amount_keeps_this_months_settlement_and_removes_later_ones() {
    let world = settling_world().await;
    let id = world.create(&long_running(world.expense)).await;
    for month in ["2026-12", "2027-01", "2027-02"] {
        world.settled(&id, month).await;
    }
    let audited = world.audit().await.len();

    let patched = world.patch(&id, &five_thousand()).await;

    assert_eq!(patched["settlements"].as_array().unwrap().len(), 2);
    assert_eq!(world.months_of(&id).await, ["2026-12", "2027-01"]);
    let events = world.audit().await;
    assert_eq!(events.len(), audited + 1, "one entry");
    assert_eq!(events[0].action, "planned_item.update");
    let removed = events[0]
        .changes
        .iter()
        .find(|(name, _)| *name == "settlements")
        .unwrap()
        .1;
    assert_eq!(
        removed.before,
        AuditValue::Text(r#"[["2027-02","paid"]]"#.into())
    );
    assert_eq!(removed.after, AuditValue::Null);
}

#[tokio::test]
async fn a_new_currency_keeps_this_month_and_removes_later_ones() {
    let world = settling_world().await;
    let id = world.create(&long_running(world.expense)).await;
    for month in ["2027-01", "2027-02"] {
        world.settled(&id, month).await;
    }

    let patched = world
        .patch(
            &id,
            &json!({"amount": {"minor_units": 4200, "currency": "EUR"}}),
        )
        .await;

    assert_eq!(patched["settlements"].as_array().unwrap().len(), 1);
    assert_eq!(world.months_of(&id).await, ["2027-01"]);
}

/// Next month's amount changing leaves this month's settlement, which is on this month's
/// amount, and ends next month's.
#[tokio::test]
async fn a_next_month_amount_change_keeps_this_months_settlement_and_ends_next_months() {
    let world = settling_world().await;
    let id = world.create(&long_running(world.expense)).await;
    for month in ["2026-12", "2027-01", "2027-02"] {
        world.settled(&id, month).await;
    }

    let mut body = five_thousand();
    body["effective_from"] = json!("next_month");
    let patched = world.patch(&id, &body).await;

    assert_eq!(world.months_of(&id).await, ["2026-12", "2027-01"]);
    assert_eq!(patched["settlements"].as_array().unwrap().len(), 2);
}

/// A change that leaves the amount alone, a tier, leaves every settlement.
#[tokio::test]
async fn a_new_tier_leaves_every_settlement() {
    let world = settling_world().await;
    let id = world.create(&long_running(world.expense)).await;
    for month in ["2027-01", "2027-02"] {
        world.settled(&id, month).await;
    }

    world.patch(&id, &json!({"tier": "essentials"})).await;

    assert_eq!(world.months_of(&id).await, ["2027-01", "2027-02"]);
}

/// A new schedule ends them from this month, the paid one too: its occurrences moved.
#[tokio::test]
async fn a_new_schedule_ends_settlements_from_this_month() {
    let world = settling_world().await;
    let id = world.create(&recurring(world.expense)).await;
    world.settled(&id, "2027-01").await;

    world
        .patch(
            &id,
            &json!({"schedule": {"type": "recurring", "every": 2, "unit": "months",
                                 "anchor_on": "2027-01-01"}}),
        )
        .await;

    assert_eq!(world.months_of(&id).await, Vec::<String>::new());
}
