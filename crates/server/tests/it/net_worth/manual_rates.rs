//! Exchange rates an admin enters (spec §8.10 N16): what they unblock, how they are marked,
//! who may enter one, and what happens beside a fetch.

use axum::http::{Method, StatusCode};
use coffer_domain::audit::{AuditActor, AuditValue};
use coffer_domain::identity::Scope;
use coffer_domain::policy::AccountLevel::{Balances, Write};
use coffer_domain::time::Clock as _;
use coffer_store::{AuditEvent, AuditFilter, PageRequest};
use serde_json::{Value, json};

use super::{World, all, of_kind, on, point, rate_missing, series, world};

const DAY_REPORT: &str = "from=2026-10-26&to=2026-11-02&interval=day";
const REFUSAL: &str = "this action needs balances access to every account";

fn rate_uri(currency: &str, on: &str) -> String {
    format!("/api/v1/fx/manual-rates/{currency}/{on}")
}

impl World {
    /// `count` rates of one currency, a day apart from 2020-01-01, entered at once.
    pub(crate) async fn many_manual_rates(&self, count: i64) {
        let start: jiff::civil::Date = "2020-01-01".parse().unwrap();
        let mut tx = self.server.store.begin().await.unwrap();
        for days in 0..count {
            let on = start.checked_add(jiff::Span::new().days(days)).unwrap();
            tx.set_manual_rate(
                coffer_domain::money::Currency::parse("XAF").unwrap(),
                on,
                "655.957".parse().unwrap(),
                self.server.clock.now(),
            )
            .await
            .unwrap();
        }
        tx.commit().await.unwrap();
    }

    async fn enter(&self, currency: &str, on: &str, rate: &str) -> Value {
        self.admin_send(Method::PUT, &rate_uri(currency, on), &json!({"rate": rate}))
            .await
    }

    async fn remove(&self, currency: &str, on: &str) {
        let (status, _) = self
            .send(Method::DELETE, &rate_uri(currency, on), &self.admin, None)
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }

    async fn rate_events(&self) -> Vec<AuditEvent> {
        self.server
            .store
            .list_audit(
                &AuditFilter {
                    entity_type: Some("fx_manual_rate".into()),
                    ..AuditFilter::default()
                },
                &PageRequest::default(),
            )
            .await
            .unwrap_or_else(|_| panic!("reading the log failed"))
            .items
    }

    /// An account in a currency nobody publishes beside one in the display currency.
    async fn household_with_xaf(&self) -> coffer_domain::ids::AccountId {
        let plain = self.holding("Plain", "EUR").await;
        self.balance(plain, "2026-10-26", 10_000).await;
        let held = self.holding("Abroad", "XAF").await;
        self.balance(held, "2026-10-26", 655_957).await;
        self.display("EUR").await;
        held
    }
}

fn parsed(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

#[tokio::test]
async fn a_rate_gives_a_total_and_everything_built_from_it_says_manual() {
    let world = world().await;
    let held = world.household_with_xaf().await;
    let before = world.report(DAY_REPORT).await;
    assert_eq!(point(&before, "2026-10-30")["total"], Value::Null);
    assert_eq!(point(&before, "2026-10-30")["manual_rate"], false);

    let answer = world.enter("XAF", "2026-10-26", "655.9570").await;

    assert_eq!(answer["rate"], "655.957", "canonical");
    let report = world.report(DAY_REPORT).await;
    let day = point(&report, "2026-10-30");
    assert_eq!(day["total"]["minor_units"], 110_000);
    assert_eq!(day["manual_rate"], true);
    assert_eq!(day["by_liquidity"][0]["manual_rate"], true);
    assert_eq!(day["unconverted"], json!([]));
    let account_point = series(&report, held)["points"]
        .as_array()
        .unwrap()
        .iter()
        .find(|one| one["on"] == "2026-10-30")
        .unwrap();
    assert_eq!(account_point["manual_rate"], true);
    assert_eq!(account_point["rate_on"], "2026-10-26");
    assert_eq!(
        report["manual_rates"],
        json!([{"currency": "XAF", "first_on": "2026-10-26", "last_on": "2026-11-02"}])
    );
    assert_eq!(
        report["status"], "firm",
        "a fresh rate is a note, not a caution"
    );
    assert_eq!(report["cautions"], json!([]));
}

#[tokio::test]
async fn rates_coverage_and_the_list_mark_it() {
    let world = world().await;
    world.rates("USD", &[("2026-10-27", "1.1")]).await;
    world.enter("USD", "2026-10-26", "1.2").await;
    world.enter("XAF", "2026-10-26", "655.957").await;

    let (_, rates) = world
        .send(
            Method::GET,
            "/api/v1/fx/rates?currency=USD&from=2026-10-26&to=2026-10-30",
            &world.admin,
            None,
        )
        .await;
    assert_eq!(
        parsed(&rates)["items"],
        json!([
            {"on": "2026-10-27", "rate": "1.1", "manual": false},
            {"on": "2026-10-26", "rate": "1.2", "manual": true},
        ])
    );
    let (_, coverage) = world
        .send(Method::GET, "/api/v1/fx/coverage", &world.admin, None)
        .await;
    assert_eq!(
        parsed(&coverage)["manual"],
        json!([
            {"currency": "USD", "first_on": "2026-10-26", "last_on": "2026-10-26"},
            {"currency": "XAF", "first_on": "2026-10-26", "last_on": "2026-10-26"},
        ])
    );
    let usd = parsed(&coverage)["items"][0].clone();
    assert_eq!(
        (usd["first_on"].clone(), usd["last_on"].clone()),
        (json!("2026-10-27"), json!("2026-10-27")),
        "what a fetch is asked for is decided by fetched rates alone"
    );
    let (_, list) = world
        .send(Method::GET, "/api/v1/fx/manual-rates", &world.admin, None)
        .await;
    let items = parsed(&list)["items"].clone();
    assert_eq!(items[0]["currency"], "USD");
    assert_eq!(items[1]["rate"], "655.957");
    assert_eq!(items.as_array().unwrap().len(), 2);
}

/// The rule of N16: never overwritten, wins its own day, gives way to a later day.
#[tokio::test]
async fn a_manual_rate_beats_a_fetch_on_its_day_and_gives_way_to_a_later_one() {
    let world = world().await;
    let held = world.holding("Abroad", "USD").await;
    world.balance(held, "2026-10-26", 100_000).await;
    world.display("EUR").await;
    world.enter("USD", "2026-10-27", "2").await;

    world
        .rates(
            "USD",
            &[
                ("2026-10-26", "1"),
                ("2026-10-27", "1"),
                ("2026-10-29", "4"),
            ],
        )
        .await;

    let report = world.report(DAY_REPORT).await;
    let amount = |on: &str| point(&report, on)["total"]["minor_units"].clone();
    assert_eq!(amount("2026-10-26"), 100_000, "a fetched rate before it");
    assert_eq!(amount("2026-10-27"), 50_000, "it wins on its day");
    assert_eq!(amount("2026-10-28"), 50_000, "and is carried forward");
    assert_eq!(amount("2026-10-29"), 25_000, "a later rate takes over");
    assert_eq!(point(&report, "2026-10-28")["manual_rate"], true);
    assert_eq!(point(&report, "2026-10-29")["manual_rate"], false);
    assert_eq!(point(&report, "2026-10-26")["manual_rate"], false);
}

#[tokio::test]
async fn removing_a_rate_falls_back_on_what_it_covered() {
    let world = world().await;
    let held = world.holding("Abroad", "USD").await;
    world.balance(held, "2026-10-26", 100_000).await;
    world.display("EUR").await;
    world.rates("USD", &[("2026-10-26", "1")]).await;
    world.enter("USD", "2026-10-26", "1.25").await;
    let entered = world.report(DAY_REPORT).await;
    assert_eq!(
        point(&entered, "2026-10-26")["total"]["minor_units"],
        80_000
    );

    world.remove("USD", "2026-10-26").await;

    let report = world.report(DAY_REPORT).await;
    assert_eq!(
        point(&report, "2026-10-26")["total"]["minor_units"],
        100_000
    );
    assert_eq!(point(&report, "2026-10-26")["manual_rate"], false);
    assert_eq!(report["manual_rates"], json!([]));
}

#[tokio::test]
async fn removing_the_only_rate_blocks_the_total_again() {
    let world = world().await;
    world.household_with_xaf().await;
    world.enter("XAF", "2026-10-26", "655.957").await;

    world.remove("XAF", "2026-10-26").await;

    let report = world.report(DAY_REPORT).await;
    assert_eq!(point(&report, "2026-10-30")["total"], Value::Null);
    assert_eq!(rate_missing(&report, "XAF")["reason"], "not_fetched_yet");
}

#[tokio::test]
async fn removing_what_is_not_there_is_404_and_writes_nothing() {
    let world = world().await;
    let before = world.audit_count().await;

    let (status, _) = world
        .send(
            Method::DELETE,
            &rate_uri("XAF", "2026-10-26"),
            &world.admin,
            None,
        )
        .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(world.audit_count().await, before);
}

#[tokio::test]
async fn each_change_is_audited_with_old_and_new_and_the_token_behind_it() {
    let world = world().await;

    world.enter("XAF", "2026-10-26", "655.957").await;
    world.enter("XAF", "2026-10-26", "655.957").await;
    world.enter("XAF", "2026-10-26", "660").await;
    world.remove("XAF", "2026-10-26").await;

    let logged = world.rate_events().await;
    assert_eq!(logged.len(), 3, "the same rate again writes nothing");
    let rate_of = |event: &AuditEvent| {
        let [(field, change)] = event.changes.iter().collect::<Vec<_>>()[..] else {
            panic!("expected one field");
        };
        assert_eq!(field, "rate");
        (change.before.clone(), change.after.clone())
    };
    let find = |action: &str, before: AuditValue| {
        logged
            .iter()
            .find(|event| event.action == action && rate_of(event).0 == before)
            .unwrap_or_else(|| panic!("no such entry"))
    };
    let first = find("fx.manual_rate.set", AuditValue::Null);
    assert_eq!(rate_of(first).1, AuditValue::from("655.957"));
    assert_eq!(first.entity_id, "XAF:2026-10-26");
    assert_eq!(
        first.actor,
        AuditActor::Member {
            member: world.alice.member,
            agent: Some(world.admin_id),
        }
    );
    let second = find("fx.manual_rate.set", AuditValue::from("655.957"));
    assert_eq!(rate_of(second).1, AuditValue::from("660"));
    let gone = find("fx.manual_rate.remove", AuditValue::from("660"));
    assert_eq!(rate_of(gone).1, AuditValue::Null);
}

#[tokio::test]
async fn a_wrong_rate_is_refused_whole() {
    let world = world().await;
    world.display("USD").await;
    let before = world.audit_count().await;

    for (rate, wanted) in [
        (json!("0"), StatusCode::BAD_REQUEST),
        (json!("0.000000"), StatusCode::BAD_REQUEST),
        (json!("-1"), StatusCode::BAD_REQUEST),
        (json!("+1"), StatusCode::BAD_REQUEST),
        (json!("1e3"), StatusCode::BAD_REQUEST),
        (json!("NaN"), StatusCode::BAD_REQUEST),
        (json!("Infinity"), StatusCode::BAD_REQUEST),
        (json!(" 1"), StatusCode::BAD_REQUEST),
        (json!("1,5"), StatusCode::BAD_REQUEST),
        (json!("1.1234567"), StatusCode::BAD_REQUEST),
        (json!("12345678901"), StatusCode::BAD_REQUEST),
        (json!(""), StatusCode::BAD_REQUEST),
        (json!(1.5), StatusCode::BAD_REQUEST),
        (Value::Null, StatusCode::BAD_REQUEST),
    ] {
        let (status, text) = world
            .send(
                Method::PUT,
                &rate_uri("XAF", "2026-10-26"),
                &world.admin,
                Some(&json!({"rate": rate})),
            )
            .await;
        assert_eq!(status, wanted, "{rate}");
        assert!(!text.contains("1234567"), "does not echo");
    }
    for (currency, day, wanted) in [
        ("ZZQ", "2026-10-26", StatusCode::BAD_REQUEST),
        ("xaf", "2026-10-26", StatusCode::BAD_REQUEST),
        ("XAF", "2026-10-26T00:00:00Z", StatusCode::BAD_REQUEST),
        ("XAF", "2026-13-01", StatusCode::BAD_REQUEST),
        ("EUR", "2026-10-26", StatusCode::UNPROCESSABLE_ENTITY),
        ("USD", "2026-10-26", StatusCode::UNPROCESSABLE_ENTITY),
        ("XAF", "2026-11-03", StatusCode::UNPROCESSABLE_ENTITY),
        ("XAF", "1969-12-31", StatusCode::UNPROCESSABLE_ENTITY),
    ] {
        let (status, _) = world
            .send(
                Method::PUT,
                &rate_uri(currency, day),
                &world.admin,
                Some(&json!({"rate": "1.5"})),
            )
            .await;
        assert_eq!(status, wanted, "{currency} {day}");
    }

    assert_eq!(world.audit_count().await, before);
    assert_eq!(
        world
            .server
            .store
            .fx_coverage_with_manual()
            .await
            .unwrap()
            .1,
        []
    );
}

#[tokio::test]
async fn today_is_the_last_day_taking_a_rate() {
    let world = world().await;

    let answer = world.enter("XAF", super::TODAY, "655.957").await;

    assert_eq!(answer["on"], super::TODAY);
}

#[tokio::test]
async fn only_an_admin_seeing_every_account_may_write() {
    let world = world().await;
    let visible = world.holding("Visible", "USD").await;
    world.enter("XAF", "2026-10-26", "655.957").await;
    let before = world.rate_events().await.len();

    let cases = [
        (Scope::Read, Vec::new(), None),
        (Scope::Write, Vec::new(), None),
        (Scope::Admin, vec![on(visible, Balances)], Some(REFUSAL)),
        (Scope::Admin, vec![on(visible, Write)], Some(REFUSAL)),
    ];
    for (scope, grants, detail) in cases {
        let token = world.token(scope, grants).await;
        for (method, body) in [
            (Method::PUT, Some(json!({"rate": "2"}))),
            (Method::DELETE, None),
        ] {
            let (status, text) = world
                .send(
                    method,
                    &rate_uri("XAF", "2026-10-26"),
                    &token,
                    body.as_ref(),
                )
                .await;
            assert_eq!(status, StatusCode::FORBIDDEN);
            if let Some(detail) = detail {
                assert_eq!(parsed(&text)["detail"], detail);
            }
        }
    }
    assert_eq!(world.rate_events().await.len(), before);

    let allowed = world.token(Scope::Admin, vec![all(Balances)]).await;
    let (status, _) = world
        .send(
            Method::PUT,
            &rate_uri("XAF", "2026-10-27"),
            &allowed,
            Some(&json!({"rate": "2"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
}

/// Refused the same with or without a rate on file, like the other FX reads.
#[tokio::test]
async fn the_list_needs_all_at_balances_and_a_refusal_says_nothing() {
    let bare = world().await;
    let held = world().await;
    let in_bare = bare.holding("Visible", "USD").await;
    let in_held = held.holding("Visible", "USD").await;
    held.enter("XAF", "2026-10-26", "655.957").await;

    let for_bare = bare.sees_only(in_bare).await;
    let for_held = held.sees_only(in_held).await;
    let uri = "/api/v1/fx/manual-rates";
    let refused = bare.send(Method::GET, uri, &for_bare, None).await;
    let also = held.send(Method::GET, uri, &for_held, None).await;

    assert_eq!(refused, also);
    assert_eq!(also.0, StatusCode::FORBIDDEN);
}

/// N2: nothing of a manual rate on a currency a token doesn't see reaches it.
#[tokio::test]
async fn a_token_sees_no_mark_for_a_currency_it_does_not_hold() {
    let world = world().await;
    world.household_with_xaf().await;
    let plain = world.holding("Euros", "EUR").await;
    world.balance(plain, "2026-10-26", 100).await;
    let token = world.sees_only(plain).await;
    let (_, before) = world.report_as(&token, DAY_REPORT).await;

    world.enter("XAF", "2026-10-26", "655.957").await;

    let (_, after) = world.report_as(&token, DAY_REPORT).await;
    assert_eq!(before, after);
    assert!(!after.contains("manual_rate\":true"));
}

/// A rate older than a week is old whoever entered it.
#[tokio::test]
async fn an_old_manual_rate_raises_rate_stale() {
    let world = world().await;
    world.household_with_xaf().await;
    world.enter("XAF", "2026-10-01", "655.957").await;

    let report = world.report(DAY_REPORT).await;

    let stale = of_kind(&report, "cautions", "rate_stale");
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0]["currency"], "XAF");
    assert_eq!(stale[0]["rate_on"], "2026-10-01");
    assert_eq!(report["status"], "provisional", "as any old rate would");
    assert_eq!(report["manual_rates"][0]["currency"], "XAF");
    assert!(point(&report, "2026-10-30")["total"].is_object());
}

/// A day before the first manual rate has no rate, and the reason isn't the fetch's.
#[tokio::test]
async fn a_balance_older_than_the_rate_stays_without_a_total() {
    let world = world().await;
    world.household_with_xaf().await;
    world
        .fetch_failed("XAF", coffer_domain::fx::FxFetchStatus::NotPublished, None)
        .await;
    world.enter("XAF", "2026-10-28", "655.957").await;

    let report = world.report(DAY_REPORT).await;

    assert_eq!(point(&report, "2026-10-27")["total"], Value::Null);
    assert_eq!(
        point(&report, "2026-10-28")["total"]["minor_units"],
        110_000
    );
    assert_eq!(rate_missing(&report, "XAF")["reason"], "none_that_early");
}

/// A rate entered first and a display currency chosen later: the caution names the side.
#[tokio::test]
async fn a_manual_rate_on_the_display_side_is_named() {
    let world = world().await;
    let held = world.holding("Euros", "EUR").await;
    world.balance(held, "2026-10-26", 10_000).await;
    world.enter("XAF", "2026-10-26", "655.957").await;

    world.display("XAF").await;

    let report = world.report(DAY_REPORT).await;
    assert_eq!(report["manual_rates"][0]["currency"], "XAF");
    assert_eq!(point(&report, "2026-10-30")["total"]["minor_units"], 65_596);
    let (status, _) = world
        .send(
            Method::DELETE,
            &rate_uri("XAF", "2026-10-26"),
            &world.admin,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "still removable");
}

/// A rate written the wrong way round, far enough from 1 to tell: refused, showing both numbers.
#[tokio::test]
async fn a_rate_far_from_a_published_one_needs_confirming() {
    let world = world().await;
    world.rates("USD", &[("2026-10-20", "1.75")]).await;
    let uri = rate_uri("USD", "2026-10-26");

    let (status, text) = world
        .send(
            Method::PUT,
            &uri,
            &world.admin,
            Some(&json!({"rate": "0.92"})),
        )
        .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let problem = parsed(&text);
    assert_eq!(problem["code"], "unusual_rate");
    let detail = problem["detail"].as_str().unwrap();
    assert!(detail.contains("0.92") && detail.contains("1.75") && detail.contains("2026-10-20"));
    assert_eq!(world.rate_events().await.len(), 0, "nothing written");

    let answer = world
        .admin_send(
            Method::PUT,
            &uri,
            &json!({"rate": "0.92", "confirm_unusual": true}),
        )
        .await;

    assert_eq!(answer["rate"], "0.92");
    let logged = world.rate_events().await;
    let change = logged[0]
        .changes
        .iter()
        .find(|(field, _)| *field == "confirmed_against_published")
        .map(|(_, change)| change.after.clone());
    assert_eq!(change, Some(AuditValue::from("1.75")), "the log says so");
}

/// Within 1.5 times passes unasked, and confirming a rate that needs none adds nothing.
#[tokio::test]
async fn a_rate_near_a_published_one_needs_no_confirming() {
    let world = world().await;
    world.rates("USD", &[("2026-10-26", "2")]).await;

    world.enter("USD", "2026-10-26", "3").await;
    world
        .admin_send(
            Method::PUT,
            &rate_uri("USD", "2026-10-25"),
            &json!({"rate": "2.5", "confirm_unusual": true}),
        )
        .await;

    for event in world.rate_events().await {
        assert_eq!(event.changes.iter().count(), 1);
    }
}

/// With no published rate near its day there is nothing to compare: no check, only the
/// form's words, whatever the entry.
#[tokio::test]
async fn with_no_published_rate_nearby_any_rate_passes() {
    let world = world().await;
    world.rates("USD", &[("2026-09-25", "2")]).await;

    world.enter("XAF", "2026-10-26", "0.001").await;
    world.enter("USD", "2026-10-26", "90").await;
    let (status, _) = world
        .send(
            Method::PUT,
            &rate_uri("USD", "2026-10-25"),
            &world.admin,
            Some(&json!({"rate": "90"})),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "30 days away is nearby"
    );
}

/// Only other manual rates near it: they are no check, or a wrong one could vouch for another.
#[tokio::test]
async fn a_manual_rate_is_not_compared_with_another_manual_rate() {
    let world = world().await;
    world.enter("XAF", "2026-10-25", "1").await;

    let answer = world.enter("XAF", "2026-10-26", "655.957").await;

    assert_eq!(answer["rate"], "655.957");
}

#[tokio::test]
async fn the_list_says_when_it_stops_short_and_goes_on_from_an_offset() {
    let world = world().await;
    world.many_manual_rates(1_001).await;
    let uri = "/api/v1/fx/manual-rates";

    let (_, first) = world.send(Method::GET, uri, &world.admin, None).await;
    let first = parsed(&first);
    assert_eq!(first["items"].as_array().unwrap().len(), 1_000);
    assert_eq!(first["truncated"], true);
    assert_eq!(first["items"][0]["on"], "2022-09-27", "the newest first");

    let (_, rest) = world
        .send(
            Method::GET,
            &format!("{uri}?offset=1000"),
            &world.admin,
            None,
        )
        .await;
    let rest = parsed(&rest);
    assert_eq!(rest["items"].as_array().unwrap().len(), 1);
    assert_eq!(rest["items"][0]["on"], "2020-01-01");
    assert_eq!(rest["truncated"], false);
}
