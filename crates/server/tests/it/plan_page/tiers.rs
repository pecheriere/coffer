//! Tiers on the plan page: items grouped by tier, and the forms that set one, read back
//! through the API.

use axum::http::StatusCode;
use serde_json::Value;

use super::{PAGE, admin, audited, item, items, monthly_bill, post, with, with_all};
use crate::forecast::world;

/// Where `needle` first appears in `body`.
fn at(body: &str, needle: &str) -> usize {
    body.find(needle)
        .unwrap_or_else(|| panic!("{needle:?} is on the page"))
}

#[tokio::test]
async fn groups_follow_the_tier_order() {
    let world = world().await;
    let mut admin = admin(&world).await;
    let food = world.expense("Food").await.to_string();
    let salary = world.category("Salary", "income").await.to_string();
    let budget = |name| with(monthly_bill(name, &food), "spending", "budget");
    let added = [
        // The paused one is added first, so only the grouping puts it last.
        with_all(budget("Cleaner"), &[("tier", "essentials"), ("active", "")]),
        budget("Hobbies"),
        with_all(
            monthly_bill("Salary", &salary),
            &[("direction", "inflow"), ("amount", "5000")],
        ),
        monthly_bill("Rent", &food),
        with_all(
            monthly_bill("Holiday", &food),
            &[
                ("type", "goal"),
                ("start_on", "2026-11-01"),
                ("due_on", "2027-06-01"),
            ],
        ),
        with(budget("Groceries"), "tier", "essentials"),
    ];
    for fields in &added {
        let reply = post(&mut admin, PAGE, fields).await;
        assert_eq!(reply.status, StatusCode::SEE_OTHER, "{fields:?}");
    }

    let body = admin.get(PAGE).await.body;

    let headings = [
        "Must pay</h2>",
        "Essentials</h2>",
        "Flexible</h2>",
        "Saving toward</h2>",
        "Income</h2>",
    ];
    let places: Vec<usize> = headings.iter().map(|heading| at(&body, heading)).collect();
    assert!(places.is_sorted(), "groups in the tier order: {places:?}");
    for (heading, mark) in [
        ("Must pay</h2>", "tier-must-pay"),
        ("Essentials</h2>", "tier-essentials"),
        ("Flexible</h2>", "tier-flexible"),
        ("Saving toward</h2>", "tier-saving"),
    ] {
        let mark =
            format!("<span class=\"tier-mark {mark}\" aria-hidden=\"true\"></span>{heading}");
        assert!(body.contains(&mark), "{heading} carries its mark");
    }
    let rows = |name: &str| at(&body, &format!("<span class=\"item-title\">{name}</span>"));
    assert!(rows("Rent") > places[0] && rows("Rent") < places[1]);
    assert!(rows("Groceries") > places[1] && rows("Groceries") < rows("Cleaner"));
    assert!(
        rows("Cleaner") < places[2],
        "the paused one last in its group"
    );
    assert!(rows("Hobbies") > places[2] && rows("Hobbies") < places[3]);
    assert!(rows("Holiday") > places[3] && rows("Holiday") < places[4]);
    assert!(rows("Salary") > places[4]);

    // Another household, with a bill alone: groups with nothing in them aren't shown.
    let other = crate::forecast::world().await;
    let mut other_admin = super::admin(&other).await;
    let rent = other.expense("Rent").await.to_string();
    post(&mut other_admin, PAGE, &monthly_bill("Rent", &rent)).await;
    let body = other_admin.get(PAGE).await.body;
    assert!(body.contains("Must pay</h2>"));
    for heading in [
        "Essentials</h2>",
        "Flexible</h2>",
        "Saving toward</h2>",
        "Income</h2>",
    ] {
        assert!(!body.contains(heading), "no empty {heading}");
    }
}

#[tokio::test]
async fn adding_a_budget_with_a_tier() {
    let world = world().await;
    let mut admin = admin(&world).await;
    let food = world.expense("Food").await.to_string();

    let page = admin.get(PAGE).await.body;
    assert!(page.contains("Which bucket this money is in"));
    assert!(page.contains("Can cover other essentials."));
    assert!(
        !page.contains("name=\"tier\" value=\"essentials\" checked"),
        "the add form preselects nothing"
    );
    let fields = with_all(
        monthly_bill("Groceries", &food),
        &[("spending", "budget"), ("tier", "essentials")],
    );
    let reply = post(&mut admin, PAGE, &fields).await;

    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    assert_eq!(item(&world, "Groceries").await["tier"], "essentials");
}

#[tokio::test]
async fn adding_without_a_tier_takes_the_default() {
    let world = world().await;
    let mut admin = admin(&world).await;
    let food = world.expense("Food").await.to_string();

    let budget = with(monthly_bill("Groceries", &food), "spending", "budget");
    post(&mut admin, PAGE, &budget).await;
    let bill = with(monthly_bill("Rent", &food), "tier", "");
    post(&mut admin, PAGE, &bill).await;

    assert_eq!(item(&world, "Groceries").await["tier"], "flexible");
    assert_eq!(item(&world, "Rent").await["tier"], "must_pay");
}

#[tokio::test]
async fn income_sends_no_tier() {
    let world = world().await;
    let mut admin = admin(&world).await;
    let salary = world.category("Salary", "income").await.to_string();

    let fields = with_all(
        monthly_bill("Salary", &salary),
        &[("direction", "inflow"), ("tier", "flexible")],
    );
    let reply = post(&mut admin, PAGE, &fields).await;

    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    assert_eq!(item(&world, "Salary").await["tier"], Value::Null);
}

#[tokio::test]
async fn goal_sends_no_tier() {
    let world = world().await;
    let mut admin = admin(&world).await;
    let food = world.expense("Food").await.to_string();

    let fields = with_all(
        monthly_bill("Holiday", &food),
        &[
            ("type", "goal"),
            ("start_on", "2026-11-01"),
            ("due_on", "2027-06-01"),
            ("tier", "flexible"),
        ],
    );
    let reply = post(&mut admin, PAGE, &fields).await;

    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    assert_eq!(item(&world, "Holiday").await["tier"], "saving_toward");
    let id = item(&world, "Holiday").await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let form = admin.get(&format!("{PAGE}/{id}")).await.body;
    assert!(form.contains("Goals are always saving toward."));
    assert!(!form.contains("name=\"tier\""), "nothing to choose");
}

#[tokio::test]
async fn changing_the_tier() {
    let world = world().await;
    let mut admin = admin(&world).await;
    let food = world.expense("Food").await.to_string();
    let budget = with(monthly_bill("Groceries", &food), "spending", "budget");
    post(&mut admin, PAGE, &budget).await;
    let id = item(&world, "Groceries").await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let path = format!("{PAGE}/{id}");

    let form = admin.get(&path).await.body;
    assert!(
        form.contains("name=\"tier\" value=\"flexible\" checked"),
        "the change form preselects the item's tier"
    );
    assert!(
        !form.contains("value=\"saving_toward\""),
        "only the tiers it may have"
    );
    let reply = post(&mut admin, &path, &with(budget, "tier", "must_pay")).await;

    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    assert_eq!(item(&world, "Groceries").await["tier"], "must_pay");
    assert_eq!(
        audited(&world).await,
        ["planned_item.create", "planned_item.update"]
    );
}

#[tokio::test]
async fn an_income_item_offers_no_tier_to_change() {
    let world = world().await;
    let mut admin = admin(&world).await;
    let salary = world.category("Salary", "income").await.to_string();
    let income = with(monthly_bill("Salary", &salary), "direction", "inflow");
    post(&mut admin, PAGE, &income).await;
    let id = item(&world, "Salary").await["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let form = admin.get(&format!("{PAGE}/{id}")).await.body;

    assert!(!form.contains("name=\"tier\""));
    assert!(!form.contains("Which bucket this money is in"));
}

#[tokio::test]
async fn a_tier_the_item_may_not_have_is_refused_in_fixed_words() {
    let world = world().await;
    let mut admin = admin(&world).await;
    let food = world.expense("Food").await.to_string();
    let bill = monthly_bill("Rent", &food);

    for (tier, reason) in [
        ("MARKER", "not a tier the form offers"),
        ("saving_toward", "only a goal is saving toward"),
    ] {
        let reply = post(&mut admin, PAGE, &with(bill.clone(), "tier", tier)).await;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY, "{reason}");
        assert!(reply.body.contains(reason), "{reason}");
        assert!(!reply.body.contains("MARKER"), "does not echo: {reason}");
    }
    assert!(items(&world).await.is_empty(), "nothing written");
}
