//! What a rule may hold.

use axum::http::{Method, StatusCode};
use coffer_domain::ids::{AccountId, CategoryId};
use serde_json::{Value, json};

use super::{World, world};

async fn refused(world: &World, body: Value) -> (StatusCode, String) {
    let (status, answer) = world
        .call(Method::POST, "/api/v1/rules", &world.write, Some(body))
        .await;
    (status, answer["detail"].as_str().unwrap_or("").to_owned())
}

#[tokio::test]
async fn a_rule_must_say_what_it_looks_for_and_what_it_sets() {
    let world = world().await;
    let category = world.category("Groceries").await;
    let cases = [
        (
            json!({"match_kind": "contains", "pattern": "SHOP"}),
            "a rule sets a category, an assignee or both",
        ),
        (
            json!({"match_kind": "contains", "pattern": "  ", "category": category}),
            "pattern must be 1 to 200 characters without control characters, and not only spaces",
        ),
        (
            json!({"match_kind": "contains", "pattern": "A\u{7}B", "category": category}),
            "pattern must be 1 to 200 characters without control characters, and not only spaces",
        ),
        (
            json!({"match_kind": "contains", "pattern": "x".repeat(201), "category": category}),
            "pattern must be 1 to 200 characters without control characters, and not only spaces",
        ),
        (
            json!({"match_kind": "contains", "pattern": "S", "category": CategoryId::new()}),
            "the category does not exist",
        ),
        (
            json!({"match_kind": "contains", "pattern": "S", "assignee": coffer_domain::ids::MemberId::new()}),
            "the assignee must be an active member",
        ),
        (
            json!({"match_kind": "contains", "pattern": "S", "category": category, "position": 10001}),
            "position must be at most 10000",
        ),
        (
            json!({
                "match_kind": "contains", "pattern": "S", "category": category,
                "amount": {"currency": "USD"},
            }),
            "an amount range needs a lowest or a highest amount, lowest first",
        ),
        (
            json!({
                "match_kind": "contains", "pattern": "S", "category": category,
                "amount": {"currency": "USD", "min_minor": 5, "max_minor": 1},
            }),
            "an amount range needs a lowest or a highest amount, lowest first",
        ),
    ];
    for (body, detail) in cases {
        let (status, answer) = refused(&world, body).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{detail}");
        assert_eq!(answer, detail);
    }
    assert_eq!(world.events("rule.create").await.len(), 0);
}

#[tokio::test]
async fn a_rule_is_not_made_on_an_archived_category() {
    let world = world().await;
    let category = world.category("Groceries").await;
    world
        .call(
            Method::PATCH,
            &format!("/api/v1/categories/{category}"),
            &world.write,
            Some(json!({"archived": true})),
        )
        .await;

    let (status, detail) = refused(
        &world,
        json!({"match_kind": "contains", "pattern": "S", "category": category}),
    )
    .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(detail, "the category is archived");
}

#[tokio::test]
async fn an_amount_range_must_be_in_the_currency_of_the_rule_s_account() {
    let world = world().await;
    let category = world.category("Groceries").await;
    let account = world.account("Everyday").await;
    let (status, detail) = refused(
        &world,
        json!({
            "match_kind": "contains", "pattern": "S", "category": category,
            "account": account.id, "amount": {"currency": "JPY", "max_minor": -1},
        }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        detail,
        "an amount range must be in the currency of the rule's account"
    );
}

#[tokio::test]
async fn a_patch_keeps_a_rule_setting_something_and_changing_nothing_writes_nothing() {
    let world = world().await;
    let category = world.category("Groceries").await;
    let rule = world.filing("SHOP", category).await;
    let uri = format!("/api/v1/rules/{rule}");

    let (status, answer) = world
        .call(
            Method::PATCH,
            &uri,
            &world.write,
            Some(json!({"category": null})),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        answer["detail"],
        "a rule sets a category, an assignee or both"
    );

    let (status, _) = world
        .call(
            Method::PATCH,
            &uri,
            &world.write,
            Some(json!({"pattern": "SHOP", "enabled": true})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(world.events("rule.update").await.len(), 0);
}

#[tokio::test]
async fn the_rule_list_is_in_running_order_with_an_unknown_account_answered_as_missing() {
    let world = world().await;
    let category = world.category("Groceries").await;
    let later = world
        .rule(
            json!({"match_kind": "contains", "pattern": "A", "category": category, "position": 7}),
        )
        .await;
    let sooner = world
        .rule(
            json!({"match_kind": "contains", "pattern": "B", "category": category, "position": 2}),
        )
        .await;
    let appended = world.filing("C", category).await;

    let (_, list) = world
        .call(Method::GET, "/api/v1/rules", &world.write, None)
        .await;

    let order: Vec<_> = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|rule| rule["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(order, [sooner, later, appended]);
    assert_eq!(list["items"][2]["position"], 8, "by default after the last");

    let (status, _) = refused(
        &world,
        json!({
            "match_kind": "contains", "pattern": "S", "category": category,
            "account": AccountId::new(),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_pattern_is_kept_as_written_spaces_and_all() {
    let world = world().await;
    let category = world.category("Tax").await;
    let rule = world.filing("TAX ", category).await;
    let (_, view) = world
        .call(
            Method::GET,
            &format!("/api/v1/rules/{rule}"),
            &world.write,
            None,
        )
        .await;
    assert_eq!(view["pattern"], "TAX ");
    let account = world.account("Everyday").await;
    let feed = world.feed(&account).await;
    let taxi = world.arrive(&feed, "a", -100, "TAXI RIDE").await;
    let tax = world.arrive(&feed, "b", -100, "CITY TAX 12").await;
    assert_eq!(world.stored(taxi).await.splits.len(), 0);
    assert_eq!(world.stored(tax).await.splits.len(), 1);
}
