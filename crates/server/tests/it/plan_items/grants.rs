//! Who may see and change a planned item (spec §8.9 R3). Below `transactions`
//! on the hinted account an item answers exactly like a missing one, without
//! a hint it needs `transactions` on some account, and every write needs
//! `all` at `write`.

use axum::http::{Method, StatusCode};
use coffer_domain::identity::Scope;
use coffer_domain::ids::PlannedItemId;
use coffer_domain::policy::AccountLevel::{Balances, Transactions, Write};
use coffer_domain::policy::TokenGrant;
use coffer_server::secrets::SecretToken;
use coffer_store::{IdempotencyOwner, Reservation};
use serde_json::{Value, json};

use super::{ITEMS, World, all, item_uri, on, recurring, with, world};

const MISSING_ITEM: StatusCode = StatusCode::NOT_FOUND;

impl World {
    async fn hinted(&self, account: coffer_domain::ids::AccountId) -> String {
        self.create(&with(
            recurring(self.expense),
            "account_hint",
            json!(account),
        ))
        .await
    }

    async fn unhinted(&self) -> String {
        self.create(&recurring(self.expense)).await
    }

    /// `GET` of a random id: what a missing item answers.
    async fn missing(&self, token: &SecretToken) -> (StatusCode, String) {
        let uri = item_uri(PlannedItemId::new());
        self.send(Method::GET, &uri, token, None, None).await
    }

    /// Asserts that `id` is absent from `token`'s lists, including inactive
    /// items, and that its `GET` is byte-identical to a missing item's.
    async fn assert_absent(&self, token: &SecretToken, id: &str) {
        for uri in [ITEMS.to_owned(), format!("{ITEMS}?include_inactive=true")] {
            assert!(
                !self
                    .listed(&uri, token)
                    .await
                    .iter()
                    .any(|listed| listed == id),
                "a hidden item is left out of lists"
            );
        }
        let answer = self
            .send(Method::GET, &item_uri(id), token, None, None)
            .await;
        let missing = self.missing(token).await;
        assert_eq!(missing.0, MISSING_ITEM);
        assert!(
            answer == missing,
            "a hidden item answers like a missing one"
        );
    }

    async fn assert_visible(&self, token: &SecretToken, id: &str) {
        assert!(
            self.listed(ITEMS, token)
                .await
                .iter()
                .any(|listed| listed == id),
            "a visible item is listed"
        );
        let (status, _) = self
            .send(Method::GET, &item_uri(id), token, None, None)
            .await;
        assert_eq!(status, StatusCode::OK);
    }
}

#[tokio::test]
async fn read_token_with_an_account_grant_lists_unhinted_and_visible_hints() {
    let world = world().await;
    let (visible, hidden) = (
        world.account("Visible").await,
        world.account("Hidden").await,
    );
    let unhinted = world.unhinted().await;
    let on_visible = world.hinted(visible).await;
    let on_hidden = world.hinted(hidden).await;
    let inactive_on_hidden = world
        .create(&with(
            with(recurring(world.expense), "account_hint", json!(hidden)),
            "active",
            json!(false),
        ))
        .await;
    let (_, token) = world
        .token(Scope::Read, vec![on(visible, Transactions)])
        .await;

    assert!(
        world.listed(ITEMS, &token).await == [unhinted.clone(), on_visible.clone()],
        "the unhinted item and the visible hint, nothing else"
    );
    world.assert_visible(&token, &unhinted).await;
    world.assert_visible(&token, &on_visible).await;
    world.assert_absent(&token, &on_hidden).await;
    world.assert_absent(&token, &inactive_on_hidden).await;
}

#[tokio::test]
async fn hidden_hint_answers_like_a_missing_item() {
    let world = world().await;
    let (visible, hidden) = (
        world.account("Visible").await,
        world.account("Hidden").await,
    );
    let id = world.hinted(hidden).await;
    let (_, token) = world.token(Scope::Write, vec![on(visible, Write)]).await;

    let answer = world
        .send(Method::GET, &item_uri(&id), &token, None, None)
        .await;
    let missing = world.missing(&token).await;

    assert_eq!(answer.0, MISSING_ITEM);
    assert!(answer == missing, "byte-identical to a random id's");
    assert!(!answer.1.contains(&id), "the body names nothing");
    assert!(
        !answer.1.contains(&hidden.to_string()),
        "the body names nothing"
    );
}

#[tokio::test]
async fn balances_level_hint_answers_like_a_missing_item() {
    let world = world().await;
    let (hinted, other) = (world.account("Hinted").await, world.account("Other").await);
    let id = world.hinted(hinted).await;
    let cases: [Vec<TokenGrant>; 3] = [
        vec![on(hinted, Balances)],
        // Transactions elsewhere doesn't lift the hinted account's own level.
        vec![on(hinted, Balances), on(other, Transactions)],
        vec![all(Balances), on(other, Transactions)],
    ];
    for grants in cases {
        let (_, token) = world.token(Scope::Read, grants).await;
        world.assert_absent(&token, &id).await;
    }
    let (_, through_all) = world.token(Scope::Read, vec![all(Balances)]).await;
    world.assert_absent(&through_all, &id).await;
}

#[tokio::test]
async fn balances_only_token_sees_no_items() {
    let world = world().await;
    let account = world.account("Checking").await;
    let unhinted = world.unhinted().await;
    let hinted = world.hinted(account).await;
    for grants in [vec![on(account, Balances)], vec![all(Balances)]] {
        let (_, token) = world.token(Scope::Read, grants).await;

        for uri in [ITEMS.to_owned(), format!("{ITEMS}?include_inactive=true")] {
            let (status, text) = world.send(Method::GET, &uri, &token, None, None).await;
            assert_eq!(status, StatusCode::OK, "an empty list, never a refusal");
            assert_eq!(text, r#"{"items":[]}"#);
        }
        world.assert_absent(&token, &unhinted).await;
        world.assert_absent(&token, &hinted).await;
    }
}

#[tokio::test]
async fn one_account_at_transactions_sees_unhinted_items() {
    let world = world().await;
    let account = world.account("Checking").await;
    let unhinted = world.unhinted().await;
    for grant in [
        on(account, Transactions),
        all(Transactions),
        on(account, Write),
    ] {
        let scope = if grant.level == Write {
            Scope::Write
        } else {
            Scope::Read
        };
        let (_, token) = world.token(scope, vec![grant]).await;
        world.assert_visible(&token, &unhinted).await;
    }
}

#[tokio::test]
async fn uniform_tokens_see_every_item() {
    let world = world().await;
    let account = world.account("Checking").await;
    let unhinted = world.unhinted().await;
    let hinted = world.hinted(account).await;
    for scope in [Scope::Read, Scope::Write] {
        let (_, token) = world.token(scope, Vec::new()).await;
        world.assert_visible(&token, &unhinted).await;
        world.assert_visible(&token, &hinted).await;
    }
}

/// Every write `token` could try on item `id`, each with its own body so an
/// unexpected reservation would show.
async fn refused_writes(world: &World, token: &SecretToken, id: &str) -> [(StatusCode, String); 3] {
    let create = recurring(world.expense);
    let uri = item_uri(id);
    [
        world
            .send(Method::POST, ITEMS, token, Some(&create), Some("write-key"))
            .await,
        world
            .send(
                Method::PATCH,
                &uri,
                token,
                Some(&json!({"name": "Mine"})),
                None,
            )
            .await,
        world.send(Method::DELETE, &uri, token, None, None).await,
    ]
}

#[tokio::test]
async fn restricted_token_cannot_write() {
    let world = world().await;
    let account = world.account("Checking").await;
    let unhinted = world.unhinted().await;
    let on_own_account = world.hinted(account).await;
    let cases: [Vec<TokenGrant>; 3] = [
        vec![on(account, Write)],
        vec![all(Transactions), on(account, Write)],
        vec![all(Balances)],
    ];
    for grants in cases {
        let (token_id, token) = world.token(Scope::Write, grants).await;
        let audited = world.audit().await.len();
        let items = world
            .listed(&format!("{ITEMS}?include_inactive=true"), &world.admin)
            .await;

        // The same refusal for an item it may read, for one on its own
        // account and for one that doesn't exist.
        let random = PlannedItemId::new().to_string();
        for id in [&unhinted, &on_own_account, &random] {
            for (status, text) in refused_writes(&world, &token, id).await {
                assert_eq!(status, StatusCode::FORBIDDEN);
                let body: Value = serde_json::from_str(&text).unwrap();
                assert_eq!(
                    body["detail"],
                    json!("this action needs write access to every account")
                );
                assert_eq!(body["type"], json!("urn:coffer:problem:forbidden"));
            }
        }

        assert_eq!(world.audit().await.len(), audited, "nothing was written");
        assert!(
            world
                .listed(&format!("{ITEMS}?include_inactive=true"), &world.admin)
                .await
                == items,
            "nothing changed"
        );

        // The key was never reserved: the store still hands it out as fresh.
        let mut tx = world.server.store.begin().await.unwrap();
        let reservation = tx
            .reserve_idempotency(
                IdempotencyOwner {
                    member: world.alice.member,
                    agent: Some(token_id),
                },
                "write-key",
                [7; 32],
                coffer_domain::time::Clock::now(world.server.clock.as_ref()),
            )
            .await
            .unwrap();
        assert!(
            matches!(reservation, Reservation::Fresh),
            "a refused create reserves no idempotency key"
        );
    }
}

#[tokio::test]
async fn all_at_write_may_write() {
    let world = world().await;
    let account = world.account("Checking").await;
    let (_, token) = world.token(Scope::Write, vec![all(Write)]).await;

    let id = world
        .create_with(
            &token,
            &with(recurring(world.expense), "account_hint", json!(account)),
        )
        .await;
    let uri = item_uri(&id);
    let patch = json!({"name": "Mine"});
    let (status, _) = world.call(Method::PATCH, &uri, &token, Some(&patch)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = world.call(Method::DELETE, &uri, &token, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn read_token_cannot_write() {
    let world = world().await;
    let id = world.unhinted().await;
    let random = PlannedItemId::new().to_string();
    for grants in [Vec::new(), vec![all(Transactions)]] {
        let (_, token) = world.token(Scope::Read, grants).await;
        for target in [&id, &random] {
            for (status, text) in refused_writes(&world, &token, target).await {
                assert_eq!(status, StatusCode::FORBIDDEN);
                let body: Value = serde_json::from_str(&text).unwrap();
                assert_eq!(body["type"], json!("urn:coffer:problem:forbidden"));
                assert_eq!(body["detail"], json!("this action requires write rights"));
            }
        }
    }
    assert_eq!(world.audit().await.len(), 1, "only the create was audited");
}
