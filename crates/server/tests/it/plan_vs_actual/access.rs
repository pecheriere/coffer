//! Who gets the report (spec §8.9 R2): `all` at `transactions` or nothing, and the same
//! nothing whatever exists, so no refusal tells a token about what it can't see.

use axum::http::{Method, StatusCode};
use coffer_domain::identity::Scope;
use coffer_domain::ids::AccountId;
use coffer_domain::policy::AccountLevel::{Balances, Transactions, Write};
use coffer_domain::policy::TokenGrant;
use coffer_server::secrets::SecretToken;
use serde_json::Value;

use super::{REPORT, World, all, goal, monthly, on, usd, world};

const REFUSAL: &str = "this action needs transactions access to every account";
const OCTOBER: &str = "/api/v1/reports/plan-vs-actual?from=2026-10&months=1&average_months=1";

impl World {
    async fn assert_refused(&self, token: &SecretToken) {
        let (status, text) = self.send(Method::GET, OCTOBER, token, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let body: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(body["type"], "urn:coffer:problem:forbidden");
        assert_eq!(body["detail"], REFUSAL);
    }

    /// Two holdings, a plan and spending on the second, so a report would have something
    /// to say about it.
    async fn populated(&self) -> (AccountId, AccountId) {
        let visible = self.holding("Visible", "checking").await;
        let hidden = self.holding("Hidden", "checking").await;
        let shared = self.expense("Shared").await;
        self.item(&monthly(
            "Secretive",
            "outflow",
            4_242,
            shared,
            "2026-01-01",
        ))
        .await;
        let roof = self.item(&goal("Hideaway", 1_000, shared)).await;
        self.posted(hidden, "2026-10-02", -3_333, shared).await;
        self.posted_linked(hidden, "2026-10-03", -5_555, shared, &roof)
            .await;
        self.manual(hidden, "2026-10-04", -6_666).await;
        (visible, hidden)
    }
}

#[tokio::test]
async fn read_token_without_grants_gets_the_report() {
    let world = world().await;
    let checking = world.checking().await;
    let food = world.expense("Food").await;
    world.posted(checking, "2026-10-06", -500, food).await;
    let token = world.token(Scope::Read, Vec::new()).await;

    let (status, text) = world.send(Method::GET, OCTOBER, &token, None).await;

    assert_eq!(status, StatusCode::OK);
    let report: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(report["rows"][0]["actual"], usd(500));
}

#[tokio::test]
async fn all_at_transactions_gets_the_report() {
    let world = world().await;
    world.populated().await;
    let token = world.token(Scope::Read, vec![all(Transactions)]).await;

    let (status, text) = world.send(Method::GET, OCTOBER, &token, None).await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        text.contains("3333"),
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
async fn hidden_accounts_mean_refusal() {
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

/// One household holds nothing else, and the other a hidden holding with a plan and
/// spending; a token with the same grants can't tell them apart.
#[tokio::test]
async fn refusal_is_the_same_with_and_without_hidden_accounts() {
    let empty = world().await;
    let populated = world().await;
    let only = empty.holding("Visible", "checking").await;
    let (visible, _) = populated.populated().await;
    let for_empty = empty.token(Scope::Read, vec![on(only, Transactions)]).await;
    let for_populated = populated
        .token(Scope::Read, vec![on(visible, Transactions)])
        .await;

    let bare = empty.send(Method::GET, OCTOBER, &for_empty, None).await;
    let full = populated
        .send(Method::GET, OCTOBER, &for_populated, None)
        .await;

    assert_eq!(bare.0, StatusCode::FORBIDDEN);
    assert!(bare == full, "bodies byte-identical");
    for hidden in [
        "4242",
        "3333",
        "5555",
        "6666",
        "Hidden",
        "Secretive",
        "Hideaway",
    ] {
        assert!(!full.1.contains(hidden), "the refusal names nothing");
    }
}

/// Access comes before the range checks, so they can't be probed with a short grant.
#[tokio::test]
async fn refusal_comes_before_range_checks() {
    let world = world().await;
    let (visible, _) = world.populated().await;
    let token = world
        .token(Scope::Read, vec![on(visible, Transactions)])
        .await;

    let in_range = world.send(Method::GET, OCTOBER, &token, None).await;
    assert_eq!(in_range.0, StatusCode::FORBIDDEN);
    for query in [
        "from=2026-10&months=37",
        "from=2201-01",
        "from=2026-10&average_months=13",
    ] {
        let answer = world
            .send(Method::GET, &format!("{REPORT}?{query}"), &token, None)
            .await;
        assert!(answer == in_range, "the same refusal whatever is asked");
    }
}
