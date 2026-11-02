//! What a restricted token gets of whose money an account is: it reads at `balances`, changes at
//! `write`, and a hidden account answers exactly like a missing one whatever a request asks.

use super::super::{World, call};
use super::binds::feeds;
use super::{admin, held_529, id_of, make, post, stored, stored_label, uri_of};
use axum::http::{Method, StatusCode};
use coffer_domain::identity::{Scope, TokenName};
use coffer_domain::ids::{AccountId, ProviderAccountId};
use coffer_domain::policy::{AccountLevel, GrantTarget, TokenGrant};
use coffer_server::secrets::SecretToken;
use serde_json::{Value, json};

async fn granted(
    world: &World,
    scope: Scope,
    target: GrantTarget,
    level: AccountLevel,
) -> SecretToken {
    let (_, secret) = world
        .server
        .agent_tokens
        .create(
            &world.alice,
            TokenName::parse("restricted").unwrap(),
            scope,
            None,
            vec![TokenGrant { target, level }],
        )
        .await
        .unwrap();
    secret
}

/// Every request that could tell an agent something by how it is refused.
fn asks() -> [Value; 9] {
    [
        json!({"belongs_to": "someone_else"}),
        json!({"belongs_to": "household"}),
        json!({"beneficiary": "Child B"}),
        json!({"beneficiary": ""}),
        json!({"beneficiary": "Child B\u{0007}"}),
        json!({"include_in_net_worth": true}),
        json!({"include_in_forecast": true}),
        json!({"kind": "education"}),
        json!({"name": "Bad\u{0007}name"}),
    ]
}

#[tokio::test]
async fn a_token_reads_it_at_balances_and_changes_it_at_write() {
    let (world, write) = admin().await;
    let target = held_529(&world, &write).await;
    let hidden = held_529(&world, &write).await;

    let reader = granted(
        &world,
        Scope::Read,
        GrantTarget::Account(id_of(&target)),
        AccountLevel::Balances,
    )
    .await;
    let (status, read) = call(&world.server, Method::GET, &uri_of(&target), &reader, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(read["belongs_to"], "someone_else");
    assert_eq!(read["beneficiary"], "Child A");
    assert_eq!(read["belongs_to_overridden"], false);

    let (status, listed) = call(
        &world.server,
        Method::GET,
        "/api/v1/accounts",
        &reader,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed["items"].as_array().unwrap().len(), 1);
    assert_eq!(listed["items"][0], read);

    let (status, _) = call(&world.server, Method::GET, &uri_of(&hidden), &reader, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let balances = granted(
        &world,
        Scope::Write,
        GrantTarget::Account(id_of(&target)),
        AccountLevel::Balances,
    )
    .await;
    let (status, _) = call(
        &world.server,
        Method::PATCH,
        &uri_of(&target),
        &balances,
        Some(json!({"beneficiary": "Child B"})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let writer = granted(
        &world,
        Scope::Write,
        GrantTarget::Account(id_of(&target)),
        AccountLevel::Write,
    )
    .await;
    let (status, changed) = call(
        &world.server,
        Method::PATCH,
        &uri_of(&target),
        &writer,
        Some(json!({"beneficiary": "Child B"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(changed["beneficiary"], "Child B");
}

#[tokio::test]
async fn below_write_every_change_answers_the_same_refusal() {
    let (world, write) = admin().await;
    let target = held_529(&world, &write).await;
    for (scope, level) in [
        (Scope::Write, AccountLevel::Balances),
        (Scope::Write, AccountLevel::Transactions),
        (Scope::Read, AccountLevel::Balances),
        (Scope::Read, AccountLevel::Transactions),
    ] {
        let limited = granted(&world, scope, GrantTarget::Account(id_of(&target)), level).await;
        let mut answers = Vec::new();
        for ask in asks() {
            answers.push(
                call(
                    &world.server,
                    Method::PATCH,
                    &uri_of(&target),
                    &limited,
                    Some(ask),
                )
                .await,
            );
        }
        assert_eq!(answers[0].0, StatusCode::FORBIDDEN, "{:?}", answers[0].1);
        assert!(
            answers.iter().all(|answer| *answer == answers[0]),
            "a refusal must not depend on what was asked"
        );
    }
    let record = stored(&world, &target).await;
    assert_eq!(stored_label(&record), Some("Child A"), "nothing changed");
}

#[tokio::test]
async fn a_hidden_one_answers_like_a_missing_one_whatever_is_asked() {
    let (world, write) = admin().await;
    let visible = make(&world, &write, "checking").await;
    let held = held_529(&world, &write).await;
    let household = make(&world, &write, "savings").await;
    let writer = granted(
        &world,
        Scope::Write,
        GrantTarget::Account(id_of(&visible)),
        AccountLevel::Write,
    )
    .await;
    let missing = format!("/api/v1/accounts/{}", AccountId::new());

    for hidden in [&held, &household] {
        let uri = uri_of(hidden);
        for ask in asks() {
            let answer = call(
                &world.server,
                Method::PATCH,
                &uri,
                &writer,
                Some(ask.clone()),
            )
            .await;
            let absent = call(&world.server, Method::PATCH, &missing, &writer, Some(ask)).await;
            assert_eq!(answer.0, StatusCode::NOT_FOUND);
            assert!(answer == absent, "hidden must answer like missing");
        }
        let answer = call(&world.server, Method::GET, &uri, &writer, None).await;
        let absent = call(&world.server, Method::GET, &missing, &writer, None).await;
        assert_eq!(answer.0, StatusCode::NOT_FOUND);
        assert!(answer == absent, "hidden must answer like missing");
    }
}

#[tokio::test]
async fn a_create_without_every_row_at_write_is_refused_whatever_is_asked() {
    let (world, write) = admin().await;
    let visible = make(&world, &write, "checking").await;
    let limited = granted(
        &world,
        Scope::Write,
        GrantTarget::Account(id_of(&visible)),
        AccountLevel::Write,
    )
    .await;
    let mut answers = Vec::new();
    for body in [
        json!({"name": "Fund", "kind": "education", "currency": "USD"}),
        json!({"name": "Fund", "kind": "education", "currency": "USD",
               "include_in_net_worth": true}),
        json!({"name": "Fund", "kind": "education", "currency": "USD",
               "beneficiary": "Child\u{0007}"}),
        json!({"name": "Fund", "kind": "checking", "currency": "USD",
               "beneficiary": "Child A"}),
        json!({"name": "Bad\u{0007}name", "kind": "education", "currency": "USD"}),
    ] {
        answers.push(
            call(
                &world.server,
                Method::POST,
                "/api/v1/accounts",
                &limited,
                Some(body),
            )
            .await,
        );
    }
    assert_eq!(answers[0].0, StatusCode::FORBIDDEN);
    assert!(
        answers.iter().all(|answer| *answer == answers[0]),
        "a refusal must not depend on what was asked"
    );
}

#[tokio::test]
async fn an_all_grant_at_write_may_create_one_held_for_someone() {
    let (world, _) = admin().await;
    let all = granted(&world, Scope::Write, GrantTarget::All, AccountLevel::Write).await;
    let created = post(
        &world,
        &all,
        json!({"name": "Fund", "kind": "education", "currency": "USD",
               "beneficiary": "Child A"}),
    )
    .await;
    assert_eq!(created["beneficiary"], "Child A");
}

#[tokio::test]
async fn a_bind_without_every_row_at_write_is_refused_whatever_is_asked() {
    let feeds = feeds().await;
    let uri = feeds.feed_uri(None).await;
    let visible = post(
        &feeds.world,
        &feeds.write,
        json!({"name": "Other", "kind": "checking", "currency": "USD"}),
    )
    .await;
    let limited = granted(
        &feeds.world,
        Scope::Write,
        GrantTarget::Account(id_of(&visible)),
        AccountLevel::Write,
    )
    .await;
    let bind = |uri: String, body: Value| {
        let (server, limited) = (&feeds.world.server, &limited);
        async move {
            call(
                server,
                Method::POST,
                &format!("{uri}/create-account"),
                limited,
                Some(body),
            )
            .await
        }
    };
    let first = bind(uri.clone(), json!({"kind": "education"})).await;
    assert_eq!(first.0, StatusCode::FORBIDDEN);
    for body in asks() {
        assert!(
            bind(uri.clone(), body).await == first,
            "a refusal must not depend on what was asked"
        );
    }
    let missing = format!("/api/v1/provider-accounts/{}", ProviderAccountId::new());
    assert!(
        bind(missing, json!({"kind": "education"})).await == first,
        "a missing feed answers the same"
    );
}
