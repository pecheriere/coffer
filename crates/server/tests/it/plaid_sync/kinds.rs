//! What Plaid says an account is, through a real sync: its subtype is a provider's hint, which a
//! bind may fall back on, and which a guess from the name never overrides.

use super::{GET_ACCOUNTS, account_json, accounts_answer, page, world};
use axum::http::StatusCode;
use coffer_domain::accounts::{AccountKind, BelongsTo};
use coffer_server::sync::connections::NewLinkedAccountInput;
use serde_json::json;

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
    let world = world().await;
    let mut account = account_json("acct-plan", "investment", &json!(1000));
    account["name"] = json!("Family Fund");
    account["subtype"] = json!("529");
    world
        .script
        .always(GET_ACCOUNTS, StatusCode::OK, accounts_answer(&[account]));
    world.script.page(page(&[], &[], &[], "done", false));
    world.sync().await;
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

/// The subtype decides: a name saying otherwise changes nothing, even with no kind sent.
#[tokio::test]
async fn a_plaid_hint_is_what_a_bind_falls_back_on_whatever_the_name_says() {
    let world = world().await;
    let mut account = account_json("acct-main", "depository", &json!(10));
    account["name"] = json!("My 529 Plan");
    account["subtype"] = json!("checking");
    world
        .script
        .always(GET_ACCOUNTS, StatusCode::OK, accounts_answer(&[account]));
    world.script.page(page(&[], &[], &[], "done", false));
    world.sync().await;
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
