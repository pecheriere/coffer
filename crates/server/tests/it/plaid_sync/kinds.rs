//! What Plaid says an account is, through a real sync: its subtype is a provider's hint, which a
//! bind may fall back on, and which a guess from the name never overrides.

use super::{GET_ACCOUNTS, World, account_json, accounts_answer, page, world};
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use coffer_domain::accounts::{AccountKind, BelongsTo};
use coffer_domain::identity::{Scope, TokenName};
use coffer_server::http::router;
use coffer_server::sync::connections::NewLinkedAccountInput;
use http_body_util::BodyExt as _;
use secrecy::ExposeSecret as _;
use serde_json::{Value, json};
use tower::ServiceExt as _;

/// A world in which Plaid reported one account, after a real sync.
async fn synced(id: &str, kind: &str, subtype: &str, name: &str) -> World {
    let world = world().await;
    let mut account = account_json(id, kind, &json!(1000));
    account["name"] = json!(name);
    account["subtype"] = json!(subtype);
    world
        .script
        .always(GET_ACCOUNTS, StatusCode::OK, accounts_answer(&[account]));
    world.script.page(page(&[], &[], &[], "done", false));
    world.sync().await;
    world
}

fn no_choices() -> NewLinkedAccountInput {
    NewLinkedAccountInput {
        name: None,
        kind: None,
        liquidity: None,
        include_in_net_worth: None,
        include_in_forecast: None,
        belongs_to: None,
        beneficiary: None,
    }
}

#[tokio::test]
async fn a_plaid_529_hint_binds_with_no_kind_and_stays_out() {
    let world = synced("acct-plan", "investment", "529", "Family Fund").await;
    let reported = world.provider_account("acct-plan").await;
    assert_eq!(reported.kind_hint, Some(AccountKind::Education));

    let linked = world
        .server
        .sync
        .create_account_and_bind(&world.alice, reported.id, no_choices())
        .await
        .unwrap();

    assert_eq!(linked.account.kind, AccountKind::Education);
    assert_eq!(linked.account.belongs_to, BelongsTo::SomeoneElse);
    assert!(!linked.account.include_in_net_worth);
    assert!(!linked.account.include_in_forecast);
}

/// The same through the API: an empty body is a 201, and the answer says where the money went.
#[tokio::test]
async fn an_empty_bind_of_a_plaid_529_answers_where_it_stands() {
    let world = synced("acct-plan", "investment", "529", "Family Fund").await;
    let reported = world.provider_account("acct-plan").await;
    let (_, token) = world
        .server
        .agent_tokens
        .create(
            &world.alice,
            TokenName::parse("claude").unwrap(),
            Scope::Write,
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    let request = Request::builder()
        .method(Method::POST)
        .uri(format!(
            "/api/v1/provider-accounts/{}/create-account",
            reported.id
        ))
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", token.expose_secret()),
        )
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{}"))
        .unwrap();

    let response = router(world.server.app_state())
        .oneshot(request)
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let linked: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(linked["account"]["kind"], json!("education"));
    assert_eq!(linked["account"]["belongs_to"], json!("someone_else"));
    assert_eq!(linked["account"]["include_in_net_worth"], json!(false));
    assert_eq!(linked["account"]["include_in_forecast"], json!(false));
    assert_eq!(linked["provider_account"]["kind_hint"], json!("education"));
    assert_eq!(linked["provider_account"]["suggested_kind"], json!(null));
    assert_eq!(
        linked["provider_account"]["account"],
        linked["account"]["id"]
    );
}

/// The subtype decides: a name saying otherwise changes nothing, even with no kind sent.
#[tokio::test]
async fn a_plaid_hint_is_what_a_bind_falls_back_on_whatever_the_name_says() {
    let world = synced("acct-main", "depository", "checking", "My 529 Plan").await;
    let reported = world.provider_account("acct-main").await;

    let linked = world
        .server
        .sync
        .create_account_and_bind(&world.alice, reported.id, no_choices())
        .await
        .unwrap();

    assert_eq!(linked.account.kind, AccountKind::Checking);
    assert_eq!(linked.account.belongs_to, BelongsTo::Household);
}
