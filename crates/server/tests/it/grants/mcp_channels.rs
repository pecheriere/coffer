//! A token with grants through the MCP endpoint: over a sample of the tools, it gets
//! exactly what the API shows it, a hidden account included.

use super::{World, grant, on, world};
use axum::http::{Method, StatusCode};
use coffer_domain::identity::Scope;
use coffer_domain::ids::{AccountId, PlannedItemId, TransactionId};
use coffer_domain::policy::{AccountLevel, GrantTarget};
use coffer_server::secrets::SecretToken;
use serde_json::{Value, json};

use crate::mcp::{Answer, as_answer, tool};

const HIDDEN_NAME: &str = "Quiet savings";
const HIDDEN_DESCRIPTION: &str = "Quiet purchase";

/// Compares each tool with the API request it stands for.
async fn compare(world: &World, token: &SecretToken, cases: Vec<(&str, Value, String)>) {
    for (name, arguments, uri) in cases {
        let (status, body) = world.get(&uri, token).await;
        let answer = tool(&world.server, token, name, arguments).await;
        assert_eq!(answer, as_answer(status, body), "{name}");
    }
}

struct Ids {
    visible: AccountId,
    hidden: AccountId,
    seen: TransactionId,
    quiet: TransactionId,
    plan: PlannedItemId,
    quiet_plan: PlannedItemId,
}

/// Tools and the API requests they stand for, with ids of what the token sees and doesn't.
fn sample(ids: &Ids) -> Vec<(&'static str, Value, String)> {
    let Ids {
        visible,
        hidden,
        seen,
        quiet,
        plan,
        quiet_plan,
    } = ids;
    vec![
        ("whoami", json!({}), "/api/v1/me".to_owned()),
        ("list_accounts", json!({}), "/api/v1/accounts".to_owned()),
        (
            "get_account",
            json!({"account_id": visible}),
            format!("/api/v1/accounts/{visible}"),
        ),
        (
            "get_account",
            json!({"account_id": hidden}),
            format!("/api/v1/accounts/{hidden}"),
        ),
        (
            "list_account_balances",
            json!({"account_id": hidden}),
            format!("/api/v1/accounts/{hidden}/balances"),
        ),
        (
            "list_transactions",
            json!({}),
            "/api/v1/transactions".to_owned(),
        ),
        (
            "list_transactions",
            json!({"account": hidden}),
            format!("/api/v1/transactions?account={hidden}"),
        ),
        (
            "get_transaction",
            json!({"transaction_id": seen}),
            format!("/api/v1/transactions/{seen}"),
        ),
        (
            "get_transaction",
            json!({"transaction_id": quiet}),
            format!("/api/v1/transactions/{quiet}"),
        ),
        (
            "list_plan_items",
            json!({}),
            "/api/v1/plan-items".to_owned(),
        ),
        (
            "get_plan_item",
            json!({"item_id": plan}),
            format!("/api/v1/plan-items/{plan}"),
        ),
        (
            "get_plan_item",
            json!({"item_id": quiet_plan}),
            format!("/api/v1/plan-items/{quiet_plan}"),
        ),
        ("get_forecast", json!({}), "/api/v1/forecast".to_owned()),
        (
            "get_plan_vs_actual",
            json!({"from": "2026-09"}),
            "/api/v1/reports/plan-vs-actual?from=2026-09".to_owned(),
        ),
        (
            "get_net_worth",
            json!({}),
            "/api/v1/reports/net-worth".to_owned(),
        ),
        (
            "get_fx_coverage",
            json!({}),
            "/api/v1/fx/coverage".to_owned(),
        ),
        (
            "get_household_settings",
            json!({}),
            "/api/v1/household/settings".to_owned(),
        ),
    ]
}

#[tokio::test]
async fn a_hidden_account_answers_in_a_tool_as_it_does_in_the_api() {
    let world = world().await;
    let visible = world.account("Checking").await;
    let hidden = world.account(HIDDEN_NAME).await;
    world.balance(visible).await;
    world.balance(hidden).await;
    let seen = world.transaction(visible, 1, "Groceries").await;
    let quiet = world.transaction(hidden, 2, HIDDEN_DESCRIPTION).await;
    let category = world.expense_category().await;
    let plan = world
        .planned_item("Rent", Some(visible), category, 1000)
        .await;
    let quiet_plan = world
        .planned_item("Quiet", Some(hidden), category, 1000)
        .await;
    let (_, token) = world
        .token(Scope::Read, vec![on(visible, AccountLevel::Transactions)])
        .await;

    let ids = Ids {
        visible,
        hidden,
        seen,
        quiet,
        plan,
        quiet_plan,
    };
    compare(&world, &token, sample(&ids)).await;

    for (name, arguments) in [
        ("list_accounts", json!({})),
        ("list_transactions", json!({})),
        ("get_net_worth", json!({})),
        ("list_plan_items", json!({})),
    ] {
        let answer = tool(&world.server, &token, name, arguments).await;
        let Answer::Value(value) = answer else {
            panic!("{name} must answer");
        };
        let text = value.to_string();
        assert!(!text.contains(HIDDEN_NAME), "{name}");
        assert!(!text.contains(HIDDEN_DESCRIPTION), "{name}");
        assert!(!text.contains(&hidden.to_string()), "{name}");
    }

    let hidden_read = tool(
        &world.server,
        &token,
        "get_account",
        json!({"account_id": hidden}),
    )
    .await;
    assert!(matches!(hidden_read, Answer::Problem(ref problem) if problem["code"] == "not_found"));
    let forecast = tool(&world.server, &token, "get_forecast", json!({})).await;
    assert!(matches!(forecast, Answer::Problem(ref problem) if problem["code"] == "forbidden"));
}

#[tokio::test]
async fn a_what_if_needs_what_a_forecast_needs_and_writes_nothing() {
    let world = world().await;
    let visible = world.account("Checking").await;
    let (_, narrow) = world
        .token(Scope::Read, vec![on(visible, AccountLevel::Transactions)])
        .await;
    let (_, wide) = world
        .token(
            Scope::Read,
            vec![grant(GrantTarget::All, AccountLevel::Transactions)],
        )
        .await;
    let body = json!({"horizon_months": 3});
    for token in [&narrow, &wide] {
        let (status, expected) = world
            .call(
                Method::POST,
                "/api/v1/forecast/what-if",
                token,
                Some(body.clone()),
            )
            .await;
        let answer = tool(&world.server, token, "what_if_forecast", body.clone()).await;
        assert_eq!(answer, as_answer(status, expected));
    }
}

/// Grants below `write`, on an account or across them all, and an account hidden from the
/// token: a review answers as the API's batch does, down to the index in the detail.
#[tokio::test]
async fn a_review_is_held_to_each_accounts_grant() {
    let world = world().await;
    let writable = world.account("Checking").await;
    let readable = world.account("Savings").await;
    let hidden = world.account(HIDDEN_NAME).await;
    let on_writable = world.transaction(writable, 1, "One").await;
    let on_readable = world.transaction(readable, 2, "Two").await;
    let on_hidden = world.transaction(hidden, 3, "Three").await;
    let (_, token) = world
        .token(
            Scope::Write,
            vec![
                on(writable, AccountLevel::Write),
                on(readable, AccountLevel::Transactions),
            ],
        )
        .await;

    for (target, expected) in [
        (on_writable, StatusCode::OK),
        (on_readable, StatusCode::FORBIDDEN),
        (on_hidden, StatusCode::NOT_FOUND),
    ] {
        let (status, body) = world
            .call(
                Method::POST,
                "/api/v1/transactions/batch",
                &token,
                Some(json!({"operations": [{"id": target, "patch": {"review": "unreviewed"}}]})),
            )
            .await;
        assert_eq!(status, expected);
        let answer = tool(
            &world.server,
            &token,
            "review_transactions",
            json!({"operations": [{"id": target, "review": "unreviewed"}]}),
        )
        .await;
        assert_eq!(answer, as_answer(status, body));
    }
}

/// A token with no account it can write is still shown the tool, since its scope may write:
/// what it can reach is for each call to say.
#[tokio::test]
async fn the_list_follows_the_scope_not_the_grants() {
    let world = world().await;
    let account = world.account("Checking").await;
    let (_, token) = world
        .token(Scope::Write, vec![on(account, AccountLevel::Balances)])
        .await;
    let listed = crate::mcp::rpc(&world.server, &token, "tools/list", json!({})).await;
    let tools = listed["result"]["tools"].as_array().unwrap();
    assert!(
        tools
            .iter()
            .any(|tool| tool["name"] == "review_transactions")
    );
}
