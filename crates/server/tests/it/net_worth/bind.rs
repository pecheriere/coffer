//! A bind brings a provider account's kept days into net worth in its own transaction
//! (spec §8.10 N3).

use axum::http::{Method, StatusCode};
use coffer_domain::accounts::AccountKind;
use coffer_domain::ids::{AccountId, ConnectionId};
use coffer_domain::money::DecimalAmount;
use coffer_domain::sync::SyncTrigger;
use coffer_domain::time::Clock as _;
use coffer_server::sync::apply::Coverage;
use coffer_server::sync::provider::{FetchResult, ProviderAccountData, ProviderKind};
use coffer_server::testkit::{FakeProvider, TestServer};
use coffer_store::NewSyncRun;
use jiff::civil::Date;
use serde_json::{Value, json};

use super::{TODAY, World, day, holder_point, money, point, rate_missing, usd, world_on};

const FEED: &str = "feed-1";
/// Marks the kept amounts, so a leak shows up in a text search.
const FIRST_RAW: &str = "4242.17";
const FIRST_MINOR: i64 = 424_217;

struct Feed {
    world: World,
    fake: std::sync::Arc<FakeProvider>,
    connection: ConnectionId,
}

async fn feed() -> Feed {
    let (server, fake) = TestServer::with_fake_provider().await;
    let world = world_on(server).await;
    let connection = world
        .server
        .seed_connection(
            ProviderKind::SimpleFin,
            "Family bank",
            world.alice.member,
            "https://user:pass@bridge.example/simplefin",
        )
        .await;
    Feed {
        world,
        fake,
        connection,
    }
}

fn data(currency: &str, on: Date, value: &str) -> ProviderAccountData {
    ProviderAccountData {
        provider_account_id: FEED.into(),
        name: "Everyday savings".into(),
        institution: Some("Example Bank".into()),
        currency: currency.into(),
        kind_hint: Some(AccountKind::Checking),
        balance: Some((on, DecimalAmount::parse(value).unwrap())),
        transactions: Vec::new(),
        removed: Vec::new(),
        coverage: Coverage::Delta,
        error: None,
        unusable: Vec::new(),
    }
}

impl Feed {
    /// A sync on the clock's day reporting `value` for that day.
    async fn sync(&self, on: &str, currency: &str, value: &str) {
        self.world.at(on);
        self.fake.push(Ok(FetchResult {
            accounts: vec![data(currency, day(on), value)],
            connection_errors: Vec::new(),
            skipped: 0,
            cursor: None,
        }));
        let server = &self.world.server;
        let mut tx = server.store.begin().await.unwrap();
        let run = tx
            .insert_sync_run(NewSyncRun {
                connection: self.connection,
                trigger: SyncTrigger::Manual,
                requested_by: Some((self.world.alice.member, None)),
                queued_at: server.clock.now(),
            })
            .await
            .unwrap();
        tx.commit().await.unwrap();
        server.sync.run(run.id).await.unwrap();
    }

    /// Binds the feed to `target` through the API.
    async fn bind(&self, target: AccountId) {
        let world = &self.world;
        let (status, text) = world
            .send(Method::GET, "/api/v1/provider-accounts", &world.admin, None)
            .await;
        assert_eq!(status, StatusCode::OK);
        let listed: Value = serde_json::from_str(&text).unwrap();
        let feed_id = listed["items"][0]["id"].as_str().unwrap().to_owned();
        world
            .admin_send(
                Method::POST,
                &format!("/api/v1/provider-accounts/{feed_id}/bind"),
                &json!({"account": target}),
            )
            .await;
    }
}

#[tokio::test]
async fn a_bind_s_history_reaches_net_worth() {
    const RANGE: &str = "from=2026-10-29&to=2026-11-02&interval=day";
    let feed = feed().await;
    let world = &feed.world;
    world.at("2026-10-29");
    world.rebuild("2026-10-29").await;
    world.display("USD").await;
    let checking = world.holding("Checking", "USD").await;
    world.balance(checking, "2026-10-29", 1_000).await;

    for (on, raw) in [
        ("2026-10-30", FIRST_RAW),
        ("2026-10-31", "11.00"),
        ("2026-11-01", "12.00"),
    ] {
        feed.sync(on, "USD", raw).await;
        world.fill(on).await;

        let (status, text) = world.report_as(&world.admin, RANGE).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            !text.contains(&FIRST_MINOR.to_string()),
            "kept, and in no answer"
        );
        let report: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(report["cautions"], json!([]), "nothing to warn about yet");
        assert_eq!(point(&report, on)["total"], usd(1_000));
    }

    world.at(TODAY);
    world.fill(TODAY).await;
    let target = world.holding("Savings", "USD").await;
    feed.bind(target).await;

    let report = world.report(RANGE).await;
    let kept = holder_point(&report, target, "2026-10-30");
    assert_eq!(kept["balance"], usd(FIRST_MINOR));
    assert_eq!(kept["balance_on"], "2026-10-30");
    assert_eq!(kept["balance_source"], "sync");
    assert_eq!(
        point(&report, "2026-10-30")["total"],
        usd(1_000 + FIRST_MINOR)
    );
    assert_eq!(point(&report, "2026-10-31")["total"], usd(1_000 + 1_100));
    assert_eq!(point(&report, "2026-11-01")["total"], usd(1_000 + 1_200));
    assert_eq!(
        holder_point(&report, target, TODAY)["balance_on"],
        "2026-11-01",
        "carried from the last day reported"
    );

    let rows = world.rows().await;
    world.rebuild(TODAY).await;
    assert!(
        world.rows().await == rows,
        "a rebuild finds nothing to change"
    );
}

#[tokio::test]
async fn a_bind_into_a_new_currency_reports_a_missing_rate_until_fetched() {
    let feed = feed().await;
    let world = &feed.world;
    world.display("USD").await;
    world.rates("USD", &[("2026-10-30", "1.1172")]).await;
    feed.sync("2026-11-01", "THB", "100.00").await;
    world.at(TODAY);
    let target = world.holding("Baht", "THB").await;

    feed.bind(target).await;

    let query = "from=2026-11-01&to=2026-11-02&interval=day";
    let report = world.report(query).await;
    assert_eq!(point(&report, TODAY)["total"], Value::Null);
    assert_eq!(
        point(&report, TODAY)["by_currency"],
        json!([money(10_000, "THB")])
    );
    assert_eq!(rate_missing(&report, "THB")["reason"], "not_fetched_yet");

    world.rates("THB", &[("2026-10-30", "36.5")]).await;

    let report = world.report(query).await;
    assert_eq!(point(&report, TODAY)["total"], usd(306));
    assert_eq!(report["cautions"], json!([]));
}
