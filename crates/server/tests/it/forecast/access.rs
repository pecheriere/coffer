//! Who gets a forecast (spec §8.9 R2): `all` at `transactions` or nothing, and the
//! same nothing whatever exists, so no number, line, caution or error tells a
//! restricted token about an account or item it can't see.

use axum::http::{Method, StatusCode};
use coffer_domain::identity::Scope;
use coffer_domain::ids::AccountId;
use coffer_domain::policy::AccountLevel::{Balances, Transactions, Write};
use coffer_domain::policy::TokenGrant;
use coffer_server::secrets::SecretToken;
use serde_json::{Value, json};

use super::{FORECAST, WHAT_IF, World, all, monthly, on, one_off_on, outflow, usd, with, world};

const REFUSAL: &str = "this action needs transactions access to every account";

/// A what-if that names nothing.
fn plain_what_if() -> Value {
    json!({"items": [{
        "name": "Roof",
        "amount": usd(1),
        "direction": "outflow",
        "schedule": one_off_on("2026-12-20"),
    }]})
}

impl World {
    async fn on_both_routes(&self, token: &SecretToken) -> [(StatusCode, String); 2] {
        [
            self.send(Method::GET, FORECAST, token, None).await,
            self.send(Method::POST, WHAT_IF, token, Some(&plain_what_if()))
                .await,
        ]
    }

    async fn assert_refused(&self, token: &SecretToken) {
        for (status, text) in self.on_both_routes(token).await {
            assert_eq!(status, StatusCode::FORBIDDEN);
            let body: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(body["type"], "urn:coffer:problem:forbidden");
            assert_eq!(body["detail"], REFUSAL);
        }
    }

    /// Accounts, balances, an item hinted at each and a transaction, so that a
    /// forecast would have something to say about all of them.
    async fn populated(&self) -> (AccountId, AccountId) {
        let visible = self.funded("Visible", 1_000).await;
        let hidden = self.funded("Hidden", 7_777_777).await;
        let category = self.expense("Shared").await;
        for (name, account) in [("Seen", visible), ("Secretive", hidden)] {
            self.item(&with(
                outflow(name, 4_242, category, &monthly("2026-01-01")),
                "account_hint",
                json!(account),
            ))
            .await;
        }
        self.spend(hidden, "2026-11-02", 3_333, category).await;
        self.floor(0).await;
        (visible, hidden)
    }
}

#[tokio::test]
async fn read_token_without_grants_gets_a_forecast() {
    let world = world().await;
    world.funded("Checking", 1_000).await;
    world.floor(0).await;
    let token = world.token(Scope::Read, Vec::new()).await;

    let (status, text) = world.send(Method::GET, FORECAST, &token, None).await;

    assert_eq!(status, StatusCode::OK);
    let forecast: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(forecast["surplus"], usd(1_000));
}

/// A `POST`, but it persists nothing (R12).
#[tokio::test]
async fn read_token_may_run_what_if() {
    let world = world().await;
    world.funded("Checking", 1_000).await;
    world.floor(0).await;
    let token = world.token(Scope::Read, Vec::new()).await;

    let (status, text) = world
        .send(Method::POST, WHAT_IF, &token, Some(&plain_what_if()))
        .await;

    assert_eq!(status, StatusCode::OK);
    let forecast: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(forecast["surplus"], usd(999));
}

#[tokio::test]
async fn all_at_transactions_gets_a_forecast() {
    let world = world().await;
    let (_, hidden) = world.populated().await;
    let token = world.token(Scope::Read, vec![all(Transactions)]).await;

    let [(forecast, body), (what_if, _)] = world.on_both_routes(&token).await;

    assert_eq!(forecast, StatusCode::OK);
    assert_eq!(what_if, StatusCode::OK);
    assert!(
        body.contains(&hidden.to_string()),
        "the answer covers the whole household"
    );
}

#[tokio::test]
async fn all_at_balances_is_refused() {
    let world = world().await;
    world.populated().await;
    let token = world.token(Scope::Read, vec![all(Balances)]).await;
    world.assert_refused(&token).await;
}

#[tokio::test]
async fn account_grants_are_refused_on_both_routes() {
    let world = world().await;
    let (visible, hidden) = world.populated().await;
    let cases: [(Scope, Vec<TokenGrant>); 5] = [
        (Scope::Read, vec![on(visible, Transactions)]),
        (Scope::Write, vec![on(visible, Write)]),
        (
            Scope::Read,
            vec![on(visible, Transactions), on(hidden, Transactions)],
        ),
        (Scope::Read, vec![all(Balances), on(visible, Transactions)]),
        (Scope::Read, vec![on(visible, Balances)]),
    ];
    for (scope, grants) in cases {
        let token = world.token(scope, grants).await;
        world.assert_refused(&token).await;
    }
}

/// One household holds nothing else, and the other a hidden account, an item hinted
/// at it and spending on it; a token with the same grants can't tell them apart.
#[tokio::test]
async fn refusal_is_the_same_with_and_without_hidden_accounts() {
    let empty = world().await;
    let populated = world().await;
    let only = empty.funded("Visible", 1_000).await;
    let (visible, _) = populated.populated().await;

    let for_empty = empty.token(Scope::Read, vec![on(only, Transactions)]).await;
    let for_populated = populated
        .token(Scope::Read, vec![on(visible, Transactions)])
        .await;
    let (bare, full) = (
        empty.on_both_routes(&for_empty).await,
        populated.on_both_routes(&for_populated).await,
    );
    assert_eq!(bare[0].0, StatusCode::FORBIDDEN);
    assert!(bare == full, "bodies byte-identical on both routes");

    // No trace of what it can't see, or of what it can.
    for (_, text) in &full {
        for hidden in ["7777777", "4242", "3333", "Hidden", "Secretive", "Seen"] {
            assert!(!text.contains(hidden), "the refusal names nothing");
        }
    }
}

/// The hint of a what-if can't ask whether an account exists.
#[tokio::test]
async fn what_if_cannot_probe_a_hidden_account() {
    let world = world().await;
    let (visible, hidden) = world.populated().await;
    let token = world
        .token(Scope::Read, vec![on(visible, Transactions)])
        .await;
    let hinted = |account: AccountId| {
        let mut body = plain_what_if();
        body["items"][0]["account_hint"] = json!(account);
        body
    };

    let existing = world
        .send(Method::POST, WHAT_IF, &token, Some(&hinted(hidden)))
        .await;
    let missing = world
        .send(
            Method::POST,
            WHAT_IF,
            &token,
            Some(&hinted(AccountId::new())),
        )
        .await;
    let invalid_elsewhere = world
        .send(
            Method::POST,
            WHAT_IF,
            &token,
            Some(&json!({"horizon_months": 61})),
        )
        .await;

    assert_eq!(existing.0, StatusCode::FORBIDDEN);
    assert!(existing == missing, "one that exists answers the same");
    assert!(
        existing == invalid_elsewhere,
        "refused before anything is checked"
    );
}
