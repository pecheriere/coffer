//! The what-if: imagined items and settings change the answer, and nothing else.

use axum::http::{Method, StatusCode};
use coffer_domain::ids::{AccountId, CategoryId};
use coffer_store::{AuditFilter, PageRequest};
use serde_json::{Value, json};

use super::{
    AS_OF, FORECAST, ITEMS, WHAT_IF, World, goal_schedule, kinds, line, monthly, one_off_on,
    outflow, usd, with, world,
};

/// A category, an account of 5,000,000 and an emergency fund of 1,000,000.
async fn funded_world() -> (World, CategoryId, AccountId) {
    let world = world().await;
    let category = world.expense("Home").await;
    let account = world.funded("Checking", 5_000_000).await;
    world.emergency_fund(1_000_000).await;
    (world, category, account)
}

/// A one-off with no category, in the window.
fn roof(minor_units: i64) -> Value {
    json!({
        "name": "Roof",
        "amount": usd(minor_units),
        "direction": "outflow",
        "schedule": one_off_on("2026-12-20"),
    })
}

#[tokio::test]
async fn what_if_adds_a_roof_and_lowers_the_surplus_by_it() {
    let (world, _, _) = funded_world().await;

    let forecast = world.what_if(&json!({"items": [roof(1_500_000)]})).await;

    assert_eq!(
        forecast["surplus"],
        usd(2_500_000),
        "4,000,000 less the roof"
    );
    assert_eq!(line(&forecast, "Roof")["counted"], usd(1_500_000));
}

#[tokio::test]
async fn what_if_writes_nothing() {
    let (world, category, _) = funded_world().await;
    world
        .item(&outflow("Stored", 100, category, &monthly("2026-01-01")))
        .await;
    let audited = || async {
        world
            .server
            .store
            .list_audit(&AuditFilter::default(), &PageRequest::default())
            .await
            .map(|page| page.items.len())
            .ok()
    };
    let audit_before = audited().await;
    let plan = async || {
        world
            .send(
                Method::GET,
                &format!("{ITEMS}?include_inactive=true"),
                &world.admin,
                None,
            )
            .await
            .1
    };
    let plan_before = plan().await;

    let (status, _) = world
        .send_keyed(
            Method::POST,
            WHAT_IF,
            &world.admin,
            Some(&json!({"items": [roof(1)]})),
            Some("one-key"),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    assert!(audited().await == audit_before, "no audit entry");
    assert!(plan().await == plan_before, "no planned item");
    // Had the what-if taken the key, this other request under it would be refused.
    let (status, _) = world
        .send_keyed(
            Method::POST,
            ITEMS,
            &world.admin,
            Some(&outflow("Other", 100, category, &monthly("2026-01-01"))),
            Some("one-key"),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "no idempotency row");
}

#[tokio::test]
async fn what_if_overrides_emergency_funds_horizon_and_income() {
    let (world, _, _) = funded_world().await;
    let pay = world.category("Pay", "income").await;
    world
        .item(&json!({
            "name": "Salary",
            "category": pay,
            "amount": usd(100),
            "direction": "inflow",
            "schedule": monthly("2026-01-25"),
        }))
        .await;

    let forecast = world
        .what_if(&json!({
            "emergency_funds": [
                {"currency": "USD", "target": {"kind": "fixed", "minor_units": 2_000_000}},
            ],
            "horizon_months": 2,
            "include_income": true,
            "stale_after_days": 9,
            "as_of": "2026-11-16",
        }))
        .await;

    assert_eq!(
        forecast["emergency_funds"],
        json!([{"currency": "USD", "target": {"kind": "fixed", "minor_units": 2_000_000}}])
    );
    assert_eq!(forecast["totals"][0]["emergency_fund"], usd(2_000_000));
    assert_eq!(forecast["horizon_months"], 2);
    assert_eq!(forecast["include_income"], true);
    assert_eq!(forecast["stale_after_days"], 9);
    assert_eq!(forecast["as_of"], "2026-11-16");
    assert_eq!(forecast["surplus"], usd(3_000_200), "two pay days counted");
}

#[tokio::test]
async fn what_if_settings_out_of_range_are_422() {
    let (world, _, _) = funded_world().await;
    for (body, detail) in [
        (
            json!({"emergency_funds": [
                {"currency": "USD", "target": {"kind": "fixed", "minor_units": -1}},
            ]}),
            "a fixed emergency fund must be between 0 and 10^15 minor units",
        ),
        (
            json!({"emergency_funds": [
                {"currency": "USD", "target": {"kind": "months_of_outflows", "months": 61}},
            ]}),
            "emergency fund months must be 1 to 60",
        ),
        (
            json!({"emergency_funds": [
                {"currency": "USD", "target": {"kind": "fixed", "minor_units": 1}},
                {"currency": "USD", "target": {"kind": "fixed", "minor_units": 2}},
            ]}),
            "each currency's emergency fund once",
        ),
        (
            json!({"horizon_months": 61}),
            "the horizon must be 1 to 60 months",
        ),
        (
            json!({"stale_after_days": 0}),
            "stale after must be 1 to 365 days",
        ),
    ] {
        let (status, text) = world
            .send(Method::POST, WHAT_IF, &world.admin, Some(&body))
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let answer: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(answer["detail"], detail);
    }
}

#[tokio::test]
async fn empty_what_if_equals_forecast() {
    let (world, category, _) = funded_world().await;
    world
        .item(&outflow("Stored", 100, category, &monthly("2026-01-01")))
        .await;

    let forecast = world.send(Method::GET, FORECAST, &world.admin, None).await;
    let what_if = world
        .send(Method::POST, WHAT_IF, &world.admin, Some(&json!({})))
        .await;

    assert_eq!(forecast.0, StatusCode::OK);
    assert!(forecast == what_if, "the same answer, byte for byte");
}

#[tokio::test]
async fn too_many_items_is_422() {
    let (world, _, _) = funded_world().await;
    let items = |count: usize| json!({"items": vec![roof(1); count]});

    let (status, _) = world
        .send(Method::POST, WHAT_IF, &world.admin, Some(&items(50)))
        .await;
    assert_eq!(status, StatusCode::OK, "the limit itself is allowed");

    let (status, text) = world
        .send(Method::POST, WHAT_IF, &world.admin, Some(&items(51)))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let answer: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        answer["detail"],
        "a what-if takes at most 50 hypothetical items"
    );
}

/// It falls due inside the window, so all of its target counts.
#[tokio::test]
async fn hypothetical_goal_counts() {
    let (world, category, _) = funded_world().await;

    let forecast = world
        .what_if(&json!({"items": [outflow(
            "Sinking",
            600_000,
            category,
            &goal_schedule("2026-06-01", AS_OF),
        )]}))
        .await;

    assert_eq!(line(&forecast, "Sinking")["counted"], usd(600_000));
    assert_eq!(forecast["surplus"], usd(3_400_000));
}

#[tokio::test]
async fn lines_identify_hypothetical_items_by_index() {
    let (world, category, _) = funded_world().await;
    let stored = world
        .item(&outflow("Stored", 100, category, &monthly("2026-01-01")))
        .await;

    let forecast = world
        .what_if(&json!({"items": [
            with(roof(1), "name", json!("First")),
            with(roof(1), "name", json!("Second")),
        ]}))
        .await;

    assert_eq!(
        line(&forecast, "Stored")["source"],
        json!({"kind": "planned_item", "id": stored})
    );
    assert_eq!(
        line(&forecast, "First")["source"],
        json!({"kind": "hypothetical", "index": 0})
    );
    assert_eq!(
        line(&forecast, "Second")["source"],
        json!({"kind": "hypothetical", "index": 1})
    );
}

/// Spending in its category reduces and pays no hypothetical item.
#[tokio::test]
async fn hypothetical_budget_is_not_reduced() {
    let (world, category, account) = funded_world().await;
    world.spend(account, "2026-11-02", 500, category).await;

    let forecast = world
        .what_if(&json!({"items": [with(
            outflow("Imagined", 1_000, category, &monthly("2026-01-28")),
            "spending",
            json!("budget"),
        )]}))
        .await;

    assert_eq!(line(&forecast, "Imagined")["reduced_by"], usd(0));
}

#[tokio::test]
async fn invalid_hypothetical_is_422_without_echo() {
    let (world, category, _) = funded_world().await;
    let income = world.category("Pay", "income").await;
    let archived = world.expense("Archived").await;
    world
        .admin_send(
            Method::PATCH,
            &format!("/api/v1/categories/{archived}"),
            &json!({"archived": true}),
        )
        .await;
    let closed = world.funded("Closed", 1).await;
    world.close(closed).await;
    let unknown_category = CategoryId::new();
    let unknown_account = AccountId::new();
    let marker = "Marker\u{7}";

    let cases: Vec<(Value, &str)> = vec![
        (
            with(roof(1), "name", json!(marker)),
            "name must be 1 to 100 characters without control characters",
        ),
        (
            roof(0),
            "amount must be between 1 and 1000000000000000 minor units",
        ),
        (
            with(roof(1), "schedule", one_off_on("1960-01-01")),
            "dates must fall in the years 1970 to 2200",
        ),
        (
            with(
                with(roof(1), "direction", json!("inflow")),
                "schedule",
                goal_schedule("2026-01-01", "2027-01-01"),
            ),
            "a goal must be an outflow",
        ),
        (
            with(roof(1), "spending", json!("budget")),
            "only a recurring outflow has a spending kind",
        ),
        (
            with(roof(1), "category", json!(unknown_category)),
            "the category does not exist",
        ),
        (
            with(roof(1), "category", json!(archived)),
            "the category is archived",
        ),
        (
            with(roof(1), "category", json!(income)),
            "an outflow needs an expense category",
        ),
        (
            with(
                with(roof(1), "direction", json!("inflow")),
                "category",
                json!(category),
            ),
            "an inflow needs an income category",
        ),
        (
            with(roof(1), "account_hint", json!(unknown_account)),
            "the account hint names no open account",
        ),
        (
            with(roof(1), "account_hint", json!(closed)),
            "the account hint names no open account",
        ),
    ];
    for (item, detail) in cases {
        let (status, text) = world
            .send(
                Method::POST,
                WHAT_IF,
                &world.admin,
                Some(&json!({"items": [item]})),
            )
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let answer: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(answer["detail"], detail);
        for echoed in [
            "Marker".to_owned(),
            unknown_category.to_string(),
            unknown_account.to_string(),
            archived.to_string(),
            closed.to_string(),
        ] {
            assert!(!text.contains(&echoed), "the refusal echoes nothing");
        }
    }
}

/// A hint at an account outside what counts, which still counts, with a caution.
#[tokio::test]
async fn hypothetical_hint_at_an_excluded_account_cautions() {
    let (world, _, _) = funded_world().await;
    let side = world
        .account_with(
            "Side",
            "checking",
            "USD",
            json!({"include_in_forecast": false}),
        )
        .await;

    let forecast = world
        .what_if(&json!({"items": [with(roof(1), "account_hint", json!(side))]}))
        .await;

    assert_eq!(kinds(&forecast, "cautions"), ["item_hint_not_counted"]);
    assert_eq!(forecast["surplus"], usd(3_999_999));
}
