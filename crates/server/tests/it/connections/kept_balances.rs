//! Balances reported while a provider account is unbound: kept one per day,
//! brought in as history by the bind, gone with the connection, and shown
//! nowhere before that.

use super::{PLAINTEXT, World, reported, setup_token, world};
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
use coffer_server::testkit::browser::{Browser, with_admin};
use coffer_store::NewSyncRun;
use jiff::civil::Date;
use jiff::{SignedDuration, Span};
use serde_json::json;

pub(super) const FEED: &str = "acct-1";
/// An amount reported on an older day, kept and nowhere else.
const OLD_RAW: &str = "4242.17";
const OLD_MINOR: &str = "424217";
/// The amount of the last report, which the provider account shows.
const LAST_RAW: &str = "13.31";
/// More than an `i64` of minor units holds.
const TOO_LARGE: &str = "999999999999999999.00";

impl World {
    pub(super) fn today(&self) -> Date {
        self.server
            .clock
            .now()
            .to_zoned(self.server.config.timezone.clone())
            .date()
    }

    pub(super) fn day(&self, ago: i64) -> Date {
        self.today().checked_sub(Span::new().days(ago)).unwrap()
    }

    /// A sync reporting `value` for the feed, in `currency`, on the day `ago`
    /// days back.
    pub(super) async fn report(&self, ago: i64, value: &str, currency: &str) {
        self.sync_once(vec![balance_of(FEED, currency, self.day(ago), value)])
            .await;
    }

    /// The kept days, as `(day, minor units)`, oldest first.
    pub(super) async fn kept(&self, connection: ConnectionId) -> Vec<(String, i64)> {
        self.server
            .store
            .kept_balances_for_tests(connection)
            .await
            .unwrap()
    }

    pub(super) async fn bind_feed(&self, target: AccountId) {
        let feed = self.provider_account(FEED).await;
        self.server
            .sync
            .bind(&self.alice, feed.id, target)
            .await
            .unwrap();
    }

    pub(super) async fn manual(&self, target: AccountId, on: Date, minor_units: i64) {
        self.server
            .app_state()
            .ledger
            .set_manual_balance(&self.alice, target, on, minor_units)
            .await
            .unwrap();
    }

    /// The account's balances through the API, newest first, as
    /// `(day, minor units, source)`.
    pub(super) async fn history(&self, target: AccountId) -> Vec<(String, i64, String)> {
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

    pub(super) fn sync_row(&self, ago: i64, minor_units: i64) -> (String, i64, String) {
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

pub(super) fn balance_of(id: &str, currency: &str, on: Date, value: &str) -> ProviderAccountData {
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

/// A bank's date can honestly lag, so 31 days back is still kept.
#[tokio::test]
async fn a_balance_at_the_age_limit_is_kept() {
    let world = world().await;
    world.report(31, "10.00", "USD").await;

    assert_eq!(
        world.kept(world.connection).await,
        [(world.day(31).to_string(), 1000)]
    );
}

#[tokio::test]
async fn a_balance_past_the_age_limit_is_not_kept() {
    let world = world().await;
    world.report(32, "10.00", "USD").await;

    assert_eq!(world.kept(world.connection).await, []);
}

/// Nothing dedupes ids within an answer: each entry is applied, one row per day.
#[tokio::test]
async fn an_id_repeated_in_one_answer_keeps_one_row_per_day() {
    let world = world().await;

    world
        .sync_once(vec![
            balance_of(FEED, "USD", world.day(3), "10.00"),
            balance_of(FEED, "USD", world.day(2), "11.00"),
            balance_of(FEED, "USD", world.day(2), "12.00"),
        ])
        .await;

    assert_eq!(
        world.kept(world.connection).await,
        [
            (world.day(3).to_string(), 1000),
            (world.day(2).to_string(), 1200)
        ]
    );
}

/// A currency change with no balance leaves nothing the bind could read in the
/// new currency: 1000 yen must not become 100000 cents.
#[tokio::test]
async fn a_currency_change_without_a_balance_clears_the_last_report() {
    let world = world().await;
    world.report(1, "1000", "JPY").await;
    world
        .sync_once(vec![reported(
            FEED,
            "USD",
            Some(AccountKind::Checking),
            Vec::new(),
        )])
        .await;
    assert_eq!(world.provider_account(FEED).await.last_balance, None);
    let target = world.account("Checking", "USD").await;

    world.bind_feed(target).await;

    assert_eq!(world.history(target).await, []);
}

#[tokio::test]
async fn a_sync_without_a_balance_in_the_same_currency_keeps_the_last_report() {
    let world = world().await;
    world.report(1, "10.00", "USD").await;
    world
        .sync_once(vec![reported(
            FEED,
            "USD",
            Some(AccountKind::Checking),
            Vec::new(),
        )])
        .await;

    let last = world.provider_account(FEED).await.last_balance.unwrap();

    assert_eq!(last.amount_raw, "10.00");
}

/// What the bind wrote, told apart from what it dropped and skipped, with the
/// days it spans and no amount.
#[tokio::test]
async fn the_bind_audit_gives_the_days_and_what_was_left_out() {
    let world = world().await;
    world.report(5, "1000", "JPY").await;
    world.report(4, "10.00", "USD").await;
    world.report(3, "12.00", "USD").await;
    world.report(2, OLD_RAW, "USD").await;
    let target = world.account("Checking", "USD").await;
    world.manual(target, world.day(4), 777).await;

    world.bind_feed(target).await;

    let binds = world.audit_actions("provider_account.bind").await;
    let field = |name: &str| {
        binds[0]
            .changes
            .iter()
            .find(|(field, _)| *field == name)
            .map(|(_, change)| change.after.clone())
    };
    assert_eq!(field("balances_brought_in"), Some(AuditValue::Int(2)));
    assert_eq!(
        field("balances_first_day"),
        Some(AuditValue::Text(world.day(3).to_string()))
    );
    assert_eq!(
        field("balances_last_day"),
        Some(AuditValue::Text(world.day(2).to_string()))
    );
    assert_eq!(field("balances_other_currency"), Some(AuditValue::Int(1)));
    assert_eq!(field("balances_already_there"), Some(AuditValue::Int(1)));
    let text = serde_json::to_string(&binds[0].changes).unwrap();
    assert!(!shows_old_day(&text));
}

#[tokio::test]
async fn the_bind_audit_has_no_days_when_nothing_was_brought_in() {
    let world = world().await;
    world
        .sync_once(vec![reported(
            FEED,
            "USD",
            Some(AccountKind::Checking),
            Vec::new(),
        )])
        .await;
    let target = world.account("Checking", "USD").await;

    world.bind_feed(target).await;

    let binds = world.audit_actions("provider_account.bind").await;
    let first = binds[0]
        .changes
        .iter()
        .find(|(field, _)| *field == "balances_first_day")
        .map(|(_, change)| change.after.clone());
    assert_eq!(first, Some(AuditValue::Null));
}

/// A failure after the bring-in, at the bind's last write, undoes all of it.
#[tokio::test]
async fn a_bind_that_fails_after_bringing_in_changes_nothing() {
    let world = world().await;
    world.report(2, "10.00", "USD").await;
    world.report(1, "11.00", "USD").await;
    let target = world.account("Checking", "USD").await;
    let feed = world.provider_account(FEED).await;
    world
        .server
        .store
        .inject_bind_audit_failure_for_tests()
        .await
        .unwrap();

    let failed = world.server.sync.bind(&world.alice, feed.id, target).await;

    assert!(matches!(failed, Err(SyncError::Store(_))));
    assert_eq!(world.history(target).await, []);
    assert_eq!(world.kept(world.connection).await.len(), 2);
    assert_eq!(world.provider_account(FEED).await.account, None);
}

/// As with held records, new credentials don't start the connection over: what
/// the old login reported stays, and a later bind brings it in.
#[tokio::test]
async fn a_reconnect_keeps_what_the_old_login_reported() {
    let world = world().await;
    world.report(1, "10.00", "USD").await;
    world.server.clock.advance(SignedDuration::from_hours(1));

    world
        .server
        .sync
        .reconnect(
            &world.alice,
            world.connection,
            setup_token("https://user:RENEWED-CREDENTIAL@bridge.example/simplefin"),
        )
        .await
        .unwrap();

    assert_eq!(
        world.kept(world.connection).await,
        [(world.day(1).to_string(), 1000)]
    );
}

/// The older day's amount appears nowhere, for anyone, through the API; a
/// restricted token sees no unbound provider account at all, so not the last
/// report either, and is refused what spans every account.
#[tokio::test]
async fn nothing_kept_shows_through_the_api_before_binding() {
    let world = world().await;
    let visible = world.account("Visible", "USD").await;
    world.report(2, OLD_RAW, "USD").await;
    world.report(0, LAST_RAW, "USD").await;
    let admin = world.token(Scope::Admin).await;
    let restricted = world.restricted_token(visible).await;
    let sweep = [
        ("/api/v1/accounts".to_owned(), StatusCode::OK),
        (
            format!("/api/v1/accounts/{visible}/balances"),
            StatusCode::OK,
        ),
        ("/api/v1/connections".to_owned(), StatusCode::OK),
        ("/api/v1/provider-accounts".to_owned(), StatusCode::OK),
        ("/api/v1/forecast".to_owned(), StatusCode::FORBIDDEN),
        ("/api/v1/audit".to_owned(), StatusCode::FORBIDDEN),
    ];

    for (uri, restricted_status) in &sweep {
        let (status, body) = world.call(Method::GET, uri, &admin, None).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert!(!shows_old_day(&body.to_string()), "{uri}");

        let (status, body) = world.call(Method::GET, uri, &restricted, None).await;
        assert_eq!(status, *restricted_status, "{uri}");
        let body = body.to_string();
        assert!(!shows_old_day(&body) && !body.contains(LAST_RAW), "{uri}");
    }
}

/// Nor on a page, for the admin signed in.
#[tokio::test]
async fn nothing_kept_shows_on_a_page_before_binding() {
    let (world, mut browser) = signed_in_world().await;
    world.report(2, OLD_RAW, "USD").await;
    assert_eq!(world.kept(world.connection).await.len(), 1);
    world.report(0, LAST_RAW, "USD").await;

    for page in ["/forecast", "/connections", "/account", "/admin/members"] {
        let reply = browser.get(page).await;
        assert_eq!(reply.status, StatusCode::OK, "{page}");
        assert!(!shows_old_day(&reply.body), "{page}");
    }
}

/// A world whose admin is also signed in to a browser.
pub(super) async fn signed_in_world() -> (World, Browser) {
    let (mut server, signed_in, _) = with_admin().await;
    let fake = server.use_fake_provider();
    let mut browser = Browser::new(&server);
    browser.cookie.clone_from(&signed_in.cookie);
    let alice = server
        .sessions
        .resolve(browser.cookie.as_deref().unwrap())
        .await
        .unwrap();
    let connection = server
        .seed_connection(
            ProviderKind::SimpleFin,
            "Family bank",
            alice.member,
            PLAINTEXT,
        )
        .await;
    let world = World {
        server,
        fake,
        alice,
        connection,
    };
    (world, browser)
}

fn shows_old_day(body: &str) -> bool {
    body.contains(OLD_RAW) || body.contains(OLD_MINOR)
}
