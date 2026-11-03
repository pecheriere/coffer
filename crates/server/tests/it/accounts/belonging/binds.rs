//! A provider account's way in: the page and the API share the rule of a create, after the
//! provider's hint has decided the kind.

use std::sync::Arc;

use super::super::{World, call, token};
use super::{COUNTED_REFUSAL, LABEL_ON_HOUSEHOLD, flags, stored, stored_flags};
use axum::http::{Method, StatusCode};
use coffer_domain::accounts::{AccountKind, BelongsTo, LiquidityClass};
use coffer_domain::identity::Scope;
use coffer_domain::ids::{ConnectionId, ProviderAccountId};
use coffer_domain::money::DecimalAmount;
use coffer_domain::sync::SyncTrigger;
use coffer_domain::time::Clock as _;
use coffer_server::secrets::SecretToken;
use coffer_server::sync::apply::Coverage;
use coffer_server::sync::provider::{FetchResult, ProviderAccountData, ProviderKind};
use coffer_server::testkit::FakeProvider;
use coffer_server::testkit::browser::{Browser, with_admin};
use coffer_store::NewSyncRun;
use jiff::Span;
use jiff::civil::Date;
use serde_json::{Value, json};

/// A signed-in admin with a connection whose provider is scripted.
pub(super) struct Feeds {
    pub(super) world: World,
    fake: Arc<FakeProvider>,
    connection: ConnectionId,
    pub(super) write: SecretToken,
    browser: Browser,
}

pub(super) async fn feeds() -> Feeds {
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
            "https://user:pass@bridge.example/simplefin",
        )
        .await;
    let world = World { server, alice };
    let (_, write) = token(&world, Scope::Write).await;
    Feeds {
        world,
        fake,
        connection,
        write,
        browser,
    }
}

fn feed(hint: Option<AccountKind>, balance: Option<(Date, &str)>) -> ProviderAccountData {
    ProviderAccountData {
        provider_account_id: "feed-1".into(),
        name: "College plan".into(),
        institution: Some("Example Bank".into()),
        currency: "USD".into(),
        kind_hint: hint,
        balance: balance.map(|(on, value)| (on, DecimalAmount::parse(value).unwrap())),
        transactions: Vec::new(),
        removed: Vec::new(),
        coverage: Coverage::Delta,
        error: None,
        unusable: Vec::new(),
    }
}

impl Feeds {
    fn day(&self, ago: i64) -> Date {
        self.world
            .server
            .clock
            .now()
            .to_zoned(self.world.server.config.default_timezone.clone())
            .date()
            .checked_sub(Span::new().days(ago))
            .unwrap()
    }

    /// A sync reporting the one feed, in a run queued directly so no rate limit applies.
    async fn report(&self, hint: Option<AccountKind>, balance: Option<(Date, &str)>) {
        self.fake.push(Ok(FetchResult {
            accounts: vec![feed(hint, balance)],
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

    /// The feed as the API lists it.
    pub(super) async fn feed_uri(&self, hint: Option<AccountKind>) -> String {
        self.report(hint, None).await;
        self.listed_uri().await
    }

    async fn listed_uri(&self) -> String {
        let (status, page) = call(
            &self.world.server,
            Method::GET,
            "/api/v1/provider-accounts",
            &self.write,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let id = page["items"][0]["id"].as_str().unwrap();
        format!("/api/v1/provider-accounts/{id}")
    }

    pub(super) async fn create_on(&self, uri: &str, body: Value) -> (StatusCode, Value) {
        call(
            &self.world.server,
            Method::POST,
            &format!("{uri}/create-account"),
            &self.write,
            Some(body),
        )
        .await
    }

    async fn audit_of(&self, uri: &str) -> Value {
        let (status, page) = call(
            &self.world.server,
            Method::GET,
            &format!(
                "/api/v1/audit?entity_type=provider_account&entity_id={}",
                uri.rsplit('/').next().unwrap()
            ),
            &self.write,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        page
    }

    /// The feed is still unbound, and no row was created for it.
    async fn assert_nothing_bound(&self) {
        let (status, page) = call(
            &self.world.server,
            Method::GET,
            "/api/v1/provider-accounts?unbound=true",
            &self.write,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(page["items"].as_array().unwrap().len(), 1, "still unbound");
        assert_eq!(
            super::super::account_count(&self.world.server).await,
            0,
            "nothing was created"
        );
    }
}

#[tokio::test]
async fn a_page_bind_of_a_529_stays_out() {
    let mut feeds = feeds().await;
    let uri = feeds.feed_uri(None).await;
    let id = uri.rsplit('/').next().unwrap().to_owned();
    let csrf = feeds.browser.get("/connections").await.csrf();
    let reply = feeds
        .browser
        .post_form(
            &format!("/connections/provider-accounts/{id}/create-account"),
            &[("csrf", &csrf), ("name", "College"), ("kind", "education")],
        )
        .await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);

    let (status, page) = call(
        &feeds.world.server,
        Method::GET,
        "/api/v1/accounts",
        &feeds.write,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let created = &page["items"][0];
    assert_eq!(created["kind"], "education");
    assert_eq!(created["belongs_to"], "someone_else");
    assert_eq!(flags(created), (false, false));
    let record = stored(&feeds.world, created).await;
    assert_eq!(stored_flags(&record), (false, false));
}

#[tokio::test]
async fn a_page_bind_of_a_checking_account_still_counts() {
    let mut feeds = feeds().await;
    let uri = feeds.feed_uri(None).await;
    let id = uri.rsplit('/').next().unwrap().to_owned();
    let csrf = feeds.browser.get("/connections").await.csrf();
    let reply = feeds
        .browser
        .post_form(
            &format!("/connections/provider-accounts/{id}/create-account"),
            &[("csrf", &csrf), ("name", "Everyday"), ("kind", "checking")],
        )
        .await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    let (_, page) = call(
        &feeds.world.server,
        Method::GET,
        "/api/v1/accounts",
        &feeds.write,
        None,
    )
    .await;
    assert_eq!(flags(&page["items"][0]), (true, true));
}

#[tokio::test]
async fn an_api_bind_with_no_flags_stays_out() {
    let feeds = feeds().await;
    let uri = feeds.feed_uri(None).await;
    let (status, linked) = feeds.create_on(&uri, json!({"kind": "education"})).await;
    assert_eq!(status, StatusCode::CREATED, "{linked}");
    let created = &linked["account"];
    assert_eq!(created["belongs_to"], "someone_else");
    assert_eq!(created["source"], "linked");
    assert_eq!(flags(created), (false, false));
    let record = stored(&feeds.world, created).await;
    assert_eq!(record.belongs_to, BelongsTo::SomeoneElse);
    assert!(!record.belongs_to_overridden);
    assert_eq!(stored_flags(&record), (false, false));
    assert_eq!(record.liquidity, LiquidityClass::Invested);
}

#[tokio::test]
async fn an_api_bind_of_a_checking_account_still_counts() {
    let feeds = feeds().await;
    let uri = feeds.feed_uri(None).await;
    let (status, linked) = feeds.create_on(&uri, json!({"kind": "checking"})).await;
    assert_eq!(status, StatusCode::CREATED, "{linked}");
    assert_eq!(linked["account"]["belongs_to"], "household");
    assert_eq!(flags(&linked["account"]), (true, true));
}

#[tokio::test]
async fn a_bind_keeps_a_flag_named_off() {
    let feeds = feeds().await;
    let uri = feeds.feed_uri(None).await;
    let (status, linked) = feeds
        .create_on(
            &uri,
            json!({"kind": "checking", "include_in_forecast": false}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{linked}");
    assert_eq!(flags(&linked["account"]), (true, false));
}

#[tokio::test]
async fn a_provider_s_hint_decides_the_kind_before_the_rule() {
    let feeds = feeds().await;
    let uri = feeds.feed_uri(Some(AccountKind::Education)).await;
    let (status, linked) = feeds.create_on(&uri, json!({})).await;
    assert_eq!(status, StatusCode::CREATED, "{linked}");
    assert_eq!(linked["account"]["kind"], "education");
    assert_eq!(linked["account"]["belongs_to"], "someone_else");
    assert_eq!(flags(&linked["account"]), (false, false));
}

#[tokio::test]
async fn a_bind_takes_whose_money_it_is_and_a_label() {
    let feeds = feeds().await;
    let uri = feeds.feed_uri(None).await;
    let (status, linked) = feeds
        .create_on(
            &uri,
            json!({"kind": "brokerage", "belongs_to": "someone_else", "beneficiary": "Child A"}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{linked}");
    assert_eq!(linked["account"]["belongs_to"], "someone_else");
    assert_eq!(linked["account"]["beneficiary"], "Child A");
    let record = stored(&feeds.world, &linked["account"]).await;
    assert!(record.belongs_to_overridden);
}

#[tokio::test]
async fn a_provider_s_name_never_fills_the_label() {
    let feeds = feeds().await;
    let uri = feeds.feed_uri(None).await;
    let (_, linked) = feeds.create_on(&uri, json!({"kind": "education"})).await;
    assert_eq!(linked["account"]["name"], "College plan");
    assert_eq!(linked["account"]["beneficiary"], Value::Null);
}

#[tokio::test]
async fn an_empty_beneficiary_on_a_bind_is_the_same_as_absent() {
    let feeds = feeds().await;
    let uri = feeds.feed_uri(None).await;
    let (status, linked) = feeds
        .create_on(&uri, json!({"kind": "checking", "beneficiary": ""}))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{linked}");
    assert_eq!(linked["account"]["beneficiary"], Value::Null);
}

#[tokio::test]
async fn a_bind_refused_by_the_rule_binds_nothing() {
    let feeds = feeds().await;
    let uri = feeds.feed_uri(None).await;
    for (body, words) in [
        (
            json!({"kind": "education", "include_in_net_worth": true}),
            COUNTED_REFUSAL,
        ),
        (
            json!({"kind": "checking", "beneficiary": "Child A"}),
            LABEL_ON_HOUSEHOLD,
        ),
    ] {
        let (status, problem) = feeds.create_on(&uri, body).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
        assert_eq!(problem["code"], "invalid");
        assert_eq!(problem["detail"], words);
        feeds.assert_nothing_bound().await;
    }
}

#[tokio::test]
async fn a_bad_label_is_refused_before_the_feed_is_read() {
    let feeds = feeds().await;
    let missing = format!("/api/v1/provider-accounts/{}", ProviderAccountId::new());
    for body in [
        json!({"beneficiary": "Child\u{0007}"}),
        json!({"name": "Bad\u{0007}name"}),
    ] {
        let (status, problem) = feeds.create_on(&missing, body).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
    }
}

#[tokio::test]
async fn a_bound_529_brings_its_kept_balances() {
    let feeds = feeds().await;
    let (first, second) = (feeds.day(3), feeds.day(2));
    feeds.report(None, Some((first, "100.00"))).await;
    feeds.report(None, Some((second, "101.50"))).await;
    let uri = feeds.listed_uri().await;

    let (status, linked) = feeds.create_on(&uri, json!({"kind": "education"})).await;
    assert_eq!(status, StatusCode::CREATED, "{linked}");
    let target = linked["account"]["id"].as_str().unwrap();
    assert_eq!(linked["account"]["belongs_to"], "someone_else");
    assert_eq!(flags(&linked["account"]), (false, false));

    let (status, page) = call(
        &feeds.world.server,
        Method::GET,
        &format!("/api/v1/accounts/{target}/balances"),
        &feeds.write,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let days: Vec<(&str, i64)> = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| {
            (
                item["on"].as_str().unwrap(),
                item["balance"]["minor_units"].as_i64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        days,
        [
            (second.to_string().as_str(), 10150),
            (first.to_string().as_str(), 10000)
        ]
    );

    let audit = feeds.audit_of(&uri).await;
    let bind = audit["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["action"] == "provider_account.bind")
        .unwrap();
    assert_eq!(bind["changes"]["balances_brought_in"]["after"], json!(2));
}
