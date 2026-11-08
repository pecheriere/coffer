//! Every form of the plan page, read back through the API.

use axum::http::{StatusCode, header};
use serde_json::{Value, json};

use super::{PAGE, admin, audited, item, items, member, monthly_bill, post, with_all};
use crate::forecast::world;
use crate::pages::forecast::text_of;

#[tokio::test]
async fn an_empty_plan_points_to_categories_and_the_forecast_points_here() {
    let world = world().await;
    let mut admin = admin(&world).await;

    let reply = admin.get(PAGE).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(reply.body.contains("Nothing planned yet"));
    assert!(
        reply.body.contains("Add one on the categories page"),
        "no category to use yet"
    );
    assert!(
        reply.body.contains("href=\"/forecast\""),
        "links to the forecast"
    );
    assert!(
        reply.body.contains("href=\"/plan\" aria-current=\"page\""),
        "in the navigation"
    );

    let forecast = admin.get("/forecast").await.body;
    assert!(forecast.contains("No planned bill, budget, income or goal falls in this window yet"));
    assert!(forecast.contains("<a href=\"/plan\">Add them on the plan page</a>"));
}

#[tokio::test]
async fn each_kind_of_item_and_schedule_is_entered_from_the_page() {
    let world = world().await;
    let mut admin = admin(&world).await;
    let rent = world.expense("Rent").await.to_string();
    let salary = world.category("Salary", "income").await.to_string();
    let checking = world.funded("Checking", 100_000).await.to_string();

    let page = admin.get(PAGE).await.body;
    assert!(page.contains("Add a planned item"));
    assert!(
        page.contains("Rent (spending)"),
        "the categories it may use"
    );

    let added = [
        with_all(
            monthly_bill("Rent", &rent),
            &[("account_hint", &checking), ("notes", "Landlord")],
        ),
        with_all(
            monthly_bill("Groceries", &rent),
            &[
                ("spending", "budget"),
                ("every", "2"),
                ("unit", "weeks"),
                ("end_on", "2027-06-30"),
            ],
        ),
        with_all(
            monthly_bill("Boiler", &rent),
            &[("type", "one_off"), ("on", "2026-12-03")],
        ),
        with_all(
            monthly_bill("Holiday", &rent),
            &[
                ("type", "goal"),
                ("start_on", "2026-11-01"),
                ("due_on", "2027-06-01"),
            ],
        ),
        with_all(
            monthly_bill("Salary", &salary),
            &[("direction", "inflow"), ("amount", "5000")],
        ),
    ];
    for fields in &added {
        let reply = post(&mut admin, PAGE, fields).await;
        assert_eq!(reply.status, StatusCode::SEE_OTHER, "{fields:?}");
        assert!(reply.header(header::LOCATION).starts_with("/plan#item-"));
    }

    let rent_item = item(&world, "Rent").await;
    assert_eq!(
        rent_item["amount"],
        json!({"minor_units": 123_456, "currency": "USD"})
    );
    assert_eq!(rent_item["direction"], "outflow");
    assert_eq!(rent_item["category"], rent.as_str());
    assert_eq!(rent_item["account_hint"], checking.as_str());
    assert_eq!(rent_item["notes"], "Landlord");
    assert_eq!(rent_item["active"], true);
    assert_eq!(rent_item["spending"], "bill");
    assert_eq!(
        rent_item["schedule"],
        json!({"type": "recurring", "every": 1, "unit": "months", "anchor_on": "2026-11-01", "end_on": null})
    );
    let groceries = item(&world, "Groceries").await;
    assert_eq!(groceries["spending"], "budget");
    assert_eq!(groceries["schedule"]["every"], 2);
    assert_eq!(groceries["schedule"]["unit"], "weeks");
    assert_eq!(groceries["schedule"]["end_on"], "2027-06-30");
    assert_eq!(groceries["notes"], Value::Null, "empty notes are none");
    let boiler = item(&world, "Boiler").await;
    assert_eq!(
        boiler["schedule"],
        json!({"type": "one_off", "on": "2026-12-03"})
    );
    assert_eq!(
        boiler["spending"],
        Value::Null,
        "the bill field is read only for a recurring outflow"
    );
    let holiday = item(&world, "Holiday").await;
    assert_eq!(
        holiday["schedule"],
        json!({"type": "goal", "start_on": "2026-11-01", "due_on": "2027-06-01", "completed": false})
    );
    let salary_item = item(&world, "Salary").await;
    assert_eq!(salary_item["direction"], "inflow");
    assert_eq!(salary_item["spending"], Value::Null);
    assert_eq!(salary_item["amount"]["minor_units"], 500_000);

    let shown = text_of(&admin.get(PAGE).await.body);
    for words in [
        "Rent Bill · 1,234.56 USD out · Rent · on Checking Every month from 1 November 2026 Landlord",
        "Groceries Budget · 1,234.56 USD out · Rent Every 2 weeks from 1 November 2026, until 30 June 2027",
        "Boiler One-off · 1,234.56 USD out · Rent On 3 December 2026",
        "Holiday Goal · 1,234.56 USD out · Rent Set aside from 1 November 2026, due 1 June 2027",
        "Salary Income · 5,000.00 USD in · Salary Every month from 1 November 2026",
    ] {
        assert!(shown.contains(words), "{words}");
    }
    assert_eq!(audited(&world).await, ["planned_item.create"; 5]);
}

#[tokio::test]
async fn an_item_is_changed_paused_resumed_and_deleted() {
    let world = world().await;
    let mut admin = admin(&world).await;
    let rent = world.expense("Rent").await.to_string();
    let checking = world.funded("Checking", 100_000).await.to_string();
    let hinted = [("account_hint", checking.as_str()), ("notes", "Landlord")];
    let fields = with_all(monthly_bill("Rent", &rent), &hinted);
    post(&mut admin, PAGE, &fields).await;
    let id = item(&world, "Rent").await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let path = format!("{PAGE}/{id}");

    let form = admin.get(&path).await;
    assert_eq!(form.status, StatusCode::OK);
    assert!(form.body.contains("value=\"Rent\""));
    assert!(form.body.contains("value=\"1234.56\""));
    assert!(form.body.contains(
        "<textarea class=\"input\" name=\"notes\" rows=\"2\" maxlength=\"2000\">\nLandlord</textarea>"
    ));
    assert!(
        form.body
            .contains(&format!("<option value=\"{checking}\" selected>"))
    );
    assert!(
        !form.body.contains("name=\"due_on\""),
        "only its own kind of schedule"
    );

    let changed = [
        ("name", "Housing"),
        ("direction", "outflow"),
        ("category", rent.as_str()),
        ("amount", "1300"),
        ("currency", "USD"),
        ("type", "recurring"),
        ("every", "1"),
        ("unit", "months"),
        ("anchor_on", "2026-11-01"),
        ("end_on", "2027-10-31"),
        ("spending", "budget"),
        ("account_hint", ""),
        ("notes", ""),
        ("active", "yes"),
    ];
    let reply = post(&mut admin, &path, &changed).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    assert_eq!(reply.header(header::LOCATION), format!("{PAGE}#item-{id}"));
    let housing = item(&world, "Housing").await;
    assert_eq!(housing["amount"]["minor_units"], 130_000);
    assert_eq!(housing["schedule"]["end_on"], "2027-10-31");
    assert_eq!(housing["spending"], "budget");
    assert_eq!(
        housing["account_hint"],
        Value::Null,
        "no account clears the hint"
    );
    assert_eq!(housing["notes"], Value::Null, "empty notes clear them");

    post(&mut admin, &format!("{path}/active"), &[("active", "no")]).await;
    assert_eq!(item(&world, "Housing").await["active"], false);
    let page = admin.get(PAGE).await.body;
    assert!(page.contains("Paused") && page.contains("Resume"));
    post(&mut admin, &format!("{path}/active"), &[("active", "yes")]).await;
    assert_eq!(item(&world, "Housing").await["active"], true);

    let confirm = admin.get(&format!("{path}/delete")).await;
    assert_eq!(confirm.status, StatusCode::OK);
    assert!(confirm.body.contains("for good"));
    assert!(
        !confirm.body.contains("linked to it"),
        "only a goal holds linked spending"
    );
    assert!(confirm.body.contains(&format!("action=\"{path}/delete\"")));
    assert_eq!(items(&world).await.len(), 1, "asking deletes nothing");
    let reply = post(&mut admin, &format!("{path}/delete"), &[]).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    assert_eq!(reply.header(header::LOCATION), PAGE);
    assert_eq!(items(&world).await.len(), 0, "deleted");

    assert_eq!(
        audited(&world).await,
        [
            "planned_item.create",
            "planned_item.delete",
            "planned_item.update",
            "planned_item.update",
            "planned_item.update",
        ]
    );
}

#[tokio::test]
async fn a_goal_shows_what_was_spent_and_deleting_it_warns_of_linked_spending() {
    let world = world().await;
    let mut admin = admin(&world).await;
    let rent = world.expense("Rent").await.to_string();
    let goal = with_all(
        monthly_bill("Holiday", &rent),
        &[
            ("type", "goal"),
            ("start_on", "2026-11-01"),
            ("due_on", "2027-06-01"),
        ],
    );
    post(&mut admin, PAGE, &goal).await;
    let id = item(&world, "Holiday").await["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let shown = text_of(&admin.get(PAGE).await.body);
    assert!(shown.contains("due 1 June 2027 Spent from it so far 0.00 USD"));
    assert!(
        !shown.contains("Target"),
        "the target is the amount, already shown"
    );

    let confirm = text_of(&admin.get(&format!("{PAGE}/{id}/delete")).await.body);

    assert!(confirm.contains(
        "A goal with spending linked to it can't be deleted: mark it completed instead, so it \
         needs no more money."
    ));
}

#[tokio::test]
async fn a_member_plans_as_the_api_lets_them() {
    let world = world().await;
    let rent = world.expense("Rent").await.to_string();
    let mut member = member(&world).await;

    assert!(
        member.get(PAGE).await.body.contains("Add a planned item"),
        "the API lets a member write"
    );
    let reply = post(&mut member, PAGE, &monthly_bill("Rent", &rent)).await;

    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    assert_eq!(items(&world).await.len(), 1);
    assert_eq!(audited(&world).await, ["planned_item.create"]);
}
