//! Channels past the report: another member's token, a replayed create, pages of transactions,
//! planned items, the pages, a label guessed, and a `Debug` that could reach a log line.

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use coffer_domain::identity::{Actor, Scope, TokenName};
use coffer_domain::ids::AccountId;
use coffer_domain::net_worth::NetWorthInterval;
use coffer_domain::policy::{AccountLevel, AccountScope, TokenGrant};
use coffer_server::http::router;
use coffer_server::secrets::SecretToken;
use http_body_util::BodyExt as _;
use secrecy::ExposeSecret as _;
use serde_json::{Value, json};
use tower::ServiceExt as _;

use super::super::net_worth_channels::{HIDDEN_MINOR, RANGE, TODAY, all, day, money};
use super::super::{World, on};
use super::family::{
    Answer, BUCKET_MINOR, Family, HIDDEN_LABEL, HIDDEN_NAME, LABEL, SINCE, assert_same_answers,
    assert_twins, family, ids_by_first_use,
};

impl World {
    async fn member_token(
        &self,
        member: &Actor,
        scope: Scope,
        grants: Vec<TokenGrant>,
    ) -> SecretToken {
        let (_, secret) = self
            .server
            .agent_tokens
            .create(
                member,
                TokenName::parse("bobs").unwrap(),
                scope,
                None,
                grants,
            )
            .await
            .unwrap();
        secret
    }
}

/// `answers` without what names who is asking.
fn about_the_household(answers: Vec<Answer>) -> Vec<Answer> {
    answers
        .into_iter()
        .filter(|answer| !answer.0.ends_with("/api/v1/me"))
        .collect()
}

/// Visibility follows the grants and not the member: a member's token and an admin's with the same
/// grants read the same, and a member's without grants sees every bucket, as a session does.
#[tokio::test]
async fn a_member_s_token_reads_like_an_admin_s() {
    let held = family().await;
    held.world.held_for(HIDDEN_LABEL, HIDDEN_MINOR).await;
    let bob = held.world.bob().await;
    for level in [
        AccountLevel::Balances,
        AccountLevel::Transactions,
        AccountLevel::Write,
    ] {
        let grants = vec![on(held.home(), level), on(held.emma(), level)];
        let (_, alices) = held.world.token(Scope::Write, grants.clone()).await;
        let bobs = held.world.member_token(&bob, Scope::Write, grants).await;
        assert_same_answers(
            &about_the_household(held.answers(&alices).await),
            &about_the_household(held.answers(&bobs).await),
        );
        let report = held.text(RANGE, &bobs).await;
        assert!(report.contains(LABEL) && !report.contains(HIDDEN_LABEL));
    }

    let unrestricted = held.world.member_token(&bob, Scope::Read, Vec::new()).await;
    assert!(held.text(RANGE, &unrestricted).await.contains(HIDDEN_LABEL));
    assert!(
        held.text("/api/v1/accounts", &unrestricted)
            .await
            .contains(HIDDEN_LABEL)
    );
}

/// A create's stored answer holds the label, so a retry under a grant that lost the right gets no
/// replay, and another token holding the key makes its own.
#[tokio::test]
async fn a_replayed_create_shows_no_label_once_the_grant_is_gone() {
    let held = family().await;
    let (id, token) = held
        .world
        .token(Scope::Write, vec![all(AccountLevel::Write)])
        .await;
    let request = json!({
        "name": HIDDEN_NAME,
        "kind": "education",
        "currency": "USD",
        "beneficiary": HIDDEN_LABEL,
    });
    let create = async |token: &SecretToken| {
        held.world
            .send(
                Method::POST,
                "/api/v1/accounts",
                token,
                Some(&request),
                Some("one-key"),
            )
            .await
    };
    let (status, first) = create(&token).await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(first.contains(HIDDEN_LABEL));
    assert_eq!(create(&token).await, (status, first.clone()), "a replay");

    for narrower in [
        vec![all(AccountLevel::Transactions)],
        vec![on(held.home(), AccountLevel::Write)],
    ] {
        held.world
            .server
            .agent_tokens
            .replace_grants(&held.world.alice, id, narrower)
            .await
            .unwrap();
        let (status, body) = create(&token).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(!body.contains(HIDDEN_LABEL) && !body.contains(HIDDEN_NAME));
    }

    let (_, other) = held
        .world
        .token(Scope::Write, vec![all(AccountLevel::Write)])
        .await;
    let (status, theirs) = create(&other).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_ne!(theirs, first, "its own account, not the other's replayed");
}

/// The same for a transaction's stored answer, once its account is hidden: a retry answers as the
/// same request on an account that doesn't exist.
#[tokio::test]
async fn a_replayed_transaction_shows_nothing_once_its_account_is_hidden() {
    let held = family().await;
    let (id, token) = held
        .world
        .token(Scope::Write, vec![on(held.emma(), AccountLevel::Write)])
        .await;
    let request = |account: AccountId| {
        json!({
            "account": account,
            "posted_on": "2026-10-28",
            "minor_units": -1000,
            "description": "Quiet tuition",
        })
    };
    let post = async |account: AccountId, key: &str| {
        let (status, body) = held
            .world
            .send(
                Method::POST,
                "/api/v1/transactions",
                &token,
                Some(&request(account)),
                Some(key),
            )
            .await;
        (status, ids_by_first_use(&body))
    };
    let (status, first) = post(held.emma(), "one-key").await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(first.contains("Quiet tuition"));

    held.world
        .server
        .agent_tokens
        .replace_grants(
            &held.world.alice,
            id,
            vec![on(held.home(), AccountLevel::Write)],
        )
        .await
        .unwrap();

    let replay = post(held.emma(), "one-key").await;
    assert_eq!(replay.0, StatusCode::NOT_FOUND, "{}", replay.1);
    assert_eq!(replay, post(AccountId::new(), "another-key").await);
}

/// A hidden bucket account's transactions move no page, no cursor's presence and no count for a
/// token reading another account's.
#[tokio::test]
async fn a_hidden_bucket_s_transactions_move_no_page() {
    let (bare, held) = (family().await, family().await);
    let hidden = held.world.held_for(LABEL, HIDDEN_MINOR).await;
    let mut pages = Vec::new();
    for scene in [&bare, &held] {
        let world = &scene.world;
        if scene.visible == held.visible {
            world.transaction(hidden, 3, "Quiet first").await;
        }
        world.transaction(scene.home(), 2, "Older").await;
        if scene.visible == held.visible {
            world.transaction(hidden, 2, "Quiet between").await;
        }
        world.transaction(scene.home(), 1, "Oldest").await;
        let (_, token) = world
            .token(
                Scope::Read,
                vec![on(scene.home(), AccountLevel::Transactions)],
            )
            .await;
        let mut uri = "/api/v1/transactions?limit=1".to_owned();
        let mut walked = Vec::new();
        loop {
            let (status, body) = world.get(&uri, &token).await;
            assert_eq!(status, StatusCode::OK);
            walked.push(ids_by_first_use(&body["items"].to_string()));
            let Some(next) = body["next"].as_str() else {
                break;
            };
            uri = format!("/api/v1/transactions?limit=1&after={next}");
        }
        pages.push(walked);
    }
    assert_eq!(pages[0].len(), 2);
    assert_eq!(pages[0], pages[1], "the same pages");
}

/// An item with no hint is household data a token holding `transactions` reads, and a split on a
/// hidden bucket account linked to it changes nothing in it. An item hinted at the hidden account
/// answers as a missing one.
#[tokio::test]
async fn a_planned_item_doesn_t_feel_a_hidden_bucket() {
    let (bare, held) = (family().await, family().await);
    let hidden = held.world.held_for(HIDDEN_LABEL, HIDDEN_MINOR).await;
    let mut named = Vec::new();
    for scene in [&bare, &held] {
        let category = scene.world.expense_category().await;
        let goal = scene
            .world
            .planned_item("Tuition", None, category, 5_000)
            .await;
        named.push((category, goal));
    }
    let (category, goal) = named[1];
    let transaction = held.world.transaction(hidden, 1, "Quiet tuition").await;
    held.world
        .link_split(transaction, category, -1000, goal)
        .await;
    let hinted = held
        .world
        .planned_item("Quiet goal", Some(hidden), category, 7_000)
        .await;

    let mut items = Vec::new();
    for (scene, (_, goal)) in [&bare, &held].into_iter().zip(&named) {
        let (_, token) = scene
            .world
            .token(
                Scope::Read,
                vec![on(scene.home(), AccountLevel::Transactions)],
            )
            .await;
        let mut answers = Vec::new();
        for uri in [
            "/api/v1/plan-items".to_owned(),
            "/api/v1/plan-items?include_inactive=true".to_owned(),
            format!("/api/v1/plan-items/{goal}"),
            "/api/v1/transactions".to_owned(),
        ] {
            answers.push(scene.answer(&uri, &token).await);
        }
        items.push(answers);
        if scene.visible == held.visible {
            let hinted = format!("/api/v1/plan-items/{hinted}");
            let missing = format!("/api/v1/plan-items/{}", AccountId::new());
            let one = scene.answer(&hinted, &token).await;
            assert_eq!(one.1, StatusCode::NOT_FOUND);
            assert_eq!(one, scene.answer(&missing, &token).await);
        }
    }
    assert_same_answers(&items[0], &items[1]);
    assert!(items[1][0].3.contains("Tuition") && !items[1][0].3.contains("Quiet goal"));
}

/// Every other route naming a hidden bucket account answers as one naming a missing account does,
/// reading or writing, every byte. A request that fails on the hidden id must not fail differently.
#[tokio::test]
async fn a_hidden_bucket_answers_like_a_missing_account_everywhere() {
    let held = family().await;
    let hidden = held.world.held_for(HIDDEN_LABEL, HIDDEN_MINOR).await;
    let missing = AccountId::new();
    let (_, writer) = held
        .world
        .token(Scope::Write, vec![on(held.home(), AccountLevel::Write)])
        .await;
    let category = held.world.expense_category().await;
    let attempts = |id: AccountId| -> Vec<(Method, String, Option<Value>)> {
        vec![
            (Method::GET, format!("/api/v1/accounts/{id}"), None),
            (Method::HEAD, format!("/api/v1/accounts/{id}"), None),
            (Method::GET, format!("/api/v1/accounts/{id}/balances"), None),
            (
                Method::GET,
                format!("/api/v1/transactions?account={id}"),
                None,
            ),
            (
                Method::PUT,
                format!("/api/v1/accounts/{id}/balances/2026-11-01"),
                Some(json!({"minor_units": 1})),
            ),
            (
                Method::POST,
                "/api/v1/transactions".to_owned(),
                Some(json!({
                    "account": id,
                    "posted_on": "2026-10-28",
                    "minor_units": -1,
                    "description": "x",
                })),
            ),
            (
                Method::POST,
                "/api/v1/plan-items".to_owned(),
                Some(json!({
                    "name": "x",
                    "category": category,
                    "amount": {"minor_units": 1, "currency": "USD"},
                    "direction": "outflow",
                    "account_hint": id,
                    "schedule": {"type": "one_off", "on": "2026-12-20"},
                })),
            ),
        ]
    };
    for (one, other) in attempts(hidden).into_iter().zip(attempts(missing)) {
        let (status, body) = held
            .world
            .send(one.0.clone(), &one.1, &writer, one.2.as_ref(), None)
            .await;
        let (other_status, other_body) = held
            .world
            .send(other.0, &other.1, &writer, other.2.as_ref(), None)
            .await;
        // Creating a planned item needs `all` at `write`, refused before its hint is looked at.
        let expected = if one.1.ends_with("/plan-items") {
            StatusCode::FORBIDDEN
        } else {
            StatusCode::NOT_FOUND
        };
        assert_eq!(status, expected, "{} {}", one.0, one.1);
        assert_eq!(status, other_status, "{} {}", one.0, one.1);
        assert_eq!(
            ids_by_first_use(&body),
            ids_by_first_use(&other_body),
            "{} {}",
            one.0,
            one.1
        );
        assert!(!body.contains(HIDDEN_LABEL));
    }
}

/// Guessing: a token that may write gives its account the label of a bucket it can't see. That
/// makes a bucket of what it sees, no sum, and no answer tells a right guess from a wrong one.
#[tokio::test]
async fn a_label_guessed_tells_nothing() {
    let (bare, held) = (family().await, family().await);
    held.world.held_for(HIDDEN_LABEL, HIDDEN_MINOR).await;
    let mut seen = Vec::new();
    for scene in [&bare, &held] {
        let (_, writer) = scene
            .world
            .token(
                Scope::Write,
                vec![
                    on(scene.home(), AccountLevel::Write),
                    on(scene.emma(), AccountLevel::Write),
                ],
            )
            .await;
        let (status, patched) = scene
            .world
            .send(
                Method::PATCH,
                &format!("/api/v1/accounts/{}", scene.emma()),
                &writer,
                Some(&json!({"beneficiary": HIDDEN_LABEL})),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        let report = scene.report(&writer).await;
        assert_eq!(report["buckets"].as_array().unwrap().len(), 1);
        assert_eq!(
            report["buckets"][0]["points"]
                .as_array()
                .unwrap()
                .last()
                .unwrap()["total"],
            money(BUCKET_MINOR, "USD")
        );
        seen.push((
            ids_by_first_use(&patched),
            ids_by_first_use(&report.to_string()),
        ));
    }
    assert_eq!(seen[0], seen[1], "byte-identical");
}

async fn bearer_on_a_page(scene: &Family, uri: &str) -> (StatusCode, String) {
    let request = Request::builder()
        .uri(uri)
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", scene.admin.expose_secret()),
        )
        .body(Body::empty())
        .unwrap();
    let response = router(scene.world.server.app_state())
        .oneshot(request)
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

/// The pages that list buckets and offer labels take a session alone: a bearer token, even an
/// admin's, gets what anyone signed out does.
#[tokio::test]
async fn the_pages_holding_labels_take_no_token() {
    let held = family().await;
    held.world.held_for(HIDDEN_LABEL, HIDDEN_MINOR).await;
    held.world.connection(&[("Noah bank", None)]).await;
    for uri in ["/net-worth", "/connections"] {
        let (status, body) = bearer_on_a_page(&held, uri).await;
        assert_ne!(status, StatusCode::OK, "{uri}");
        assert!(
            !body.contains(HIDDEN_LABEL) && !body.contains(LABEL),
            "{uri}"
        );
    }
}

/// A label reaches no `Debug`, which could reach a log line (B5): not of a report, a record, or
/// an account as answered.
#[tokio::test]
async fn no_debug_holds_a_label() {
    let held = family().await;
    let report = held
        .world
        .server
        .ledger
        .net_worth(
            &held.world.alice,
            serde_json::from_value(json!({"from": SINCE, "to": TODAY, "interval": "day"})).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(report.buckets.len(), 1);
    let record = held
        .world
        .server
        .store
        .get_account(held.emma())
        .await
        .unwrap();
    let view = held
        .world
        .server
        .ledger
        .get_account(&held.world.alice, held.emma())
        .await
        .unwrap();
    let data = held
        .world
        .server
        .store
        .net_worth_data(
            &AccountScope::All,
            day(SINCE),
            day(TODAY),
            NetWorthInterval::Day,
        )
        .await
        .unwrap();
    for debug in [
        format!("{report:?}"),
        format!("{data:?}"),
        format!("{:?}", report.buckets[0]),
        format!("{record:?}"),
        format!("{view:?}"),
    ] {
        assert!(!debug.contains(LABEL));
    }
    assert_eq!(view.beneficiary.as_deref(), Some(LABEL));
}

/// Twins with the visible accounts bound to one connection and the hidden account to the same one:
/// the connection reads alike, holding only the provider accounts of what the token sees.
#[tokio::test]
async fn a_connection_shows_only_the_provider_accounts_of_what_the_token_sees() {
    let (bare, held) = (family().await, family().await);
    let hidden = held.world.held_for(HIDDEN_LABEL, HIDDEN_MINOR).await;
    let bank = "Family bank account";
    bare.world.connection(&[(bank, Some(bare.emma()))]).await;
    held.world
        .connection(&[(bank, Some(held.emma())), ("Noah bank", Some(hidden))])
        .await;
    assert_twins(&bare, &held).await;
    let connections = held.text("/api/v1/connections", &held.token).await;
    assert!(connections.contains(bank) && !connections.contains("Noah bank"));
    let everything = held.text("/api/v1/connections", &held.admin).await;
    assert!(everything.contains("Noah bank"));
    assert!(
        !everything.contains(HIDDEN_LABEL),
        "a connection holds no label for anyone"
    );
}

/// A connection is household data, shown to any reading token whether or not it serves an account
/// the token sees: this is what that shows, so it can't grow unnoticed.
#[tokio::test]
async fn a_connection_serving_only_a_hidden_account_shows_no_more_than_itself() {
    let held = family().await;
    let hidden = held.world.held_for(HIDDEN_LABEL, HIDDEN_MINOR).await;
    held.world.connection(&[("Noah bank", Some(hidden))]).await;
    let (status, body) = held.world.get("/api/v1/connections", &held.token).await;
    assert_eq!(status, StatusCode::OK);
    let connection = &body["items"][0];
    assert_eq!(connection["accounts"], json!([]));
    assert_eq!(connection["last_error"], Value::Null);
    assert_eq!(connection["held_cursor"], Value::Null);
    let fields: Vec<&str> = connection
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        fields,
        [
            "accounts",
            "held_cursor",
            "id",
            "label",
            "last_attempt_at",
            "last_error",
            "last_success_at",
            "next_sync_at",
            "owner",
            "provider",
            "status",
        ]
    );
    assert!(!body.to_string().contains(HIDDEN_LABEL));
    assert_eq!(
        held.world
            .get("/api/v1/provider-accounts", &held.token)
            .await
            .1["items"],
        json!([])
    );
    let answers = held.answers(&held.token).await;
    assert!(answers.iter().all(|answer| !answer.3.contains(HIDDEN_NAME)));
}
