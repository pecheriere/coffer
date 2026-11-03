//! What a long run finds changed between reading a transaction and writing it. Each test
//! calls one write transaction with what the run had read, as a run reaching it would.

use axum::http::{Method, StatusCode};
use coffer_domain::identity::{Actor, Role, Scope, TokenName, Via};
use coffer_domain::ids::AccountId;
use coffer_domain::policy::{AccountLevel, GrantTarget, TokenGrant};
use coffer_server::auth::AuthError;
use coffer_server::ledger::LedgerError;
use serde_json::json;

use super::{World, world};

/// A write token of alice's as the actor a run holds, and its id.
async fn agent(world: &World, grants: Vec<TokenGrant>) -> Actor {
    let (record, _) = world
        .server
        .agent_tokens
        .create(
            &world.alice,
            TokenName::parse("runner").unwrap(),
            Scope::Write,
            None,
            grants,
        )
        .await
        .unwrap();
    Actor {
        member: world.alice.member,
        role: Role::Admin,
        via: Via::Agent {
            token: record.id,
            scope: Scope::Write,
        },
    }
}

fn token_of(actor: &Actor) -> coffer_domain::ids::AgentTokenId {
    let Via::Agent { token, .. } = actor.via else {
        panic!("an agent");
    };
    token
}

fn write_on(account: AccountId) -> TokenGrant {
    TokenGrant {
        target: GrantTarget::Account(account),
        level: AccountLevel::Write,
    }
}

#[tokio::test]
async fn a_person_who_got_there_first_stands() {
    let world = world().await;
    let account = world.account("Everyday").await;
    let feed = world.feed(&account).await;
    let id = world.arrive(&feed, "a", -100, "SHOP").await;
    let chosen = world.category("Chosen").await;
    let groceries = world.category("Groceries").await;
    let rule = world.filing("SHOP", groceries).await;
    // The run read it open. Then a person files it, and another person clears a second one.
    assert_eq!(world.dry_run(&rule).await["would_change"], 1);
    world
        .patch_transaction(
            id,
            json!({"splits": [{"category": chosen, "minor_units": -100}]}),
        )
        .await;

    let done = world
        .ledger
        .fill_rule_batch(&world.alice, rule.parse().unwrap(), &[id])
        .await
        .unwrap();

    assert_eq!(done.changed, 0);
    let stored = world.stored(id).await;
    assert_eq!(stored.splits[0].category, chosen);
    assert_eq!(stored.categorized.unwrap().rule, None);
    assert_eq!(world.events("rule.apply").await.len(), 0);
}

#[tokio::test]
async fn a_rule_deleted_or_changed_midway_stops_filling_as_it_was() {
    let world = world().await;
    let account = world.account("Everyday").await;
    let feed = world.feed(&account).await;
    let id = world.arrive(&feed, "a", -100, "SHOP").await;
    let groceries = world.category("Groceries").await;
    let rule = world.filing("SHOP", groceries).await;
    let parsed = rule.parse().unwrap();
    let uri = format!("/api/v1/rules/{rule}");

    let (status, _) = world
        .call(
            Method::PATCH,
            &uri,
            &world.write,
            Some(json!({"pattern": "NOTHING LIKE IT"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let done = world
        .ledger
        .fill_rule_batch(&world.alice, parsed, &[id])
        .await
        .unwrap();
    assert_eq!(done.changed, 0, "it no longer matches");

    let (status, _) = world.call(Method::DELETE, &uri, &world.write, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let done = world
        .ledger
        .fill_rule_batch(&world.alice, parsed, &[id])
        .await
        .unwrap();
    assert_eq!(done.changed, 0);
    assert_eq!(world.stored(id).await.splits.len(), 0);
}

#[tokio::test]
async fn a_token_revoked_midway_writes_nothing_more() {
    let world = world().await;
    let account = world.account("Everyday").await;
    let feed = world.feed(&account).await;
    let id = world.arrive(&feed, "a", -100, "SHOP").await;
    let groceries = world.category("Groceries").await;
    let rule = world.filing("SHOP", groceries).await;
    let actor = agent(&world, Vec::new()).await;
    world
        .server
        .agent_tokens
        .revoke(&world.alice, token_of(&actor))
        .await
        .unwrap();

    let answer = world
        .ledger
        .fill_rule_batch(&actor, rule.parse().unwrap(), &[id])
        .await;

    assert!(matches!(
        answer,
        Err(LedgerError::Auth(AuthError::Unauthenticated))
    ));
    assert_eq!(world.stored(id).await.splits.len(), 0);
}

#[tokio::test]
async fn a_grant_taken_away_midway_writes_nothing_more() {
    let world = world().await;
    let (mine, other) = (world.account("Mine").await, world.account("Other").await);
    let feed = world.feed(&mine).await;
    let id = world.arrive(&feed, "a", -100, "SHOP").await;
    let groceries = world.category("Groceries").await;
    let rule = world
        .rule(json!({
            "match_kind": "contains", "pattern": "SHOP", "category": groceries, "account": mine.id,
        }))
        .await;
    let actor = agent(&world, vec![write_on(mine.id)]).await;
    world
        .server
        .agent_tokens
        .replace_grants(&world.alice, token_of(&actor), vec![write_on(other.id)])
        .await
        .unwrap();

    let answer = world
        .ledger
        .fill_rule_batch(&actor, rule.parse().unwrap(), &[id])
        .await;

    assert!(matches!(answer, Err(LedgerError::Denied(_))));
    assert_eq!(world.stored(id).await.splits.len(), 0);
}

#[tokio::test]
async fn a_member_disabled_midway_writes_nothing_more_and_a_new_role_applies() {
    let world = world().await;
    let account = world.account("Everyday").await;
    let feed = world.feed(&account).await;
    let id = world.arrive(&feed, "a", -100, "SHOP").await;
    let groceries = world.category("Groceries").await;
    let rule = world.filing("SHOP", groceries).await;
    let parsed = rule.parse().unwrap();
    let bob = world.second_member().await;
    // Bob's run started while Bob was an admin; Bob's role in the store is `member`, which
    // still writes, so the run holds. Disabled, it stops.
    let running = Actor {
        member: bob,
        role: Role::Admin,
        via: Via::Session,
    };
    world
        .server
        .identity
        .disable_member(&world.alice, bob)
        .await
        .unwrap();

    let answer = world.ledger.fill_rule_batch(&running, parsed, &[id]).await;

    assert!(matches!(
        answer,
        Err(LedgerError::Auth(AuthError::Unauthenticated))
    ));
    assert_eq!(world.stored(id).await.splits.len(), 0);
}

#[tokio::test]
async fn a_dry_run_says_when_it_stopped_reading() {
    let world = world().await;
    let account = world.account("Everyday").await;
    let feed = world.feed(&account).await;
    let records = (0..3)
        .map(|n| super::record(&format!("r{n}"), -100, "SHOP"))
        .collect();
    world.sync(&feed, records).await;
    let groceries = world.category("Groceries").await;
    let rule = world.filing("SHOP", groceries).await;

    let bounded = world
        .ledger
        .dry_run_rule_reading(&world.alice, rule.parse().unwrap(), None, 2)
        .await
        .unwrap();
    assert_eq!(bounded.would_change, 2);
    assert!(!bounded.complete);

    let whole = world.dry_run(&rule).await;
    assert_eq!(whole["would_change"], 3);
    assert_eq!(whole["complete"], true);
}
