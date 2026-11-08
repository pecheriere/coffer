//! What the tools look like: their words, schemas and annotations, and how a refused argument
//! is answered.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use super::super::{Answer, tool};
use super::{Household, SENT, UNTRUSTED_WORDS, household, listed_tools, names};

/// The words a model relies on, since no type holds them.
#[tokio::test]
async fn every_tool_says_how_to_read_it() {
    let household = household().await;
    for tool in listed_tools(&household.server, &household.write).await {
        let name = tool["name"].as_str().unwrap();
        let description = tool["description"].as_str().unwrap();
        assert!(description.contains(UNTRUSTED_WORDS), "{name}");
        let has_amounts = tool["outputSchema"].to_string().contains("\"minor_units\"");
        assert_eq!(description.contains("minor units"), has_amounts, "{name}");
    }
    let tools = listed_tools(&household.server, &household.write).await;
    for name in ["get_forecast", "what_if_forecast", "get_net_worth"] {
        let tool = tools.iter().find(|tool| tool["name"] == name).unwrap();
        let description = tool["description"].as_str().unwrap();
        for word in ["`status`", "provisional", "cautions", "blockers", "`kind`"] {
            assert!(description.contains(word), "{name}: {word}");
        }
    }
}

fn references(value: &Value, found: &mut Vec<String>) {
    match value {
        Value::Object(object) => {
            for (key, nested) in object {
                match (key.as_str(), nested) {
                    ("$ref", Value::String(target)) => found.push(target.clone()),
                    _ => references(nested, found),
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|item| references(item, found)),
        _ => {}
    }
}

/// A client gets nothing but the tool, so each schema has to stand alone.
#[tokio::test]
async fn every_schema_stands_alone_and_is_an_object() {
    let household = household().await;
    for tool in listed_tools(&household.server, &household.write).await {
        let name = tool["name"].as_str().unwrap();
        for key in ["inputSchema", "outputSchema"] {
            let schema = &tool[key];
            assert_eq!(schema["type"], "object", "{name} {key}");
            let mut found = Vec::new();
            references(schema, &mut found);
            for target in found {
                let def = target
                    .strip_prefix("#/$defs/")
                    .unwrap_or_else(|| panic!("{name} {key}: {target}"));
                assert!(schema["$defs"].get(def).is_some(), "{name} {key}: {def}");
            }
        }
    }
}

#[tokio::test]
async fn a_tools_arguments_are_its_operations_parameters() {
    let household = household().await;
    let tools = listed_tools(&household.server, &household.write).await;
    let schema =
        |name: &str| tools.iter().find(|tool| tool["name"] == name).unwrap()["inputSchema"].clone();

    let balances = schema("list_account_balances");
    assert_eq!(balances["required"], json!(["account_id"]));
    assert_eq!(balances["additionalProperties"], false);
    for parameter in ["account_id", "from", "to", "after", "limit"] {
        assert!(
            balances["properties"].get(parameter).is_some(),
            "{parameter}"
        );
    }
    assert!(
        balances["properties"]["limit"]["description"]
            .as_str()
            .is_some()
    );

    assert!(
        schema("what_if_forecast")["properties"]
            .get("items")
            .is_some()
    );
    assert!(
        schema("list_transactions")["properties"]
            .get("review")
            .is_some()
    );
    assert!(
        schema("list_review_queue")["properties"]
            .get("review")
            .is_none()
    );
    assert_eq!(schema("whoami")["properties"], json!({}));
}

#[tokio::test]
async fn a_read_token_is_shown_no_tool_that_writes_and_each_tool_says_which_it_is() {
    let household = household().await;
    let write = listed_tools(&household.server, &household.write).await;
    let read = listed_tools(&household.server, &household.read).await;

    let writing: BTreeSet<String> = write
        .iter()
        .filter(|tool| tool["annotations"]["readOnlyHint"] != true)
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(writing, BTreeSet::from(["review_transactions".to_owned()]));
    assert_eq!(
        names(&read),
        names(&write).difference(&writing).cloned().collect()
    );
    for tool in &read {
        assert_eq!(tool["annotations"]["destructiveHint"], false);
    }
    let review = write
        .iter()
        .find(|tool| tool["name"] == "review_transactions")
        .unwrap();
    assert_eq!(review["annotations"]["destructiveHint"], true);
}

#[tokio::test]
async fn a_refused_argument_is_named_and_what_was_sent_is_not_echoed() {
    let household = household().await;
    let Household { server, read, .. } = &household;

    let bad_value = tool(server, read, "list_transactions", json!({"from": SENT})).await;
    let Answer::Problem(problem) = bad_value else {
        panic!("a bad date must be refused");
    };
    assert_eq!(problem["code"], "invalid");
    assert_eq!(problem["detail"], "the `from` parameter is not valid");

    let unknown = tool(server, read, "list_accounts", json!({SENT: true})).await;
    let Answer::Problem(problem) = unknown else {
        panic!("an unknown argument must be refused");
    };
    assert_eq!(problem["code"], "invalid");
    assert!(!problem.to_string().contains(SENT));

    let missing = tool(server, read, "get_account", json!({})).await;
    assert!(matches!(missing, Answer::Problem(ref problem) if problem["code"] == "invalid"));
}
