//! `GET /api/v1/free-money` through the real router (money redesign §4.3, §4.4, §10.1): the
//! household of the domain's worked example, seeded through the API, answers its numbers; a
//! token short of the forecast's access learns nothing, whatever exists.

use axum::http::{Method, StatusCode};
use coffer_domain::identity::{Actor, Role, Scope, TokenName, Via};
use coffer_domain::ids::AccountId;
use coffer_domain::policy::AccountLevel::{Balances, Transactions};
use coffer_server::secrets::SecretToken;
use serde_json::{Value, json};

use super::{World, all, goal_schedule, monthly, on, outflow, usd, with, world};

const FREE_MONEY: &str = "/api/v1/free-money";
const REFUSAL: &str = "this action needs transactions access to every account";

impl World {
    /// The household's day moved to 15 January 2027, with checking holding `minor_units` at the
    /// end of the 14th.
    async fn on_january_15(&self, minor_units: i64) -> AccountId {
        self.server
            .clock
            .set("2027-01-15T12:00:00Z".parse().unwrap());
        let checking = self
            .account_with("Checking", "checking", "USD", json!({}))
            .await;
        self.balance(checking, "2027-01-14", minor_units).await;
        checking
    }

    /// Free money as `token` sees it.
    async fn free_money_as(&self, token: &SecretToken) -> (StatusCode, String) {
        self.send(Method::GET, FREE_MONEY, token, None).await
    }

    /// Free money, which must answer 200.
    async fn free_money(&self) -> Value {
        let (status, text) = self.free_money_as(&self.admin).await;
        assert_eq!(status, StatusCode::OK);
        serde_json::from_str(&text).unwrap()
    }

    /// The domain's worked example: rent of 2,000 on the 20th, groceries of 600 (essentials)
    /// with 250 spent, restaurants of 300 (flexible) with 100 spent, and a goal of 730 over a
    /// year, 184 of whose 365 days are gone.
    async fn worked_example(&self, checking: AccountId) {
        let home = self.expense("Rent").await;
        let food = self.expense("Groceries").await;
        let out = self.expense("Restaurants").await;
        self.item(&outflow("Rent", 200_000, home, &monthly("2027-01-20")))
            .await;
        let groceries = with(
            outflow("Groceries", 60_000, food, &monthly("2027-01-01")),
            "spending",
            json!("budget"),
        );
        self.item(&with(groceries, "tier", json!("essentials")))
            .await;
        self.item(&with(
            outflow("Restaurants", 30_000, out, &monthly("2027-01-01")),
            "spending",
            json!("budget"),
        ))
        .await;
        let travel = self.expense("Travel").await;
        self.item(&outflow(
            "Trip",
            73_000,
            travel,
            &goal_schedule("2026-07-15", "2027-07-15"),
        ))
        .await;
        self.spend(checking, "2027-01-10", 25_000, food).await;
        self.spend(checking, "2027-01-10", 10_000, out).await;
    }
}

#[tokio::test]
async fn free_money_answers_the_formula() {
    let world = world().await;
    let checking = world.on_january_15(1_000_000).await;
    world.worked_example(checking).await;
    world.emergency_fund(300_000).await;

    let answer = world.free_money().await;

    assert_eq!(answer["as_of"], "2027-01-15");
    assert_eq!(answer["must_pay_through"], "2027-02-14");
    assert_eq!(answer["status"], "firm", "{answer}");
    assert_eq!(answer["blockers"], json!([]));
    assert_eq!(answer["cautions"], json!([]));
    assert_eq!(
        answer["currencies"],
        json!([{
            "currency": "USD",
            "cash_available": usd(1_000_000),
            "still_need": {
                "must_pay_this_month": usd(200_000),
                "must_pay_coming": usd(0),
                "essentials": usd(35_000),
                "flexible": usd(20_000),
                "total": usd(255_000),
            },
            "set_aside": usd(36_800),
            "emergency_fund": {"kind": "fixed", "minor_units": 300_000},
            "emergency_fund_target": usd(300_000),
            "emergency_fund_held": usd(300_000),
            "free_money": usd(408_200),
            "shortfall": usd(0),
        }])
    );
}

/// Below the forecast's access, the same refusal before anything is read: with or without a
/// plan, and whether the token is short at every account or sees only some of them.
#[tokio::test]
async fn a_restricted_token_is_refused() {
    let world = world().await;
    let checking = world.on_january_15(1_000_000).await;
    let tokens = [
        world.token(Scope::Read, vec![all(Balances)]).await,
        world
            .token(Scope::Read, vec![on(checking, Transactions)])
            .await,
    ];
    let mut before = Vec::new();
    for token in &tokens {
        before.push(world.free_money_as(token).await);
    }

    world.worked_example(checking).await;
    world.emergency_fund(300_000).await;

    for (token, earlier) in tokens.iter().zip(before) {
        let (status, text) = world.free_money_as(token).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!((status, text.clone()), earlier, "the same whatever exists");
        let body: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(body["type"], "urn:coffer:problem:forbidden");
        assert_eq!(body["detail"], REFUSAL);
    }
}

#[tokio::test]
async fn a_member_reads_it() {
    let world = world().await;
    world.on_january_15(1_000_000).await;
    world.emergency_fund(300_000).await;
    let invite = world
        .server
        .identity
        .create_invite(&world.alice, Role::Member, None)
        .await
        .unwrap();
    let bob = Actor {
        member: world
            .server
            .authenticator()
            .enroll(&invite, "bob", "Bob")
            .await,
        role: Role::Member,
        via: Via::Session,
    };
    let (_, token) = world
        .server
        .agent_tokens
        .create(
            &bob,
            TokenName::parse("bobs").unwrap(),
            Scope::Read,
            None,
            Vec::new(),
        )
        .await
        .unwrap();

    let (status, text) = world.free_money_as(&token).await;

    assert_eq!(status, StatusCode::OK);
    let answer: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(answer["currencies"][0]["free_money"], usd(700_000));
}

#[tokio::test]
async fn no_target_shows_the_caution() {
    let world = world().await;
    let checking = world.on_january_15(1_000_000).await;
    world.worked_example(checking).await;

    let answer = world.free_money().await;

    assert_eq!(answer["status"], "provisional");
    assert_eq!(
        answer["cautions"],
        json!([{"kind": "emergency_fund_not_set", "currency": "USD"}])
    );
    let row = &answer["currencies"][0];
    assert_eq!(row["emergency_fund"], Value::Null);
    assert_eq!(row["emergency_fund_target"], usd(0));
    assert_eq!(
        row["free_money"],
        usd(1_000_000 - 255_000 - 36_800),
        "cash, less what the plan still needs and the goal set aside"
    );
}

/// A counted account without a balance leaves its currency without a number, and says which.
#[tokio::test]
async fn a_missing_balance_leaves_no_number() {
    let world = world().await;
    world.on_january_15(1_000_000).await;
    world.emergency_fund(0).await;
    let empty = world
        .account_with("Savings", "savings", "USD", json!({}))
        .await;

    let answer = world.free_money().await;

    assert_eq!(answer["status"], "unavailable");
    assert_eq!(
        answer["blockers"],
        json!([{"kind": "balance_missing", "account": empty, "name": "Savings"}])
    );
    let row = &answer["currencies"][0];
    for field in ["free_money", "emergency_fund_held", "shortfall"] {
        assert_eq!(row[field], Value::Null, "{field}");
    }
}
