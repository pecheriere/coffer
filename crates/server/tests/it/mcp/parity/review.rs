//! Reviewing through `review_transactions`, and reading what a review leaves.

use axum::http::{Method, StatusCode};
use coffer_domain::audit::AuditActor;
use coffer_store::{AuditFilter, PageRequest};
use serde_json::json;

use super::super::{Answer, as_answer, tool};
use super::{BANK_TEXT, Household, api, household, id};

/// Being hidden from the list isn't what stops it: the handler refuses, as it does the API.
#[tokio::test]
async fn a_read_token_calling_a_tool_that_writes_is_refused_and_nothing_changes() {
    let household = household().await;
    let Household {
        server,
        read,
        transaction,
        ..
    } = &household;
    let (status, refused) = api(
        server,
        read,
        Method::POST,
        "/api/v1/transactions/batch",
        Some(&json!({"operations": [{"id": transaction, "patch": {"review": "reviewed"}}]})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let answer = tool(
        server,
        read,
        "review_transactions",
        json!({"operations": [{"id": transaction, "review": "reviewed"}]}),
    )
    .await;

    assert_eq!(answer, Answer::Problem(refused));
    let (_, seen) = api(
        server,
        read,
        Method::GET,
        &format!("/api/v1/transactions/{transaction}"),
        None,
    )
    .await;
    assert_eq!(seen["review"], "unreviewed");
}

#[tokio::test]
async fn a_review_categorizes_assigns_and_is_audited_to_its_token() {
    let household = household().await;
    let Household {
        server,
        write,
        write_id,
        member,
        category,
        transaction,
        ..
    } = &household;

    let answer = tool(
        server,
        write,
        "review_transactions",
        json!({"operations": [{
            "id": transaction,
            "review": "reviewed",
            "assignee": member,
            "splits": [{"category": category, "minor_units": -1234}],
        }]}),
    )
    .await;

    let (_, seen) = api(
        server,
        write,
        Method::GET,
        &format!("/api/v1/transactions/{transaction}"),
        None,
    )
    .await;
    assert_eq!(seen["review"], "reviewed");
    assert_eq!(seen["assignee"].as_str(), Some(member.as_str()));
    assert_eq!(
        seen["splits"][0]["category"].as_str(),
        Some(category.as_str())
    );
    assert_eq!(answer, Answer::Value(json!({"items": [seen]})));

    let events = server
        .store
        .list_audit(
            &AuditFilter {
                entity_type: Some("transaction".into()),
                ..AuditFilter::default()
            },
            &PageRequest::default(),
        )
        .await
        .unwrap()
        .items;
    let update = events
        .iter()
        .find(|event| event.action == "transaction.update")
        .unwrap();
    assert!(
        matches!(update.actor, AuditActor::Member { agent: Some(agent), .. } if agent == *write_id)
    );
}

#[tokio::test]
async fn a_review_is_all_or_nothing_and_says_which_failed() {
    let household = household().await;
    let Household {
        server,
        write,
        transaction,
        account,
        ..
    } = &household;
    let (_, other) = api(
        server,
        write,
        Method::POST,
        "/api/v1/transactions",
        Some(
            &json!({"account": account, "posted_on": "2026-10-06", "minor_units": -1,
                     "description": "Other"}),
        ),
    )
    .await;

    let answer = tool(
        server,
        write,
        "review_transactions",
        json!({"operations": [
            {"id": transaction, "review": "reviewed"},
            {"id": id(&other), "splits": [{"category": account, "minor_units": -1}]},
        ]}),
    )
    .await;

    let Answer::Problem(problem) = answer else {
        panic!("a bad operation must refuse the whole batch");
    };
    assert!(
        problem["detail"]
            .as_str()
            .unwrap()
            .starts_with("operations[1]: ")
    );
    let (_, seen) = api(
        server,
        write,
        Method::GET,
        &format!("/api/v1/transactions/{transaction}"),
        None,
    )
    .await;
    assert_eq!(seen["review"], "unreviewed");
}

/// A prompt injected through a bank's text could ask for these. The token could change them
/// over the API; the tool has no way to.
#[tokio::test]
async fn a_review_cant_write_a_payee_or_notes() {
    let household = household().await;
    let Household {
        server,
        write,
        transaction,
        ..
    } = &household;
    for field in ["payee", "notes"] {
        let answer = tool(
            server,
            write,
            "review_transactions",
            json!({"operations": [{"id": transaction, field: "changed"}]}),
        )
        .await;
        assert!(matches!(answer, Answer::Problem(ref problem) if problem["code"] == "invalid"));
    }
    let (_, seen) = api(
        server,
        write,
        Method::GET,
        &format!("/api/v1/transactions/{transaction}"),
        None,
    )
    .await;
    assert!(seen["payee"].is_null());
    assert!(seen["notes"].is_null());
}

/// The bank's text comes back as what it is, a value in its field.
#[tokio::test]
async fn a_banks_text_is_returned_as_data_only() {
    let household = household().await;
    let answer = tool(
        &household.server,
        &household.read,
        "list_review_queue",
        json!({}),
    )
    .await;
    let Answer::Value(page) = answer else {
        panic!("the queue must answer");
    };
    assert_eq!(page["items"][0]["description"], BANK_TEXT);
}

#[tokio::test]
async fn a_transaction_a_rule_filled_reads_the_same_through_a_tool_and_can_be_listed_by_its_rule() {
    let household = household().await;
    let Household {
        server,
        write,
        read,
        account,
        category,
        ..
    } = &household;
    let (status, rule) = api(
        server,
        write,
        Method::POST,
        "/api/v1/rules",
        Some(&json!({"match_kind": "contains", "pattern": "corner shop", "category": category})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let rule = id(&rule);
    let (status, filled) = api(
        server,
        write,
        Method::POST,
        "/api/v1/transactions",
        Some(
            &json!({"account": account, "posted_on": "2026-10-06", "minor_units": -500,
                     "description": "THE CORNER SHOP"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let filled = id(&filled);
    let (status, _) = api(
        server,
        write,
        Method::POST,
        &format!("/api/v1/rules/{rule}/apply"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    for token in [write, read] {
        for (name, arguments, uri) in [
            (
                "get_transaction",
                json!({"transaction_id": filled}),
                format!("/api/v1/transactions/{filled}"),
            ),
            (
                "list_transactions",
                json!({"rule": rule}),
                format!("/api/v1/transactions?rule={rule}"),
            ),
        ] {
            let (status, body) = api(server, token, Method::GET, &uri, None).await;
            let answer = tool(server, token, name, arguments).await;
            assert_eq!(answer, as_answer(status, body), "{name}");
        }
    }

    let listed = tool(server, read, "list_transactions", json!({"rule": rule})).await;
    let Answer::Value(page) = listed else {
        panic!("a page of what the rule filled");
    };
    assert_eq!(page["items"].as_array().map(Vec::len), Some(1));
    assert_eq!(page["items"][0]["id"], filled);
    assert_eq!(page["items"][0]["categorized_by"]["rule"], rule);
}

#[tokio::test]
async fn the_review_queue_holds_what_is_unreviewed_and_nothing_else() {
    let household = household().await;
    let Household {
        server,
        write,
        transaction,
        ..
    } = &household;
    let reviewed = tool(
        server,
        write,
        "review_transactions",
        json!({"operations": [{"id": transaction, "review": "reviewed"}]}),
    )
    .await;
    assert!(matches!(reviewed, Answer::Value(_)));

    let queue = tool(server, write, "list_review_queue", json!({})).await;
    assert!(matches!(queue, Answer::Value(ref page) if page["items"] == json!([])));

    let asked = tool(
        server,
        write,
        "list_review_queue",
        json!({"review": "reviewed"}),
    )
    .await;
    assert!(matches!(asked, Answer::Problem(ref problem) if problem["code"] == "invalid"));
}
