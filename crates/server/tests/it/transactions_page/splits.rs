//! Splitting a transaction across categories, with amounts typed in its direction, and a part
//! counting towards a goal. The service checks they add up and says why when they don't.

use axum::http::StatusCode;
use coffer_domain::ids::{CategoryId, PlannedItemId};
use coffer_domain::money::{Currency, Money};
use coffer_domain::plan::{PlanDirection, PlanSchedule};
use coffer_server::ledger::plan_items::PlannedItemInput;
use jiff::civil::date;
use serde_json::json;

use super::{World, assert_redirect, assert_refused, detail, world};

async fn goal(world: &World, category: CategoryId) -> PlannedItemId {
    world
        .server
        .ledger
        .create_planned_item(
            &world.alice,
            PlannedItemInput {
                name: "Holiday".into(),
                category,
                amount: Money {
                    minor_units: 100_000,
                    currency: Currency::parse("USD").unwrap(),
                },
                direction: PlanDirection::Outflow,
                account_hint: None,
                notes: None,
                active: None,
                schedule: PlanSchedule::Goal {
                    start_on: date(2026, 1, 1),
                    due_on: date(2027, 6, 1),
                    completed: false,
                },
                spending: None,
            },
            None,
        )
        .await
        .unwrap()
        .id
}

#[tokio::test]
async fn parts_typed_in_the_transactions_direction_are_stored_with_its_sign() {
    let mut world = world().await;
    let groceries = world.category("Groceries").await;
    let travel = world.category("Travel").await;
    let holiday = goal(&world, travel).await;
    let id = world.manual(-12_500, "Market", date(2026, 10, 2)).await;
    let path = format!("{}/splits", detail(id));
    let page = world.page(&detail(id)).await;
    assert!(page.body.contains("The parts must add up to 125.00 USD."));
    assert!(page.body.contains("name=\"goal_0\""), "goals are offered");

    let reply = world
        .post(
            &path,
            &[
                ("category_0", &groceries.to_string()),
                ("amount_0", "100.00"),
                ("goal_0", ""),
                ("category_1", &travel.to_string()),
                ("amount_1", "25"),
                ("goal_1", &holiday.to_string()),
                ("category_2", ""),
                ("amount_2", ""),
                ("goal_2", ""),
            ],
        )
        .await;
    assert_redirect(&reply, &detail(id));
    let read = world.read(id).await;
    assert_eq!(
        read["splits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|split| (
                split["category"].clone(),
                split["amount"]["minor_units"].clone(),
                split["goal"].clone()
            ))
            .collect::<Vec<_>>(),
        [
            (json!(groceries.to_string()), json!(-10_000), json!(null)),
            (
                json!(travel.to_string()),
                json!(-2_500),
                json!(holiday.to_string())
            ),
        ]
    );
    let page = world.page(&detail(id)).await;
    assert!(page.body.contains("Travel group: Travel, towards Holiday"));
    assert!(
        page.body.contains("value=\"100.00\""),
        "the form shows the parts as typed"
    );

    let reply = world
        .post(
            &path,
            &[
                ("category_0", &groceries.to_string()),
                ("amount_0", "100.00"),
                ("category_1", &travel.to_string()),
                ("amount_1", "-5.00"),
            ],
        )
        .await;
    assert_refused(
        &reply,
        StatusCode::UNPROCESSABLE_ENTITY,
        "the splits must sum exactly to the transaction amount",
        "xyzzy",
    );
    assert_eq!(
        world.read(id).await["splits"][1]["amount"]["minor_units"],
        -2_500,
        "unchanged"
    );
}

#[tokio::test]
async fn a_row_that_cannot_be_read_is_refused_without_echo() {
    let mut world = world().await;
    let groceries = world.category("Groceries").await.to_string();
    let id = world.manual(-1_000, "Market", date(2026, 10, 2)).await;
    let path = format!("{}/splits", detail(id));

    let cases: [(Vec<(&str, &str)>, &str); 6] = [
        (
            vec![("category_0", &groceries), ("amount_0", "xyzzy")],
            "an amount is a number",
        ),
        (
            vec![("category_0", &groceries), ("amount_0", "1.005")],
            "an amount is a number",
        ),
        (
            vec![("category_0", &groceries), ("amount_0", "")],
            "each part needs a category and an amount",
        ),
        (
            vec![("category_0", "xyzzy"), ("amount_0", "10")],
            "not a category the form offers",
        ),
        (
            vec![("category_50", &groceries), ("amount_50", "10")],
            "the form could not be read",
        ),
        (vec![("xyzzy_0", "10")], "the form could not be read"),
    ];
    for (fields, words) in cases {
        let reply = world.post(&path, &fields).await;
        assert_refused(&reply, StatusCode::UNPROCESSABLE_ENTITY, words, "xyzzy");
    }
    assert_eq!(world.read(id).await["splits"], json!([]));

    let reply = world.post(&path, &[]).await;
    assert_redirect(&reply, &detail(id));
    assert_eq!(
        world.read(id).await["splits"],
        json!([]),
        "no rows uncategorizes"
    );
}

#[tokio::test]
async fn money_in_is_split_as_typed() {
    let mut world = world().await;
    let gifts = world.category("Gifts").await;
    let id = world.manual(5_000, "Grandma", date(2026, 10, 2)).await;
    let reply = world
        .post(
            &format!("{}/splits", detail(id)),
            &[("category_0", &gifts.to_string()), ("amount_0", "50")],
        )
        .await;
    assert_redirect(&reply, &detail(id));
    assert_eq!(
        world.read(id).await["splits"][0]["amount"]["minor_units"],
        5_000
    );
}
