//! What the forecast says it doesn't know, through the router: accounts and feeds it
//! can't see, balances it can't trust, and a database it can't read.

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use coffer_domain::accounts::AccountKind;
use coffer_domain::fx::{FxSource, RateObservation};
use coffer_domain::ids::{AccountId, ConnectionId, ProviderAccountId};
use coffer_domain::money::Currency;
use coffer_domain::sync::SyncTrigger;
use coffer_domain::time::Clock as _;
use coffer_server::sync::apply::Coverage;
use coffer_server::sync::provider::{FetchResult, ProviderAccountData, ProviderKind};
use coffer_server::testkit::{FakeProvider, TestServer};
use coffer_store::{ConnectionStatus, ConnectionStatusChange, NewSyncRun};
use serde_json::{Value, json};

use super::{AS_OF, FORECAST, World, caution, goal_schedule, kinds, outflow, usd, world, world_on};

const LABEL: &str = "Family bank";
const PROVIDER_ERROR: &str = "the bank says account 4242 is frozen";

/// A world with a provider connection.
pub(crate) struct Fed {
    pub(crate) world: World,
    fake: Arc<FakeProvider>,
    connection: ConnectionId,
}

pub(crate) async fn fed() -> Fed {
    let (server, fake) = TestServer::with_fake_provider().await;
    let world = world_on(server).await;
    let connection = world
        .server
        .seed_connection(ProviderKind::SimpleFin, LABEL, world.alice.member, "opaque")
        .await;
    world.emergency_fund(0).await;
    world.funded("Checking", 1_000).await;
    Fed {
        world,
        fake,
        connection,
    }
}

pub(crate) fn reported(currency: &str) -> ProviderAccountData {
    ProviderAccountData {
        provider_account_id: "one".into(),
        name: "Everyday savings".into(),
        institution: Some("Example Bank".into()),
        currency: currency.into(),
        kind_hint: Some(AccountKind::Savings),
        balance: None,
        transactions: Vec::new(),
        removed: Vec::new(),
        coverage: Coverage::Delta,
        error: None,
        unusable: Vec::new(),
    }
}

impl Fed {
    pub(crate) async fn sync(&self, account: ProviderAccountData) {
        self.fake.push(Ok(FetchResult {
            accounts: vec![account],
            connection_errors: Vec::new(),
            skipped: 0,
            cursor: None,
        }));
        let mut tx = self.world.server.store.begin().await.unwrap();
        let run = tx
            .insert_sync_run(NewSyncRun {
                connection: self.connection,
                trigger: SyncTrigger::Manual,
                requested_by: Some((self.world.alice.member, None)),
                queued_at: self.world.server.clock.now(),
            })
            .await
            .unwrap();
        tx.commit().await.unwrap();
        self.world.server.sync.run(run.id).await.unwrap();
    }

    /// Reports a provider account in USD and binds it to `account`.
    pub(super) async fn bound(&self, account: AccountId) -> ProviderAccountId {
        self.sync(reported("USD")).await;
        let id = self.provider_account().await;
        self.world
            .server
            .sync
            .bind(&self.world.alice, id, account)
            .await
            .unwrap();
        id
    }

    async fn provider_account(&self) -> ProviderAccountId {
        let found = self
            .world
            .server
            .store
            .list_provider_accounts(Some(self.connection))
            .await;
        let Ok(found) = found else {
            panic!("the store must answer");
        };
        found.first().unwrap().id
    }

    async fn set_status(&self, status: ConnectionStatus) {
        let mut tx = self.world.server.store.begin().await.unwrap();
        tx.update_connection_status(
            self.connection,
            ConnectionStatusChange {
                status,
                last_error: Some(PROVIDER_ERROR.into()),
                attempted_at: None,
                succeeded_at: None,
                at: self.world.server.clock.now(),
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }

    /// The first counted account, its only one but for a test's own.
    pub(super) async fn checking(&self) -> AccountId {
        let forecast = self.world.forecast().await;
        let Ok(id) = serde_json::from_value(forecast["accounts"][0]["account"].clone()) else {
            panic!("the forecast lists an id");
        };
        id
    }
}

#[tokio::test]
async fn unbound_provider_account_is_a_caution() {
    let fed = fed().await;
    fed.sync(reported("USD")).await;

    let forecast = fed.world.forecast().await;

    assert_eq!(forecast["status"], "provisional");
    assert_eq!(forecast["surplus"], usd(1_000), "it keeps the number");
    let unbound = caution(&forecast, "unbound_provider_account");
    assert!(
        unbound["provider_account"] == json!(fed.provider_account().await),
        "names it"
    );
    assert_eq!(unbound["connection"], json!(fed.connection));
    assert_eq!(unbound["name"], "Everyday savings");
    assert_eq!(unbound["institution"], "Example Bank");
    assert_eq!(unbound["currency_raw"], "USD");
    assert_eq!(unbound["currency_supported"], true);
}

#[tokio::test]
async fn a_dismissed_unbound_account_raises_no_caution_until_restored() {
    let fed = fed().await;
    fed.sync(reported("USD")).await;
    let id = fed.provider_account().await;
    let sync = &fed.world.server.sync;
    let alice = &fed.world.alice;

    sync.dismiss_provider_account(alice, id).await.unwrap();
    let dismissed = fed.world.forecast().await;
    assert!(
        !kinds(&dismissed, "cautions").contains(&"unbound_provider_account".to_owned()),
        "dismissed: no caution"
    );
    assert_eq!(dismissed["surplus"], usd(1_000), "the number is the same");

    sync.restore_provider_account(alice, id).await.unwrap();
    let restored = fed.world.forecast().await;
    assert_eq!(
        kinds(&restored, "cautions"),
        ["unbound_provider_account"],
        "restored: the caution is back"
    );
}

#[tokio::test]
async fn unsupported_currency_on_an_unbound_account_says_so() {
    let fed = fed().await;
    fed.sync(reported("ZZZ")).await;

    let forecast = fed.world.forecast().await;

    let unbound = caution(&forecast, "unbound_provider_account");
    assert_eq!(unbound["currency_raw"], "ZZZ");
    assert_eq!(unbound["currency_supported"], false);
}

#[tokio::test]
async fn bound_account_with_a_provider_error_is_a_caution() {
    let fed = fed().await;
    let checking = fed.checking().await;
    let provider_account = fed.bound(checking).await;
    fed.sync(ProviderAccountData {
        error: Some(PROVIDER_ERROR.into()),
        ..reported("USD")
    })
    .await;

    let (status, text) = fed
        .world
        .send(Method::GET, FORECAST, &fed.world.admin, None)
        .await;

    assert_eq!(status, StatusCode::OK);
    assert!(!text.contains("4242"), "no provider error text in the body");
    let forecast: Value = serde_json::from_str(&text).unwrap();
    let problem = caution(&forecast, "account_feed_problem");
    assert!(problem["account"] == json!(checking), "names it");
    assert!(
        problem["provider_account"] == json!(provider_account),
        "names its feed"
    );
    assert_eq!(problem["reason"], "provider_error");
}

#[tokio::test]
async fn unbound_account_with_a_provider_error_shows_no_error_text() {
    let fed = fed().await;
    fed.sync(ProviderAccountData {
        error: Some(PROVIDER_ERROR.into()),
        ..reported("USD")
    })
    .await;

    let (status, text) = fed
        .world
        .send(Method::GET, FORECAST, &fed.world.admin, None)
        .await;

    assert_eq!(status, StatusCode::OK);
    assert!(text.contains("unbound_provider_account"), "still reported");
    assert!(!text.contains("4242"), "no provider error text in the body");
}

#[tokio::test]
async fn bound_account_in_another_currency_is_a_caution() {
    for (currency, reason) in [("EUR", "currency_differs"), ("ZZZ", "currency_unsupported")] {
        let fed = fed().await;
        let checking = fed.checking().await;
        fed.bound(checking).await;
        fed.sync(reported(currency)).await;

        let forecast = fed.world.forecast().await;

        // The sync refused the account too, which is a provider error of its own.
        let reasons: Vec<&Value> = forecast["cautions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|caution| caution["kind"] == "account_feed_problem")
            .map(|caution| &caution["reason"])
            .collect();
        assert!(reasons.contains(&&json!(reason)), "the reason is reported");
    }
}

#[tokio::test]
async fn connection_needing_reauth_is_a_caution() {
    for (status, reported_as) in [
        (ConnectionStatus::ReauthRequired, "reauth_required"),
        (ConnectionStatus::Error, "error"),
    ] {
        let fed = fed().await;
        let checking = fed.checking().await;
        fed.bound(checking).await;
        fed.set_status(status).await;

        let (_, text) = fed
            .world
            .send(Method::GET, FORECAST, &fed.world.admin, None)
            .await;

        assert!(!text.contains("4242"), "no provider error text in the body");
        let forecast: Value = serde_json::from_str(&text).unwrap();
        let attention = caution(&forecast, "connection_needs_attention");
        assert_eq!(attention["connection"], json!(fed.connection));
        assert_eq!(attention["label"], LABEL);
        assert_eq!(attention["status"], reported_as);
    }
}

/// A connection that feeds only money outside the household is failing on the connections
/// page, and cannot change the forecast's number, so the forecast does not say so.
#[tokio::test]
async fn failing_connection_serving_only_a_held_account_is_not_a_caution() {
    let fed = fed().await;
    let held = fed
        .world
        .account_with(
            "For a child",
            "savings",
            "USD",
            json!({"belongs_to": "someone_else"}),
        )
        .await;
    fed.bound(held).await;
    fed.set_status(ConnectionStatus::Error).await;

    let forecast = fed.world.forecast().await;

    assert_eq!(forecast["status"], "firm");
    assert_eq!(forecast["cautions"], json!([]));
}

/// A connection whose only provider accounts a member dismissed has nothing that could change
/// the number: a dismissed account is not an unbound one here, though the connection has accounts.
#[tokio::test]
async fn failing_connection_with_only_dismissed_accounts_is_not_a_caution() {
    let fed = fed().await;
    fed.sync(reported("USD")).await;
    let id = fed.provider_account().await;
    fed.set_status(ConnectionStatus::Error).await;

    let before = fed.world.forecast().await;
    assert_eq!(
        kinds(&before, "cautions"),
        ["unbound_provider_account", "connection_needs_attention"],
        "undismissed, the unbound account makes the failure matter"
    );

    fed.world
        .server
        .sync
        .dismiss_provider_account(&fed.world.alice, id)
        .await
        .unwrap();
    let after = fed.world.forecast().await;

    assert_eq!(after["status"], "firm");
    assert_eq!(after["cautions"], json!([]));
}

/// Spending linked to a goal lowers its need on a held account too, so a feed that stalls
/// there can leave the need overstated: the forecast says so.
#[tokio::test]
async fn failing_connection_serving_a_held_account_paying_a_goal_is_a_caution() {
    let fed = fed().await;
    let held = fed
        .world
        .account_with(
            "For a child",
            "savings",
            "USD",
            json!({"belongs_to": "someone_else"}),
        )
        .await;
    let tuition = fed.world.expense("Tuition").await;
    let goal = fed
        .world
        .item(&outflow(
            "First year",
            60_000,
            tuition,
            &goal_schedule("2026-06-01", "2027-01-31"),
        ))
        .await;
    fed.world
        .spend_linked(held, AS_OF, 10_000, tuition, &goal)
        .await;
    fed.bound(held).await;
    fed.set_status(ConnectionStatus::Error).await;

    let forecast = fed.world.forecast().await;

    assert_eq!(forecast["status"], "provisional");
    caution(&forecast, "connection_needs_attention");
}

#[tokio::test]
async fn removed_connection_is_not_reported() {
    let fed = fed().await;
    let checking = fed.checking().await;
    let second = fed.world.funded("Second", 0).await;
    fed.bound(second).await;
    fed.sync(ProviderAccountData {
        provider_account_id: "two".into(),
        ..reported("USD")
    })
    .await;
    fed.set_status(ConnectionStatus::Removed).await;

    let forecast = fed.world.forecast().await;

    assert_eq!(forecast["status"], "firm", "nothing of it is reported");
    assert_eq!(forecast["cautions"], json!([]));
    assert!(
        forecast["accounts"][0]["account"] == json!(checking),
        "lines are in id order"
    );
}

/// A connection in error whose provider account was bound and failing: removing it
/// silences both.
#[tokio::test]
async fn removed_connection_silences_a_bound_provider_error() {
    let fed = fed().await;
    let checking = fed.checking().await;
    fed.bound(checking).await;
    fed.sync(ProviderAccountData {
        error: Some(PROVIDER_ERROR.into()),
        ..reported("USD")
    })
    .await;
    fed.set_status(ConnectionStatus::Removed).await;

    let forecast = fed.world.forecast().await;

    assert_eq!(forecast["cautions"], json!([]));
}

/// The default is 7 days, and the balance is 8 days old at `AS_OF`.
#[tokio::test]
async fn stale_balance_is_a_caution() {
    let world = world().await;
    let account = world
        .account_with("Old", "checking", "USD", json!({}))
        .await;
    world.balance(account, "2026-11-07", 500).await;
    world.emergency_fund(0).await;

    let forecast = world.forecast().await;

    assert_eq!(forecast["status"], "provisional");
    assert_eq!(forecast["surplus"], usd(500), "it keeps the number");
    let stale = caution(&forecast, "balance_stale");
    assert!(stale["account"] == json!(account), "names it");
    assert_eq!(stale["on"], "2026-11-07");
    assert_eq!(stale["age_days"], 8);
}

#[tokio::test]
async fn counted_account_without_balance_blocks() {
    let world = world().await;
    let account = world
        .account_with("Empty", "checking", "USD", json!({}))
        .await;
    world.emergency_fund(0).await;

    let forecast = world.forecast().await;

    assert_eq!(forecast["status"], "unavailable");
    assert_eq!(forecast["surplus"], Value::Null);
    assert_eq!(
        forecast["blockers"],
        json!([{"kind": "balance_missing", "account": account, "name": "Empty"}])
    );
}

#[tokio::test]
async fn mixed_currencies_answer_200_without_a_surplus() {
    let world = world().await;
    world.funded("Checking", 1_000).await;
    let euros = world
        .account_with("Euros", "checking", "EUR", json!({}))
        .await;
    world.balance(euros, super::BALANCE_ON, 2_000).await;
    world.emergency_fund(100).await;
    world
        .emergency_fund_of("EUR", &json!({"kind": "fixed", "minor_units": 0}))
        .await;

    let forecast = world.forecast().await;

    assert_eq!(forecast["status"], "unavailable");
    assert_eq!(forecast["surplus"], Value::Null);
    assert_eq!(kinds(&forecast, "blockers"), ["mixed_currencies"]);
    assert_eq!(forecast["blockers"][0]["currencies"], json!(["EUR", "USD"]));
    let rows = forecast["totals"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "one row per currency");
    let row = |currency: &str| rows.iter().find(|row| row["currency"] == currency).unwrap();
    assert_eq!(row("EUR")["surplus"]["minor_units"], 2_000, "own number");
    assert_eq!(
        row("USD")["surplus"]["minor_units"],
        900,
        "after its emergency fund"
    );
}

/// A forecast doesn't convert (spec §8.10 N1): a rate can't lift the blocker. It passes
/// at once by design, and fails only if someone wires exchange rates into the forecast.
#[tokio::test]
async fn rates_don_t_lift_the_mixed_currencies_blocker() {
    let world = world().await;
    world.funded("Checking", 1_000).await;
    let euros = world
        .account_with("Euros", "checking", "EUR", json!({}))
        .await;
    world.balance(euros, super::BALANCE_ON, 2_000).await;
    world.emergency_fund(100).await;
    world
        .emergency_fund_of("EUR", &json!({"kind": "fixed", "minor_units": 0}))
        .await;
    let before = world.forecast().await;

    let mut tx = world.server.store.begin().await.unwrap();
    tx.upsert_fx_rates(
        Currency::parse("USD").unwrap(),
        FxSource::Ecb,
        &[RateObservation {
            on: "2026-11-13".parse().unwrap(),
            rate: "1.1".parse().unwrap(),
        }],
        world.server.clock.now(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    world.settings(&json!({"display_currency": "USD"})).await;
    let forecast = world.forecast().await;

    assert_eq!(forecast["status"], "unavailable");
    assert_eq!(forecast["surplus"], Value::Null);
    assert_eq!(kinds(&forecast, "blockers"), ["mixed_currencies"]);
    assert_eq!(forecast["totals"], before["totals"], "each currency's own");
}

#[tokio::test]
async fn corrupt_account_currency_fails_loudly() {
    let world = world().await;
    let account = world.funded("Checking", 1_000).await;
    world.emergency_fund(0).await;
    world
        .server
        .store
        .execute_unchecked_for_tests(
            "UPDATE accounts SET currency = 'ZZZ' WHERE id = ?",
            &account.to_string(),
        )
        .await
        .unwrap();

    let (status, text) = world.send(Method::GET, FORECAST, &world.admin, None).await;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    let body: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(body["type"], "urn:coffer:problem:internal");
    assert_eq!(body.get("surplus"), None, "no number");
}
