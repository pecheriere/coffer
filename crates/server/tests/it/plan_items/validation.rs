//! What a planned item refuses, one test per refusal.

use axum::http::{Method, StatusCode};
use coffer_domain::ids::{AccountId, CategoryId};
use coffer_domain::money::{Currency, Money};
use coffer_domain::plan::{MAX_PLAN_ITEMS, PlanDirection, PlanSchedule, PlannedItemName};
use coffer_domain::time::Clock as _;
use coffer_domain::transactions::CategoryKind;
use coffer_server::ledger::categories::{CategoryGroupPatch, CategoryPatch};
use coffer_store::NewPlannedItem;
use jiff::civil::date;
use serde_json::{Value, json};

use super::{ITEMS, World, goal, one_off, recurring, with, world};

const UNPROCESSABLE: StatusCode = StatusCode::UNPROCESSABLE_ENTITY;

impl World {
    async fn archive_category(&self, category: CategoryId) {
        self.server
            .ledger
            .update_category(
                &self.alice,
                category,
                CategoryPatch {
                    archived: Some(true),
                    ..CategoryPatch::default()
                },
            )
            .await
            .unwrap();
    }
}

fn amount(minor_units: i64) -> Value {
    json!({"minor_units": minor_units, "currency": "USD"})
}

#[tokio::test]
async fn amount_zero_negative_or_too_large() {
    let world = world().await;
    let id = world.create(&recurring(world.expense)).await;
    for minor_units in [0, -1, 1_000_000_000_000_001] {
        let refused = amount(minor_units);
        let detail = world
            .refused(
                &with(recurring(world.expense), "amount", refused.clone()),
                UNPROCESSABLE,
            )
            .await;
        assert_eq!(
            detail,
            "amount must be between 1 and 1000000000000000 minor units"
        );
        let detail = world
            .patch_refused(&id, &json!({"amount": refused}), UNPROCESSABLE)
            .await;
        assert_eq!(
            detail,
            "amount must be between 1 and 1000000000000000 minor units"
        );
    }
    let largest = amount(1_000_000_000_000_000);
    world
        .create(&with(recurring(world.expense), "amount", largest))
        .await;
}

#[tokio::test]
async fn unknown_currency_is_400() {
    let world = world().await;
    let bad = json!({"minor_units": 1, "currency": "ZZZ"});
    world
        .refused(
            &with(recurring(world.expense), "amount", bad),
            StatusCode::BAD_REQUEST,
        )
        .await;
}

#[tokio::test]
async fn name_empty_or_too_long_or_with_control_characters() {
    let world = world().await;
    let id = world.create(&recurring(world.expense)).await;
    for name in ["", "   ", "line\nbreak", &"x".repeat(101)] {
        let detail = world
            .refused(
                &with(recurring(world.expense), "name", json!(name)),
                UNPROCESSABLE,
            )
            .await;
        assert_eq!(
            detail,
            "name must be 1 to 100 characters without control characters"
        );
        world
            .patch_refused(&id, &json!({"name": name}), UNPROCESSABLE)
            .await;
    }
}

#[tokio::test]
async fn notes_too_long_or_with_control_characters() {
    let world = world().await;
    let id = world.create(&recurring(world.expense)).await;
    for notes in ["bell\u{7}", &"x".repeat(2001)] {
        world
            .refused(
                &with(recurring(world.expense), "notes", json!(notes)),
                UNPROCESSABLE,
            )
            .await;
        world
            .patch_refused(&id, &json!({"notes": notes}), UNPROCESSABLE)
            .await;
    }
}

#[tokio::test]
async fn goal_must_be_outflow() {
    let world = world().await;
    let income = world.category(CategoryKind::Income).await.1;
    let body = with(goal(income), "direction", json!("inflow"));
    let detail = world.refused(&body, UNPROCESSABLE).await;
    assert_eq!(detail, "a goal must be an outflow");

    let id = world.create(&goal(world.expense)).await;
    let change = json!({"direction": "inflow", "category": income});
    let detail = world.patch_refused(&id, &change, UNPROCESSABLE).await;
    assert_eq!(detail, "a goal must be an outflow", "nor may it become one");
}

#[tokio::test]
async fn inflow_needs_income_category() {
    let world = world().await;
    let body = with(recurring(world.expense), "direction", json!("inflow"));
    let detail = world.refused(&body, UNPROCESSABLE).await;
    assert_eq!(detail, "an inflow needs an income category");

    // A direction changed alone is checked against the item's category.
    let id = world.create(&recurring(world.expense)).await;
    let detail = world
        .patch_refused(&id, &json!({"direction": "inflow"}), UNPROCESSABLE)
        .await;
    assert_eq!(detail, "an inflow needs an income category");

    // Both changed together fit.
    let income = world.category(CategoryKind::Income).await.1;
    let after = world
        .patch(&id, &json!({"direction": "inflow", "category": income}))
        .await;
    assert_eq!(after["direction"], json!("inflow"));
}

#[tokio::test]
async fn outflow_needs_expense_category() {
    let world = world().await;
    let income = world.category(CategoryKind::Income).await.1;
    let detail = world.refused(&recurring(income), UNPROCESSABLE).await;
    assert_eq!(detail, "an outflow needs an expense category");

    // A category changed alone is checked against the item's direction.
    let id = world.create(&recurring(world.expense)).await;
    let detail = world
        .patch_refused(&id, &json!({"category": income}), UNPROCESSABLE)
        .await;
    assert_eq!(detail, "an outflow needs an expense category");
}

#[tokio::test]
async fn transfer_category_refused() {
    let world = world().await;
    let transfer = world.category(CategoryKind::Transfer).await.1;
    let outflow = recurring(transfer);
    let detail = world.refused(&outflow, UNPROCESSABLE).await;
    assert_eq!(detail, "an outflow needs an expense category");
    let inflow = with(recurring(transfer), "direction", json!("inflow"));
    let detail = world.refused(&inflow, UNPROCESSABLE).await;
    assert_eq!(detail, "an inflow needs an income category");
}

#[tokio::test]
async fn archived_category_refused() {
    let world = world().await;
    let id = world.create(&recurring(world.expense)).await;
    let archived = world.category(CategoryKind::Expense).await.1;
    world.archive_category(archived).await;

    let detail = world.refused(&recurring(archived), UNPROCESSABLE).await;
    assert_eq!(detail, "the category is archived");
    let detail = world
        .patch_refused(&id, &json!({"category": archived}), UNPROCESSABLE)
        .await;
    assert_eq!(detail, "the category is archived");
}

#[tokio::test]
async fn category_in_archived_group_refused() {
    let world = world().await;
    let (group, category) = world.category(CategoryKind::Expense).await;
    world
        .server
        .ledger
        .update_category_group(
            &world.alice,
            group,
            CategoryGroupPatch {
                archived: Some(true),
                ..CategoryGroupPatch::default()
            },
        )
        .await
        .unwrap();

    let detail = world.refused(&recurring(category), UNPROCESSABLE).await;
    assert_eq!(detail, "the category is archived");
}

#[tokio::test]
async fn a_patch_checks_only_what_changed() {
    let world = world().await;
    let account = world.account("Checking").await;
    let body = with(recurring(world.expense), "account_hint", json!(account));
    let id = world.create(&body).await;
    world.archive_category(world.expense).await;
    world.close_account(account).await;

    // The item's category has since been archived and its account closed: it
    // can still be renamed, deactivated and sent the same values.
    let after = world
        .patch(
            &id,
            &json!({"name": "Sewer bill", "active": false,
                    "category": world.expense, "account_hint": account}),
        )
        .await;
    assert_eq!(after["name"], json!("Sewer bill"));

    // Anything checked against the category still is.
    let detail = world
        .patch_refused(&id, &json!({"direction": "inflow"}), UNPROCESSABLE)
        .await;
    assert_eq!(detail, "the category is archived");
}

#[tokio::test]
async fn unknown_category_is_422() {
    let world = world().await;
    let id = world.create(&recurring(world.expense)).await;
    let unknown = CategoryId::new();
    let detail = world.refused(&recurring(unknown), UNPROCESSABLE).await;
    assert_eq!(detail, "the category does not exist");
    let detail = world
        .patch_refused(&id, &json!({"category": unknown}), UNPROCESSABLE)
        .await;
    assert_eq!(detail, "the category does not exist");
}

#[tokio::test]
async fn hint_on_missing_or_closed_account_is_422() {
    let world = world().await;
    let id = world.create(&recurring(world.expense)).await;
    let closed = world.account("Closed").await;
    world.close_account(closed).await;
    for hint in [AccountId::new(), closed] {
        let body = with(recurring(world.expense), "account_hint", json!(hint));
        let detail = world.refused(&body, UNPROCESSABLE).await;
        assert_eq!(detail, "the account hint names no open account");
        let detail = world
            .patch_refused(&id, &json!({"account_hint": hint}), UNPROCESSABLE)
            .await;
        assert_eq!(detail, "the account hint names no open account");
    }
}

#[tokio::test]
async fn end_before_anchor() {
    let world = world().await;
    let schedule = json!({"type": "recurring", "every": 1, "unit": "months",
                          "anchor_on": "2027-02-01", "end_on": "2027-01-31"});
    let body = with(recurring(world.expense), "schedule", schedule.clone());
    let detail = world.refused(&body, UNPROCESSABLE).await;
    assert_eq!(detail, "the end date cannot be before the anchor date");

    let id = world.create(&recurring(world.expense)).await;
    let detail = world
        .patch_refused(&id, &json!({"schedule": schedule}), UNPROCESSABLE)
        .await;
    assert_eq!(detail, "the end date cannot be before the anchor date");
}

#[tokio::test]
async fn every_out_of_range() {
    let world = world().await;
    for every in [0, 1000] {
        let schedule = json!({"type": "recurring", "every": every, "unit": "months",
                              "anchor_on": "2027-02-01"});
        let body = with(recurring(world.expense), "schedule", schedule);
        let detail = world.refused(&body, UNPROCESSABLE).await;
        assert_eq!(detail, "every must be 1 to 999");
    }
}

#[tokio::test]
async fn goal_due_not_after_start() {
    let world = world().await;
    for due_on in ["2027-01-01", "2026-12-31"] {
        let schedule = json!({"type": "goal", "start_on": "2027-01-01", "due_on": due_on});
        let body = with(goal(world.expense), "schedule", schedule);
        let detail = world.refused(&body, UNPROCESSABLE).await;
        assert_eq!(detail, "a goal's due date must be after its start date");
    }
}

#[tokio::test]
async fn dates_out_of_range() {
    let world = world().await;
    for day in ["1969-12-31", "2201-01-01"] {
        let schedules = [
            (
                recurring(world.expense),
                json!({"type": "recurring", "every": 1, "unit": "months", "anchor_on": day}),
            ),
            (
                one_off(world.expense),
                json!({"type": "one_off", "on": day}),
            ),
        ];
        for (body, schedule) in schedules {
            let detail = world
                .refused(&with(body, "schedule", schedule), UNPROCESSABLE)
                .await;
            assert_eq!(detail, "dates must fall in the years 1970 to 2200");
        }
    }
}

#[tokio::test]
async fn dates_other_than_plain_days_are_400() {
    let world = world().await;
    let id = world.create(&one_off(world.expense)).await;
    for day in [
        "2027-03-15T23:00:00-05:00",
        "2027-03-15T00:00:00Z",
        "2027-02-30",
        "20270315",
    ] {
        let schedule = json!({"type": "one_off", "on": day});
        let body = with(one_off(world.expense), "schedule", schedule.clone());
        let detail = world.refused(&body, StatusCode::BAD_REQUEST).await;
        assert!(!detail.contains(day), "a refusal never echoes the input");
        world
            .patch_refused(&id, &json!({"schedule": schedule}), StatusCode::BAD_REQUEST)
            .await;
    }
}

#[tokio::test]
async fn type_change_refused() {
    let world = world().await;
    let id = world.create(&recurring(world.expense)).await;
    for schedule in [
        json!({"type": "one_off", "on": "2027-03-15"}),
        json!({"type": "goal", "start_on": "2027-01-01", "due_on": "2027-07-01"}),
    ] {
        let detail = world
            .patch_refused(&id, &json!({"schedule": schedule}), UNPROCESSABLE)
            .await;
        assert_eq!(detail, "a planned item's type never changes");
    }
    assert_eq!(world.audit().await.len(), 1, "only the create was audited");
}

#[tokio::test]
async fn unknown_field_is_400() {
    let world = world().await;
    let id = world.create(&recurring(world.expense)).await;
    let bad = StatusCode::BAD_REQUEST;

    world
        .refused(&with(recurring(world.expense), "surprise", json!(1)), bad)
        .await;
    world.patch_refused(&id, &json!({"surprise": 1}), bad).await;
    let schedule = json!({"type": "one_off", "on": "2027-03-15", "surprise": 1});
    world
        .refused(&with(one_off(world.expense), "schedule", schedule), bad)
        .await;
    let (status, _) = world
        .call(
            Method::GET,
            &format!("{ITEMS}?surprise=1"),
            &world.admin,
            None,
        )
        .await;
    assert_eq!(status, bad, "a query's unknown parameter too");
}

#[tokio::test]
async fn item_limit_is_409() {
    let world = world().await;
    let now = world.server.clock.now();
    let mut tx = world.server.store.begin().await.unwrap();
    for _ in 1..MAX_PLAN_ITEMS {
        tx.insert_planned_item(NewPlannedItem {
            name: PlannedItemName::parse("Water bill").unwrap(),
            category: world.expense,
            amount: Money {
                minor_units: 100,
                currency: Currency::parse("USD").unwrap(),
            },
            direction: PlanDirection::Outflow,
            account_hint: None,
            notes: None,
            active: false,
            schedule: PlanSchedule::OneOff {
                on: date(2027, 1, 1),
            },
            created_at: now,
        })
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();

    // Up to the limit is fine, inactive items counting towards it.
    let last = world.create(&recurring(world.expense)).await;

    let (status, answer) = world
        .call(
            Method::POST,
            ITEMS,
            &world.admin,
            Some(&recurring(world.expense)),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(answer["type"], json!("urn:coffer:problem:conflict"));
    assert_eq!(
        answer["detail"],
        json!("a household holds at most 500 planned items")
    );
    assert_eq!(world.audit().await.len(), 1, "the refusal audited nothing");

    // Deleting one makes room.
    let (status, _) = world
        .call(Method::DELETE, &super::item_uri(&last), &world.admin, None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    world.create(&recurring(world.expense)).await;
}
