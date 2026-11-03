//! Money that isn't the household's, through the real router (spec §8.11 B6, B7): a bucket
//! answers beside net worth, added to nothing, and a token sees one only from what it sees at
//! `balances`. Answers are compared whole.

use std::sync::Arc;

use axum::http::StatusCode;
use coffer_domain::fx::{FxSource, RateObservation};
use coffer_domain::ids::AccountId;
use coffer_domain::money::Currency;
use coffer_server::fx::{FxService, RateError, RateProvider};
use coffer_server::sync::provider::ProviderFuture;
use jiff::civil::Date;
use serde_json::{Value, json};

use super::bind::feed;
use super::{TODAY, World, day, money, usd, world};

const WEEK: &str = "from=2026-10-26&to=2026-11-01&interval=day";
const ONE_DAY: &str = "from=2026-11-02&to=2026-11-02&interval=day";
/// Every point of the golden week, as a check of hand-computed totals.
const GOLDEN_TOTALS: [(&str, i64); 7] = [
    ("2026-10-26", 611_880),
    ("2026-10-27", 611_500),
    ("2026-10-28", 581_330),
    ("2026-10-29", 581_300),
    ("2026-10-30", 631_720),
    ("2026-10-31", 631_720),
    ("2026-11-01", 631_720),
];

fn parsed(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

/// `report` as it reads with nothing of its buckets.
fn top_level(report: &Value) -> Value {
    let mut top = report.clone();
    top.as_object_mut().unwrap().remove("buckets");
    top
}

/// The buckets of `report`.
fn buckets(report: &Value) -> &Vec<Value> {
    report["buckets"].as_array().unwrap()
}

/// `(on, total)` of each of `section`'s points.
fn totals(section: &Value) -> Vec<(String, Value)> {
    section["points"]
        .as_array()
        .unwrap()
        .iter()
        .map(|point| {
            (
                point["on"].as_str().unwrap().to_owned(),
                point["total"].clone(),
            )
        })
        .collect()
}

fn golden_totals() -> Vec<(String, Value)> {
    GOLDEN_TOTALS
        .iter()
        .map(|(on, minor)| ((*on).to_owned(), usd(*minor)))
        .collect()
}

/// The names `section` lists.
fn names(section: &Value) -> Vec<&str> {
    section["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|one| one["name"].as_str().unwrap())
        .collect()
}

/// The first of what `section` lists.
fn first_listed(section: &Value) -> &Value {
    &section["accounts"][0]
}

/// `(belongs_to, beneficiary)` of each bucket, in order.
fn keys(report: &Value) -> Vec<(Value, Value)> {
    buckets(report)
        .iter()
        .map(|bucket| (bucket["belongs_to"].clone(), bucket["beneficiary"].clone()))
        .collect()
}

impl World {
    /// An `education` account in `currency`, held for `label` unless that is `None`.
    async fn college(&self, name: &str, currency: &str, label: Option<&str>) -> AccountId {
        self.holding_with(name, "education", currency, json!({"beneficiary": label}))
            .await
    }

    /// Takes `holder`'s row of 10-28 out of the daily table.
    async fn hole(&self, holder: AccountId) {
        self.server
            .store
            .execute_unchecked_for_tests(
                "DELETE FROM account_daily_values WHERE account_id = ? AND on_date = '2026-10-28'",
                &holder.to_string(),
            )
            .await
            .unwrap();
    }

    /// Everything a bucket could reach an answer with: a label, a currency with no rate, a
    /// hole, an old balance, a positive liability, an owing closed account and a gift.
    async fn every_kind_of_bucket(&self) {
        let baht = self.college("Baht fund", "THB", Some("Child A")).await;
        self.balance(baht, "2026-10-26", 7_777_777).await;
        let holed = self.college("Holed fund", "USD", Some("Child B")).await;
        self.balance(holed, "2026-10-26", 6_666_666).await;
        self.hole(holed).await;
        let old = self.college("Old fund", "USD", Some("Child C")).await;
        self.balance(old, "2020-01-06", 5_555_555).await;
        let wrong = self
            .holding_with(
                "Wrong fund",
                "education",
                "USD",
                json!({"beneficiary": "Child D", "liquidity": "long_term_liability"}),
            )
            .await;
        self.balance(wrong, "2026-10-26", 4_444_444).await;
        let closed = self.college("Closed fund", "USD", Some("Child E")).await;
        self.balance(closed, "2026-10-26", -3_333_333).await;
        self.at("2026-10-30");
        self.patch_holding(closed, &json!({"closed": true})).await;
        self.at(TODAY);
        let gift = self
            .holding_with("Giving", "charitable", "JPY", json!({}))
            .await;
        self.balance(gift, "2026-10-27", 2_222_222).await;
    }
}

/// The golden household's whole answer, with six buckets of every kind of trouble beside it.
#[tokio::test]
async fn every_kind_of_bucket_changes_nothing_at_the_top() {
    let world = world().await;
    world.golden().await;
    let before = world.report(WEEK).await;
    assert_eq!(before["buckets"], json!([]));
    assert_eq!(totals(&before), golden_totals());

    world.every_kind_of_bucket().await;

    let report = world.report(WEEK).await;
    assert_eq!(buckets(&report).len(), 6);
    assert_eq!(top_level(&report), top_level(&before));
    assert_eq!(report["status"], "firm");
}

/// Each bucket's trouble stays its own, and says the kind it found.
#[tokio::test]
async fn a_bucket_s_cautions_are_its_own() {
    let world = world().await;
    world.golden().await;
    world.rates("JPY", &[("2026-10-27", "175.68")]).await;

    world.every_kind_of_bucket().await;

    let report = world.report(WEEK).await;
    let kinds: Vec<(&str, Vec<&str>)> = buckets(&report)
        .iter()
        .map(|bucket| {
            (
                bucket["status"].as_str().unwrap(),
                bucket["cautions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|caution| caution["kind"].as_str().unwrap())
                    .collect(),
            )
        })
        .collect();
    assert_eq!(
        kinds,
        [
            ("provisional", vec!["rate_missing"]),
            ("provisional", vec!["daily_value_missing"]),
            ("provisional", vec!["balance_stale"]),
            ("provisional", vec!["liability_balance_positive"]),
            ("provisional", vec!["closed_account_owing"]),
            ("firm", vec![]),
        ]
    );
    assert_eq!(report["cautions"], json!([]));
}

/// The household's own trouble in reverse: the bucket answers as it did.
#[tokio::test]
async fn a_household_s_trouble_changes_nothing_in_a_bucket() {
    let world = world().await;
    let golden = world.golden().await;
    let fund = world.college("College fund", "USD", Some("Child A")).await;
    world.balance(fund, "2026-10-26", 2_000_000).await;
    let before = world.report(WEEK).await;

    let baht = world.holding("Baht", "THB").await;
    world.balance(baht, "2026-10-26", 50_000).await;
    world.hole(golden.checking).await;

    let report = world.report(WEEK).await;
    assert_eq!(report["status"], "provisional");
    assert_eq!(report["points"][0]["total"], Value::Null);
    assert_eq!(report["buckets"], before["buckets"]);
    assert_eq!(report["buckets"][0]["status"], "firm");
    assert_eq!(report["buckets"][0]["points"][0]["total"], usd(2_000_000));
}

/// A token seeing only the household's accounts has no way to tell any bucket exists.
#[tokio::test]
async fn a_hidden_bucket_changes_no_byte() {
    let world = world().await;
    let golden = world.golden().await;
    let token = world.sees_only(golden.checking).await;
    let before = world.report_as(&token, WEEK).await;
    assert_eq!(before.0, StatusCode::OK);

    world.every_kind_of_bucket().await;

    assert!(
        world.report_as(&token, WEEK).await == before,
        "byte-identical"
    );
    assert_eq!(parsed(&before.1)["buckets"], json!([]));
    assert_eq!(buckets(&world.report(WEEK).await).len(), 6);
}

/// A second account behind a token's own bucket: its label's other holder, whatever it holds.
#[tokio::test]
async fn a_hidden_account_in_a_seen_bucket_changes_no_byte() {
    let world = world().await;
    world.display("USD").await;
    world.rates("USD", &[("2026-10-26", "1.1188")]).await;
    let seen = world.college("Seen fund", "USD", Some("Child A")).await;
    world.balance(seen, "2026-10-26", 100_000).await;
    let token = world.sees_only(seen).await;
    let before = world.report_as(&token, WEEK).await;

    for (name, currency, on) in [
        ("Sibling baht", "THB", "2026-10-26"),
        ("Sibling yen", "JPY", "2020-01-06"),
        ("Sibling dollars", "USD", "1999-01-04"),
    ] {
        let sibling = world.college(name, currency, Some("Child A")).await;
        world.balance(sibling, on, 5).await;
        world.hole(sibling).await;
    }
    let other_child = world.college("Other", "USD", Some("Child B")).await;
    world.balance(other_child, "2026-10-26", 5).await;

    let after = world.report_as(&token, WEEK).await;
    assert!(after == before, "byte-identical");
    assert_eq!(names(&parsed(&after.1)["buckets"][0]), ["Seen fund"]);
    for word in ["Sibling", "THB", "JPY", "Child B"] {
        assert!(!after.1.contains(word), "a hidden word shows");
    }
}

/// It sums what it sees, unmarked: a bucket of one where the admin sees two.
#[tokio::test]
async fn a_token_sums_only_what_it_sees_of_a_bucket() {
    let world = world().await;
    world.display("USD").await;
    let seen = world.college("Seen fund", "USD", Some("Child A")).await;
    world.balance(seen, "2026-11-01", 100_000).await;
    let sibling = world.college("Sibling fund", "USD", Some("Child A")).await;
    world.balance(sibling, "2026-11-01", 700).await;
    let token = world.sees_only(seen).await;

    let (_, text) = world.report_as(&token, ONE_DAY).await;
    let full = world.report(ONE_DAY).await;

    let report = parsed(&text);
    assert_eq!(keys(&report), [(json!("someone_else"), json!("Child A"))]);
    assert_eq!(names(&report["buckets"][0]), ["Seen fund"]);
    assert_eq!(report["buckets"][0]["points"][0]["total"], usd(100_000));
    assert_eq!(full["buckets"][0]["points"][0]["total"], usd(100_700));
    assert_eq!(names(&full["buckets"][0]), ["Seen fund", "Sibling fund"]);
}

/// A token holding no bucket account sees no label and no sum, while the admin sees a bucket.
#[tokio::test]
async fn a_token_sees_a_bucket_only_from_its_own_grants() {
    let world = world().await;
    let golden = world.golden().await;
    let fund = world.college("College fund", "USD", Some("Child A")).await;
    world.balance(fund, "2026-10-26", 2_000_000).await;

    let token = world.sees_only(golden.side).await;
    let (_, text) = world.report_as(&token, WEEK).await;

    assert!(!text.contains("Child A"), "a hidden label shows");
    assert!(!text.contains("2000000"), "a hidden sum shows");
    assert_eq!(parsed(&text)["buckets"], json!([]));
    assert_eq!(buckets(&world.report(WEEK).await).len(), 1);
}

/// Only a bucket visible: nothing of the household shows, and the token's top level says no
/// more than a household with none would.
#[tokio::test]
async fn a_token_on_a_bucket_alone_has_no_household_number() {
    let world = world().await;
    world.golden().await;
    let fund = world.college("College fund", "USD", Some("Child A")).await;
    world.balance(fund, "2026-10-26", 2_000_000).await;

    let token = world.sees_only(fund).await;
    let (_, text) = world.report_as(&token, WEEK).await;

    let report = parsed(&text);
    assert_eq!(report["status"], "unavailable");
    assert_eq!(report["blockers"], json!([{"kind": "no_counted_accounts"}]));
    assert_eq!(report["points"], json!([]));
    assert_eq!(names(&report), Vec::<&str>::new());
    assert_eq!(report["buckets"][0]["status"], "firm");
    assert!(!text.contains("Checking"), "a hidden name shows");
}

/// A bucket too large to sum refuses the report for whoever sees it, and for nobody else.
#[tokio::test]
async fn a_hidden_bucket_too_large_changes_no_byte() {
    let world = world().await;
    let golden = world.golden().await;
    let token = world.sees_only(golden.checking).await;
    let before = world.report_as(&token, WEEK).await;

    for name in ["Huge", "Vast"] {
        let fund = world.college(name, "USD", Some("Child A")).await;
        world.balance(fund, "2026-10-26", i64::MAX).await;
    }

    let (admin, _) = world.report_as(&world.admin, WEEK).await;
    assert_eq!(admin, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        world.report_as(&token, WEEK).await == before,
        "byte-identical"
    );
}

#[tokio::test]
async fn a_bound_529_reaches_its_bucket_not_net_worth() {
    const RANGE: &str = "from=2026-10-29&to=2026-11-02&interval=day";
    let feed = feed().await;
    let world = &feed.world;
    world.at("2026-10-29");
    world.rebuild("2026-10-29").await;
    world.display("USD").await;
    let checking = world.holding("Checking", "USD").await;
    world.balance(checking, "2026-10-29", 1_000).await;
    for (on, raw) in [
        ("2026-10-30", "4242.17"),
        ("2026-10-31", "11.00"),
        ("2026-11-01", "12.00"),
    ] {
        feed.sync(on, "USD", raw).await;
        world.fill(on).await;
    }
    world.at(TODAY);
    world.fill(TODAY).await;
    let before = world.report(RANGE).await;
    let target = world.college("College fund", "USD", Some("Child A")).await;

    feed.bind(target).await;

    let report = world.report(RANGE).await;
    assert_eq!(top_level(&report), top_level(&before));
    let bucket = &report["buckets"][0];
    assert_eq!(
        totals(bucket),
        [
            ("2026-10-30".to_owned(), usd(424_217)),
            ("2026-10-31".to_owned(), usd(1_100)),
            ("2026-11-01".to_owned(), usd(1_200)),
            (TODAY.to_owned(), usd(1_200)),
        ]
    );
    assert_eq!(first_listed(bucket)["first_on"], "2026-10-30");
    assert_eq!(bucket["status"], "firm");
}

#[tokio::test]
async fn the_answer_has_the_documented_shape() {
    let world = world().await;
    world.display("USD").await;
    assert_eq!(world.report(ONE_DAY).await["buckets"], json!([]));

    let fund = world.college("College fund", "USD", Some("Child A")).await;
    world.balance(fund, "2026-11-01", 2_000_000).await;

    let report = world.report(ONE_DAY).await;
    assert_eq!(
        report["buckets"],
        json!([{
            "belongs_to": "someone_else",
            "beneficiary": "Child A",
            "status": "firm",
            "points": [{
                "on": TODAY,
                "period_start": TODAY,
                "partial": false,
                "total": usd(2_000_000),
                "by_liquidity": [
                    {"liquidity": "invested", "amount": usd(2_000_000), "manual_rate": false},
                ],
                "by_currency": [usd(2_000_000)],
                "unconverted": [],
                "manual_rate": false,
            }],
            "accounts": [{
                "account": fund,
                "name": "College fund",
                "liquidity": "invested",
                "currency": "USD",
                "counted": true,
                "not_counted": null,
                "first_on": "2026-11-01",
                "closed_on": null,
                "points": [{
                    "on": TODAY,
                    "balance": usd(2_000_000),
                    "balance_on": "2026-11-01",
                    "balance_source": "manual",
                    "counted": true,
                    "amount": usd(2_000_000),
                    "rate_on": null,
                    "manual_rate": false,
                    "conversion": "same",
                }],
            }],
            "blockers": [],
            "cautions": [],
            "manual_rates": [],
        }])
    );
}

/// No label is `null`, not missing, and `given_away` follows every `someone_else`.
#[tokio::test]
async fn buckets_come_in_their_fixed_order() {
    let world = world().await;
    world.display("USD").await;
    for (name, kind, extra) in [
        ("Giving", "charitable", json!({})),
        ("Spelled", "education", json!({"beneficiary": "child a"})),
        ("Fund", "education", json!({"beneficiary": "Child A"})),
        ("Unnamed", "education", json!({})),
    ] {
        let one = world.holding_with(name, kind, "USD", extra).await;
        world.balance(one, "2026-11-01", 100).await;
    }

    let report = world.report(ONE_DAY).await;

    assert_eq!(
        keys(&report),
        [
            (json!("someone_else"), Value::Null),
            (json!("someone_else"), json!("Child A")),
            (json!("someone_else"), json!("child a")),
            (json!("given_away"), Value::Null),
        ]
    );
}

#[tokio::test]
async fn changing_belongs_to_writes_no_row_and_moves_the_answer() {
    let world = world().await;
    world.display("USD").await;
    let moved = world.holding("Moved", "USD").await;
    world.balance(moved, "2026-10-26", 100_000).await;
    let kept = world.holding("Kept", "USD").await;
    world.balance(kept, "2026-10-26", 700).await;
    let rows = world.rows().await;
    assert_eq!(world.report(WEEK).await["points"][0]["total"], usd(100_700));

    world
        .patch_holding(
            moved,
            &json!({"belongs_to": "someone_else", "beneficiary": "Child A"}),
        )
        .await;

    assert!(world.rows().await == rows, "no row changed");
    let report = world.report(WEEK).await;
    assert_eq!(names(&report), ["Kept"]);
    for (_, total) in totals(&report) {
        assert_eq!(total, usd(700));
    }
    assert_eq!(names(&report["buckets"][0]), ["Moved"]);
    for (_, total) in totals(&report["buckets"][0]) {
        assert_eq!(total, usd(100_000));
    }

    world
        .patch_holding(moved, &json!({"belongs_to": "household"}))
        .await;

    assert!(world.rows().await == rows, "no row changed");
    let back = world.report(WEEK).await;
    assert_eq!(back["buckets"], json!([]));
    assert_eq!(back["points"][0]["total"], usd(100_700));
}

/// Answers each currency with one rate on 10-30, as the ECB would.
struct OneRate;

impl RateProvider for OneRate {
    fn source(&self) -> FxSource {
        FxSource::Ecb
    }

    fn fetch(
        &self,
        currency: Currency,
        _since: Option<Date>,
        _today: Date,
    ) -> ProviderFuture<'_, Result<Vec<RateObservation>, RateError>> {
        let rate = if currency.as_str() == "THB" {
            "36.5"
        } else {
            "1.1172"
        };
        Box::pin(async move {
            Ok(vec![RateObservation {
                on: day("2026-10-30"),
                rate: rate.parse().unwrap(),
            }])
        })
    }
}

/// N7: a currency only a bucket holds is fetched like any other, and what it answers is in the
/// display currency. 10,000 satang at 36.5 baht and 1.1172 dollars a euro: 10,000 / 36.5 *
/// 1.1172 = 306.08, so USD 3.06.
#[tokio::test]
async fn a_bucket_in_another_currency_converts() {
    let world = world().await;
    world.display("USD").await;
    let fund = world.college("Baht fund", "THB", Some("Child A")).await;
    world.balance(fund, "2026-11-01", 10_000).await;
    let before = world.report(ONE_DAY).await;
    assert_eq!(before["buckets"][0]["points"][0]["total"], Value::Null);
    let fx = FxService::new(
        world.server.store.clone(),
        world.server.clock.clone(),
        world.server.config.timezone.clone(),
        Some(Arc::new(OneRate) as Arc<dyn RateProvider>),
    );

    fx.fetch_due().await.unwrap();

    let report = world.report(ONE_DAY).await;
    let bucket = &report["buckets"][0];
    assert_eq!(bucket["status"], "firm");
    assert_eq!(bucket["points"][0]["total"], usd(306));
    assert_eq!(
        bucket["points"][0]["by_currency"],
        json!([money(10_000, "THB")])
    );
    assert_eq!(first_listed(bucket)["points"][0]["rate_on"], "2026-10-30");
}

/// `all` at `balances` is the admin's answer, buckets and all.
#[tokio::test]
async fn an_all_token_sees_what_the_admin_sees() {
    let world = world().await;
    world.golden().await;
    world.every_kind_of_bucket().await;
    let token = world
        .token(
            coffer_domain::identity::Scope::Read,
            vec![super::all(coffer_domain::policy::AccountLevel::Balances)],
        )
        .await;

    let (status, text) = world.report_as(&token, WEEK).await;

    assert_eq!(status, StatusCode::OK);
    let report = parsed(&text);
    assert_eq!(buckets(&report).len(), 6);
    assert_eq!(report, world.report(WEEK).await);
}
