//! A beneficiary may be a child's name: no log line, problem body or refusal holds it, and
//! neither does anything a token that can't see its account can read.

use super::super::{World, call};
use super::binds::{Feeds, feeds};
use super::{id_of, uri_of};
use axum::http::{Method, StatusCode};
use coffer_domain::identity::{Scope, TokenName};
use coffer_domain::policy::{AccountLevel, GrantTarget, TokenGrant};
use coffer_server::secrets::SecretToken;
use coffer_server::testkit::{capture_logs, captured_log_text, log_marker};
use serde_json::{Value, json};

/// What every label starts with, so finding it anywhere means a leak.
const CHILD: &str = "Child A";
const LABEL: &str = "Child A Quillfeather";
const CHANGED: &str = "Child A Thistlewick";
const BOUND: &str = "Child A Marrowdale";
const COLLECTION: &str = "/api/v1/accounts";

async fn limited(world: &World, target: GrantTarget, level: AccountLevel) -> SecretToken {
    let (_, secret) = world
        .server
        .agent_tokens
        .create(
            &world.alice,
            TokenName::parse("restricted").unwrap(),
            Scope::Read,
            None,
            vec![TokenGrant { target, level }],
        )
        .await
        .unwrap();
    secret
}

fn assert_holds_none(answer: &(StatusCode, Value)) {
    let text = answer.1.to_string();
    assert!(!text.contains(CHILD), "{} holds the label", answer.0);
}

/// A create and a patch that succeed, each answering the label to the one who wrote it.
async fn create_and_patch(feeds: &Feeds) -> Value {
    let (server, write) = (&feeds.world.server, &feeds.write);
    let (status, held) = call(
        server,
        Method::POST,
        COLLECTION,
        write,
        Some(
            json!({"name": "Fund", "kind": "education", "currency": "USD",
                    "beneficiary": LABEL}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(held["beneficiary"], LABEL);
    let (status, changed) = call(
        server,
        Method::PATCH,
        &uri_of(&held),
        write,
        Some(json!({"beneficiary": CHANGED})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(changed["beneficiary"], CHANGED);
    held
}

/// A bind refused for each reason, then one that succeeds.
async fn bind_refused_then_bound(feeds: &Feeds) -> Value {
    let feed = feeds.feed_uri(None).await;
    for body in [
        json!({"kind": "checking", "beneficiary": LABEL}),
        json!({"kind": "custodial", "beneficiary": format!("{LABEL}\u{0007}")}),
        json!({"kind": "custodial", "beneficiary": LABEL, "include_in_forecast": true}),
    ] {
        let answer = feeds.create_on(&feed, body).await;
        assert_eq!(answer.0, StatusCode::UNPROCESSABLE_ENTITY);
        assert_holds_none(&answer);
    }
    let (status, linked) = feeds
        .create_on(&feed, json!({"kind": "custodial", "beneficiary": BOUND}))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(linked["account"]["beneficiary"], BOUND);
    linked["account"].clone()
}

/// Every refusal with the label in front of it: the extractor's and the rule's.
async fn refusals_hold_none(feeds: &Feeds, held: &Value) {
    let (server, write) = (&feeds.world.server, &feeds.write);
    for body in [
        json!({"name": "Fund", "kind": "education", "currency": "USD", "beneficiary": [LABEL]}),
        json!({"name": "Fund", "kind": LABEL, "currency": "USD"}),
        json!({"name": "Fund", "kind": "education", "currency": "USD", "belongs_to": LABEL}),
        json!({"name": "Fund", "kind": "education", "currency": "USD", LABEL: "x"}),
        json!({"name": "Fund", "kind": "checking", "currency": "USD", "beneficiary": LABEL}),
        json!({"name": "Fund", "kind": "education", "currency": "USD",
               "beneficiary": format!("{LABEL}{}", "x".repeat(64))}),
        json!({"name": "Fund", "kind": "education", "currency": "USD", "beneficiary": LABEL,
               "include_in_net_worth": true}),
    ] {
        let answer = call(server, Method::POST, COLLECTION, write, Some(body)).await;
        assert!(answer.0.is_client_error(), "{}", answer.0);
        assert_holds_none(&answer);
    }
    for body in [
        json!({"beneficiary": format!("{LABEL}\u{0007}")}),
        json!({"belongs_to": "household", "beneficiary": LABEL}),
        json!({"beneficiary": LABEL, "include_in_forecast": true}),
    ] {
        let answer = call(server, Method::PATCH, &uri_of(held), write, Some(body)).await;
        assert_eq!(answer.0, StatusCode::UNPROCESSABLE_ENTITY);
        assert_holds_none(&answer);
    }
}

/// A token that can't see them, at each read that could name them, and the audit log.
async fn unseen_reads_hold_none(feeds: &Feeds, held: &Value, bound: &Value) {
    let (server, write) = (&feeds.world.server, &feeds.write);
    let (status, other) = call(
        server,
        Method::POST,
        COLLECTION,
        write,
        Some(json!({"name": "Other", "kind": "checking", "currency": "USD"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let entity = held["id"].as_str().unwrap();
    for level in [AccountLevel::Balances, AccountLevel::Transactions] {
        let token = limited(&feeds.world, GrantTarget::Account(id_of(&other)), level).await;
        for (uri, expected) in [
            (uri_of(held), StatusCode::NOT_FOUND),
            (uri_of(bound), StatusCode::NOT_FOUND),
            (COLLECTION.to_owned(), StatusCode::OK),
            ("/api/v1/provider-accounts".to_owned(), StatusCode::OK),
            ("/api/v1/connections".to_owned(), StatusCode::OK),
            ("/api/v1/audit".to_owned(), StatusCode::FORBIDDEN),
            (
                format!("/api/v1/audit?entity_type=account&entity_id={entity}"),
                StatusCode::FORBIDDEN,
            ),
        ] {
            let answer = call(server, Method::GET, &uri, &token, None).await;
            assert_eq!(answer.0, expected, "{uri}");
            assert_holds_none(&answer);
        }
    }
    // Seeing every row at `balances` isn't enough for the audit log, which names them all.
    let every_row = limited(&feeds.world, GrantTarget::All, AccountLevel::Balances).await;
    let answer = call(server, Method::GET, "/api/v1/audit", &every_row, None).await;
    assert_eq!(answer.0, StatusCode::FORBIDDEN);
    assert_holds_none(&answer);
}

#[tokio::test]
async fn a_beneficiary_is_never_logged_or_echoed() {
    capture_logs();
    let feeds = feeds().await;
    let marker = log_marker();

    let held = create_and_patch(&feeds).await;
    let bound = bind_refused_then_bound(&feeds).await;
    refusals_hold_none(&feeds, &held).await;
    unseen_reads_hold_none(&feeds, &held, &bound).await;

    // The audit log holds it as it holds a name, for whoever may read that.
    let (status, audit) = call(
        &feeds.world.server,
        Method::GET,
        "/api/v1/audit?entity_type=account",
        &feeds.write,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        audit.to_string().contains(CHANGED),
        "the audit log holds the label"
    );

    let log = captured_log_text();
    assert!(log.contains(&marker), "the capture still covers this test");
    assert!(!log.contains(CHILD), "a log line holds the label");
}

#[tokio::test]
async fn a_view_s_debug_holds_no_beneficiary() {
    let feeds = feeds().await;
    let feed = feeds.feed_uri(None).await;
    let (status, linked) = feeds
        .create_on(&feed, json!({"kind": "custodial", "beneficiary": LABEL}))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let view = feeds
        .world
        .server
        .ledger
        .get_account(&feeds.world.alice, id_of(&linked["account"]))
        .await
        .unwrap();
    assert_eq!(view.beneficiary.as_deref(), Some(LABEL));
    assert!(
        !format!("{view:?}").contains(CHILD),
        "Debug holds the label"
    );
}
