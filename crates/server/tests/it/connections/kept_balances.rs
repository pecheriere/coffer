//! Balances reported while a provider account is unbound: kept one per day,
//! brought in as history by the bind, gone with the connection, and shown
//! nowhere before that.

use super::{PLAINTEXT, World, reported, world};
use axum::http::{Method, StatusCode};
use coffer_domain::accounts::AccountKind;
use coffer_domain::audit::AuditValue;
use coffer_domain::identity::{Scope, TokenName};
use coffer_domain::ids::{AccountId, ConnectionId};
use coffer_domain::money::DecimalAmount;
use coffer_domain::policy::{AccountLevel, GrantTarget, TokenGrant};
use coffer_domain::sync::SyncTrigger;
use coffer_domain::time::Clock as _;
use coffer_server::secrets::SecretToken;
use coffer_server::sync::SyncError;
use coffer_server::sync::provider::{FetchResult, ProviderAccountData, ProviderKind};
use coffer_store::NewSyncRun;
use jiff::Span;
use jiff::civil::Date;
use serde_json::json;

const FEED: &str = "acct-1";
/// More than an `i64` of minor units holds.
const TOO_LARGE: &str = "999999999999999999.00";

impl World {
    fn today(&self) -> Date {
        self.server
            .clock
            .now()
            .to_zoned(self.server.config.timezone.clone())
            .date()
    }

    fn day(&self, ago: i64) -> Date {
        self.today().checked_sub(Span::new().days(ago)).unwrap()
    }

    /// A sync reporting `value` for the feed, in `currency`, on the day `ago`
    /// days back.
    async fn report(&self, ago: i64, value: &str, currency: &str) {
        self.sync_once(vec![balance_of(FEED, currency, self.day(ago), value)])
            .await;
    }

    /// The kept days, as `(day, minor units)`, oldest first.
    async fn kept(&self, connection: ConnectionId) -> Vec<(String, i64)> {
        self.server
            .store
            .kept_balances_for_tests(connection)
            .await
            .unwrap()
    }

    async fn bind_feed(&self, target: AccountId) {
        let feed = self.provider_account(FEED).await;
        self.server
            .sync
            .bind(&self.alice, feed.id, target)
            .await
            .unwrap();
    }

    async fn manual(&self, target: AccountId, on: Date, minor_units: i64) {
        self.server
            .app_state()
            .ledger
            .set_manual_balance(&self.alice, target, on, minor_units)
            .await
            .unwrap();
    }

    /// The account's balances through the API, newest first, as
    /// `(day, minor units, source)`.
    async fn history(&self, target: AccountId) -> Vec<(String, i64, String)> {
        let token = self.token(Scope::Read).await;
        let (status, page) = self
            .call(
                Method::GET,
                &format!("/api/v1/accounts/{target}/balances"),
                &token,
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        page["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| {
                (
                    item["on"].as_str().unwrap().to_owned(),
                    item["balance"]["minor_units"].as_i64().unwrap(),
                    item["source"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }

    fn sync_row(&self, ago: i64, minor_units: i64) -> (String, i64, String) {
        (self.day(ago).to_string(), minor_units, "sync".to_owned())
    }

    async fn restricted_token(&self, visible: AccountId) -> SecretToken {
        let (_, secret) = self
            .server
            .agent_tokens
            .create(
                &self.alice,
                TokenName::parse("restricted").unwrap(),
                Scope::Read,
                None,
                vec![TokenGrant {
                    target: GrantTarget::Account(visible),
                    level: AccountLevel::Balances,
                }],
            )
            .await
            .unwrap();
        secret
    }
}

fn balance_of(id: &str, currency: &str, on: Date, value: &str) -> ProviderAccountData {
    let mut data = reported(id, currency, Some(AccountKind::Checking), Vec::new());
    data.balance = Some((on, DecimalAmount::parse(value).unwrap()));
    data
}

#[tokio::test]
async fn days_reported_while_unbound_become_history_at_bind() {
    let world = world().await;
    world.report(2, "10.00", "USD").await;
    world.report(1, "11.00", "USD").await;
    world.report(0, "12.00", "USD").await;
    let target = world.account("Checking", "USD").await;

    world.bind_feed(target).await;

    assert_eq!(
        world.history(target).await,
        [
            world.sync_row(0, 1200),
            world.sync_row(1, 1100),
            world.sync_row(2, 1000)
        ]
    );
}

#[tokio::test]
async fn two_syncs_in_a_day_keep_the_last() {
    let world = world().await;
    world.report(1, "10.00", "USD").await;
    world.report(1, "15.50", "USD").await;

    assert_eq!(
        world.kept(world.connection).await,
        [(world.day(1).to_string(), 1550)]
    );
}

#[tokio::test]
async fn a_future_day_is_kept_as_today() {
    let world = world().await;
    world.report(-30, "10.00", "USD").await;

    assert_eq!(
        world.kept(world.connection).await,
        [(world.today().to_string(), 1000)]
    );
}

#[tokio::test]
async fn binding_does_not_overwrite_an_earlier_snapshot() {
    let world = world().await;
    world.report(2, "10.00", "USD").await;
    world.report(1, "11.00", "USD").await;
    let target = world.account("Checking", "USD").await;
    world.manual(target, world.day(2), 777).await;

    world.bind_feed(target).await;

    let history = world.history(target).await;
    assert_eq!(
        history[1],
        (world.day(2).to_string(), 777, "manual".to_owned())
    );
}

/// The bind also writes the balance last reported, which is usually the same
/// day as the last one kept.
#[tokio::test]
async fn binding_does_not_overwrite_a_snapshot_of_the_last_reported_day() {
    let world = world().await;
    world.report(0, "10.00", "USD").await;
    let target = world.account("Checking", "USD").await;
    world.manual(target, world.today(), 777).await;

    world.bind_feed(target).await;

    assert_eq!(
        world.history(target).await,
        [(world.today().to_string(), 777, "manual".to_owned())]
    );
}

#[tokio::test]
async fn binding_removes_what_was_kept() {
    let world = world().await;
    world.report(1, "10.00", "USD").await;
    let target = world.account("Checking", "USD").await;

    world.bind_feed(target).await;

    assert_eq!(world.kept(world.connection).await, []);
}

#[tokio::test]
async fn a_bound_feed_keeps_nothing() {
    let world = world().await;
    world.report(1, "10.00", "USD").await;
    let target = world.account("Checking", "USD").await;
    world.bind_feed(target).await;

    world.report(0, "11.00", "USD").await;

    assert_eq!(world.kept(world.connection).await, []);
    assert_eq!(world.history(target).await[0], world.sync_row(0, 1100));
}

/// Only what became a snapshot counts: a day the account already had does not.
#[tokio::test]
async fn the_bind_audit_says_how_many_balances_were_brought_in() {
    let world = world().await;
    world.report(3, "10.00", "USD").await;
    world.report(2, "11.00", "USD").await;
    world.report(1, "12.00", "USD").await;
    let target = world.account("Checking", "USD").await;
    world.manual(target, world.day(3), 777).await;

    world.bind_feed(target).await;

    let binds = world.audit_actions("provider_account.bind").await;
    let brought_in = binds[0]
        .changes
        .iter()
        .find(|(field, _)| *field == "balances_brought_in")
        .map(|(_, change)| change.after.clone());
    assert_eq!(brought_in, Some(AuditValue::Int(2)));
}

#[tokio::test]
async fn an_unreadable_balance_stores_nothing() {
    let world = world().await;
    world.report(1, TOO_LARGE, "USD").await;

    assert_eq!(world.kept(world.connection).await, []);
}

#[tokio::test]
async fn an_unreadable_balance_leaves_the_days_value_as_it_was() {
    let world = world().await;
    world.report(1, "10.00", "USD").await;

    world.report(1, TOO_LARGE, "USD").await;

    assert_eq!(
        world.kept(world.connection).await,
        [(world.day(1).to_string(), 1000)]
    );
}

/// 1000 yen are not 1000 cents.
#[tokio::test]
async fn a_balance_kept_in_another_currency_is_not_brought_in() {
    let world = world().await;
    world.report(2, "1000", "JPY").await;
    world.report(1, "11.00", "USD").await;
    let target = world.account("Checking", "USD").await;

    world.bind_feed(target).await;

    assert_eq!(world.history(target).await, [world.sync_row(1, 1100)]);
    assert_eq!(world.kept(world.connection).await, []);
}

#[tokio::test]
async fn a_bind_refused_for_its_currency_leaves_everything_as_it_was() {
    let world = world().await;
    world.report(1, "10.00", "USD").await;
    let target = world.account("Euros", "EUR").await;
    let feed = world.provider_account(FEED).await;

    let refused = world.server.sync.bind(&world.alice, feed.id, target).await;

    assert!(matches!(refused, Err(SyncError::CurrencyMismatch)));
    assert_eq!(world.history(target).await, []);
    assert_eq!(world.kept(world.connection).await.len(), 1);
}

/// The last report doesn't convert, and the account created for it still shows
/// the balance it just brought in.
#[tokio::test]
async fn an_account_created_at_bind_answers_with_its_latest_kept_balance() {
    let world = world().await;
    world.report(1, "10.00", "USD").await;
    world.report(0, TOO_LARGE, "USD").await;
    let feed = world.provider_account(FEED).await;
    let token = world.token(Scope::Write).await;

    let (status, linked) = world
        .call(
            Method::POST,
            &format!("/api/v1/provider-accounts/{}/create-account", feed.id),
            &token,
            Some(json!({})),
        )
        .await;

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        linked["account"]["latest_balance"]["balance"]["minor_units"],
        json!(1000)
    );
}

#[tokio::test]
async fn removing_a_connection_clears_its_kept_balances_only() {
    let world = world().await;
    world.report(1, "10.00", "USD").await;
    let other = world
        .server
        .seed_connection(
            ProviderKind::SimpleFin,
            "Other bank",
            world.alice.member,
            PLAINTEXT,
        )
        .await;
    world.fake.push(Ok(FetchResult {
        accounts: vec![balance_of("acct-9", "USD", world.day(1), "20.00")],
        connection_errors: Vec::new(),
        skipped: 0,
        cursor: None,
    }));
    let mut tx = world.server.store.begin().await.unwrap();
    let run = tx
        .insert_sync_run(NewSyncRun {
            connection: other,
            trigger: SyncTrigger::Manual,
            requested_by: None,
            queued_at: world.server.clock.now(),
        })
        .await
        .unwrap();
    tx.commit().await.unwrap();
    world.server.sync.run(run.id).await.unwrap();

    world
        .server
        .sync
        .remove_connection(&world.alice, world.connection)
        .await
        .unwrap();

    assert_eq!(world.kept(world.connection).await, []);
    assert_eq!(world.kept(other).await.len(), 1);
}

/// Whoever reads whatever, the older day's amount appears nowhere, and only a
/// token that may see unbound provider accounts sees the last report.
#[tokio::test]
async fn nothing_kept_shows_before_binding() {
    const OLD_RAW: &str = "4242.17";
    const OLD_MINOR: &str = "424217";
    const LAST_RAW: &str = "13.31";
    const SWEEP: [&str; 5] = [
        "/api/v1/accounts",
        "/api/v1/connections",
        "/api/v1/provider-accounts",
        "/api/v1/forecast",
        "/api/v1/audit",
    ];
    let world = world().await;
    let visible = world.account("Visible", "USD").await;
    world.report(2, OLD_RAW, "USD").await;
    world.report(0, LAST_RAW, "USD").await;
    let admin = world.token(Scope::Admin).await;
    let restricted = world.restricted_token(visible).await;

    for uri in SWEEP {
        let (status, body) = world.call(Method::GET, uri, &admin, None).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        let body = body.to_string();
        assert!(
            !body.contains(OLD_RAW) && !body.contains(OLD_MINOR),
            "{uri}"
        );

        let (_, body) = world.call(Method::GET, uri, &restricted, None).await;
        let body = body.to_string();
        assert!(
            !body.contains(OLD_RAW) && !body.contains(OLD_MINOR) && !body.contains(LAST_RAW),
            "{uri}"
        );
    }
}
