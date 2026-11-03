//! What a token with account grants may do with rules, and may learn through them.

use axum::http::{Method, StatusCode};
use coffer_domain::identity::Scope;
use coffer_domain::policy::{AccountLevel, GrantTarget, TokenGrant};
use serde_json::json;

use super::{record, world};

fn on(account: coffer_domain::ids::AccountId, level: AccountLevel) -> TokenGrant {
    TokenGrant {
        target: GrantTarget::Account(account),
        level,
    }
}

#[tokio::test]
async fn a_rule_for_one_account_needs_write_on_it_and_one_for_all_on_all() {
    let world = world().await;
    let mine = world.account("Mine").await;
    let theirs = world.account("Theirs").await;
    let category = world.category("Groceries").await;
    let narrow = world
        .token(Scope::Write, vec![on(mine.id, AccountLevel::Write)])
        .await;
    let reader = world
        .token(Scope::Write, vec![on(mine.id, AccountLevel::Transactions)])
        .await;
    let create = |account: Option<_>| json!({"match_kind": "contains", "pattern": "S", "category": category, "account": account});

    let (status, _) = world
        .call(Method::POST, "/api/v1/rules", &narrow, Some(create(None)))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "not for every account");
    let (status, _) = world
        .call(
            Method::POST,
            "/api/v1/rules",
            &narrow,
            Some(create(Some(theirs.id))),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "a hidden account is missing");
    let (status, _) = world
        .call(
            Method::POST,
            "/api/v1/rules",
            &reader,
            Some(create(Some(mine.id))),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "reading isn't writing");
    let (status, rule) = world
        .call(
            Method::POST,
            "/api/v1/rules",
            &narrow,
            Some(create(Some(mine.id))),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{rule}");
    let uri = format!("/api/v1/rules/{}", rule["id"].as_str().unwrap());

    for body in [json!({"account": null}), json!({"account": theirs.id})] {
        let (status, _) = world.call(Method::PATCH, &uri, &narrow, Some(body)).await;
        assert_ne!(status, StatusCode::OK, "it can't reach past its account");
    }
    let (status, _) = world
        .call(
            Method::PATCH,
            &uri,
            &reader,
            Some(json!({"enabled": false})),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = world.call(Method::DELETE, &uri, &reader, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = world
        .call(Method::POST, &format!("{uri}/apply"), &reader, None)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = world.call(Method::DELETE, &uri, &narrow, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn a_read_scope_can_look_but_not_change() {
    let world = world().await;
    let category = world.category("Groceries").await;
    let rule = world.filing("S", category).await;
    let read = world.token(Scope::Read, Vec::new()).await;

    let (status, list) = world.call(Method::GET, "/api/v1/rules", &read, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    let (status, _) = world
        .call(
            Method::GET,
            &format!("/api/v1/rules/{rule}/dry-run"),
            &read,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = world
        .call(
            Method::POST,
            &format!("/api/v1/rules/{rule}/apply"),
            &read,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = world
        .call(
            Method::POST,
            "/api/v1/rules",
            &read,
            Some(json!({"match_kind": "contains", "pattern": "S", "category": category})),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_rule_on_a_hidden_account_is_missing() {
    let world = world().await;
    let mine = world.account("Mine").await;
    let theirs = world.account("Theirs").await;
    let category = world.category("Groceries").await;
    let hidden = world
        .rule(json!({
            "match_kind": "contains", "pattern": "PRIVATE CLINIC", "category": category,
            "account": theirs.id,
        }))
        .await;
    let shared = world.filing("SHOP", category).await;
    let narrow = world
        .token(Scope::Write, vec![on(mine.id, AccountLevel::Write)])
        .await;
    let balances = world
        .token(Scope::Write, vec![on(mine.id, AccountLevel::Balances)])
        .await;

    let (status, list) = world
        .call(Method::GET, "/api/v1/rules", &narrow, None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let ids: Vec<_> = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|rule| rule["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        ids,
        std::slice::from_ref(&shared),
        "one for every account shows"
    );
    assert!(!list.to_string().contains("PRIVATE"));
    for (method, uri) in [
        (Method::GET, format!("/api/v1/rules/{hidden}")),
        (Method::GET, format!("/api/v1/rules/{hidden}/dry-run")),
        (Method::POST, format!("/api/v1/rules/{hidden}/apply")),
        (Method::DELETE, format!("/api/v1/rules/{hidden}")),
    ] {
        let (status, _) = world.call(method, &uri, &narrow, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    // Holding only balances shows no rule at all: nothing here is at that level.
    let (status, list) = world
        .call(Method::GET, "/api/v1/rules", &balances, None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["items"], json!([]));
    let (status, _) = world
        .call(
            Method::GET,
            &format!("/api/v1/rules/{shared}"),
            &balances,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_dry_run_counts_nothing_a_token_cannot_read() {
    let world = world().await;
    let mine = world.account("Mine").await;
    let theirs = world.account("Theirs").await;
    let my_feed = world.feed(&mine).await;
    let their_feed = world.feed(&theirs).await;
    world
        .sync(&my_feed, vec![record("a", -100, "SHOP MINE")])
        .await;
    let records = vec![
        record("b", -100, "SHOP THEIRS ONE"),
        record("c", -200, "SHOP THEIRS TWO"),
    ];
    world.sync(&their_feed, records).await;
    let reviewed = world.holding(&their_feed, "c").await;
    world
        .patch_transaction(reviewed, json!({"review": "reviewed"}))
        .await;
    let category = world.category("Groceries").await;
    let rule = world.filing("SHOP", category).await;
    let narrow = world
        .token(Scope::Write, vec![on(mine.id, AccountLevel::Transactions)])
        .await;

    let (status, answer) = world
        .call(
            Method::GET,
            &format!("/api/v1/rules/{rule}/dry-run"),
            &narrow,
            None,
        )
        .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(answer["would_change"], 1);
    assert_eq!(answer["left_alone"]["reviewed"], 0, "theirs isn't counted");
    assert_eq!(answer["changes"].as_array().unwrap().len(), 1);
    assert_eq!(answer["changes"][0]["description"], "SHOP MINE");
    assert!(!answer.to_string().contains("THEIRS"));

    let whole = world.dry_run(&rule).await;
    assert_eq!(whole["would_change"], 2);
    assert_eq!(whole["left_alone"]["reviewed"], 1);
}

#[tokio::test]
async fn a_run_by_a_narrow_token_touches_only_its_account() {
    let world = world().await;
    let mine = world.account("Mine").await;
    let theirs = world.account("Theirs").await;
    let my_feed = world.feed(&mine).await;
    let their_feed = world.feed(&theirs).await;
    let my_id = world.arrive(&my_feed, "a", -100, "SHOP").await;
    let their_id = world.arrive(&their_feed, "b", -100, "SHOP").await;
    let category = world.category("Groceries").await;
    let narrow = world
        .token(Scope::Write, vec![on(mine.id, AccountLevel::Write)])
        .await;
    let (status, rule) = world
        .call(
            Method::POST,
            "/api/v1/rules",
            &narrow,
            Some(json!({
                "match_kind": "contains", "pattern": "SHOP", "category": category,
                "account": mine.id,
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let everywhere = world.filing("SHOP", category).await;

    let (status, answer) = world
        .call(
            Method::POST,
            &format!("/api/v1/rules/{}/apply", rule["id"].as_str().unwrap()),
            &narrow,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(answer["changed"], 1);
    assert_eq!(world.stored(my_id).await.splits.len(), 1);
    assert_eq!(world.stored(their_id).await.splits.len(), 0);

    let (status, _) = world
        .call(
            Method::POST,
            &format!("/api/v1/rules/{everywhere}/apply"),
            &narrow,
            None,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a rule for every account is out of reach"
    );
    assert_eq!(world.stored(their_id).await.splits.len(), 0);
}

#[tokio::test]
async fn the_audit_of_a_run_stays_behind_the_audit_log_s_own_grant() {
    let world = world().await;
    let mine = world.account("Mine").await;
    let narrow = world
        .token(Scope::Write, vec![on(mine.id, AccountLevel::Write)])
        .await;

    let (status, _) = world
        .call(Method::GET, "/api/v1/audit?entity_type=rule", &narrow, None)
        .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_new_rule_s_place_says_nothing_of_rules_a_token_cannot_see() {
    let world = world().await;
    let mine = world.account("Mine").await;
    let theirs = world.account("Theirs").await;
    let category = world.category("Groceries").await;
    world
        .rule(json!({
            "match_kind": "contains", "pattern": "P", "category": category,
            "account": theirs.id, "position": 9000,
        }))
        .await;
    let narrow = world
        .token(Scope::Write, vec![on(mine.id, AccountLevel::Write)])
        .await;

    let (status, rule) = world
        .call(
            Method::POST,
            "/api/v1/rules",
            &narrow,
            Some(json!({
                "match_kind": "contains", "pattern": "S", "category": category,
                "account": mine.id,
            })),
        )
        .await;

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(rule["position"], 0);
}
