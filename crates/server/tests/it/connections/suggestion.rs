//! A kind suggested from a provider's name, in the API: a field of the view while it could be
//! used, and never a kind (spec §8.11 B12).

use super::{reported, world};
use axum::http::{Method, StatusCode};
use coffer_domain::accounts::AccountKind;
use coffer_domain::identity::Scope;
use coffer_server::sync::provider::ProviderAccountData;
use serde_json::{Value, json};

fn named(provider_account_id: &str, name: &str, hint: Option<AccountKind>) -> ProviderAccountData {
    ProviderAccountData {
        name: name.into(),
        ..reported(provider_account_id, "USD", hint, Vec::new())
    }
}

fn item<'page>(page: &'page Value, id: &str) -> &'page Value {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == json!(id))
        .unwrap()
}

#[tokio::test]
async fn no_kind_is_refused_even_with_a_suggestion() {
    let world = world().await;
    world
        .sync_once(vec![named("acct-1", "My 529 Plan", None)])
        .await;
    let held = world.provider_account("acct-1").await;
    let writer = world.token(Scope::Write).await;
    let uri = format!("/api/v1/provider-accounts/{}/create-account", held.id);
    let (status, page) = world
        .call(Method::GET, "/api/v1/provider-accounts", &writer, None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        page["items"][0]["suggested_kind"],
        json!("education"),
        "a suggestion is there to be applied, if this ever applied it"
    );

    for body in [json!({}), json!({"kind": null}), json!({"name": "Plan"})] {
        let (status, problem) = world.call(Method::POST, &uri, &writer, Some(body)).await;

        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(problem["code"], json!("invalid"));
    }

    assert!(
        world
            .server
            .ledger
            .list_accounts(&world.alice, true)
            .await
            .unwrap()
            .is_empty(),
        "no account"
    );
    assert_eq!(world.provider_account("acct-1").await.account, None);
    assert_eq!(world.audit_actions("account.create").await.len(), 0);
    assert_eq!(world.audit_actions("provider_account.bind").await.len(), 0);
}

#[tokio::test]
async fn suggested_kind_is_in_the_view_only_while_unbound() {
    let world = world().await;
    world
        .sync_once(vec![
            named("guessed", "Roth IRA", None),
            named("hinted", "Roth IRA", Some(AccountKind::Brokerage)),
            named("unclear", "Family Fund", None),
        ])
        .await;
    let guessed = world.provider_account("guessed").await.id.to_string();
    let hinted = world.provider_account("hinted").await.id.to_string();
    let unclear = world.provider_account("unclear").await.id.to_string();
    let writer = world.token(Scope::Write).await;

    let (_, page) = world
        .call(Method::GET, "/api/v1/provider-accounts", &writer, None)
        .await;
    assert_eq!(item(&page, &guessed)["suggested_kind"], json!("retirement"));
    assert_eq!(item(&page, &guessed)["kind_hint"], json!(null));
    assert_eq!(
        item(&page, &hinted)["suggested_kind"],
        json!(null),
        "the provider said: nothing to guess"
    );
    assert_eq!(item(&page, &hinted)["kind_hint"], json!("brokerage"));
    let unclear_item = item(&page, &unclear).as_object().unwrap();
    assert_eq!(
        unclear_item.get("suggested_kind"),
        Some(&json!(null)),
        "the field is always there"
    );

    let (status, linked) = world
        .call(
            Method::POST,
            &format!("/api/v1/provider-accounts/{guessed}/create-account"),
            &writer,
            Some(json!({"kind": "brokerage"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        linked["account"]["kind"],
        json!("brokerage"),
        "the kind sent, not the guess"
    );
    assert_eq!(linked["provider_account"]["suggested_kind"], json!(null));

    let (_, page) = world
        .call(Method::GET, "/api/v1/provider-accounts", &writer, None)
        .await;
    assert_eq!(item(&page, &guessed)["suggested_kind"], json!(null));
    assert_eq!(
        item(&page, &unclear)["suggested_kind"],
        json!(null),
        "an unbound account with nothing to suggest stays as it was"
    );
}
