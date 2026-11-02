//! The one thing N2 lets cross the line, in a bucket: a rate is reference data the household stores
//! once, so a visible currency a hidden account caused to be fetched has rates sooner. These pin
//! exactly how far that reaches: the visible row of that currency, an amount and a date, and
//! never a count, a name, an amount of the hidden account or another currency.

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use coffer_domain::accounts::AccountKind;
use coffer_domain::fx::{FxFetchStatus, FxSource, RateObservation};
use coffer_domain::identity::Scope;
use coffer_domain::money::Currency;
use coffer_domain::policy::AccountLevel;
use coffer_server::fx::{FxService, RateError, RateProvider};
use coffer_server::sync::provider::ProviderFuture;
use jiff::civil::Date;
use serde_json::{Value, json};

use super::super::World;
use super::super::net_worth_channels::{HIDDEN_MINOR, RANGE, all, day};
use super::super::on;
use super::family::{
    BUCKET_MINOR, Family, HIDDEN_LABEL, HIDDEN_NAME, LABEL, SINCE, Setup, assert_twins,
    family_with, ids_by_first_use,
};

const IN_YEN: Setup = Setup {
    currency: "JPY",
    display: true,
    built: true,
};

async fn in_yen() -> Family {
    family_with(IN_YEN, async |_| {}).await
}

impl World {
    async fn hidden_in(&self, currency: &str, minor_units: i64) {
        let id = self
            .bucket_account(
                HIDDEN_NAME,
                AccountKind::Education,
                currency,
                None,
                Some(HIDDEN_LABEL),
            )
            .await;
        self.balance_on(id, SINCE, minor_units).await;
    }

    /// Fresh rates for every day of the range the tests read: one before it starts, one on its
    /// last day, so none is more than a week old.
    async fn fresh_rates(&self) {
        let days = ["2026-10-25", "2026-11-02"];
        self.rates("JPY", &[(days[0], "175.68"), (days[1], "175.68")])
            .await;
        self.rates("USD", &[(days[0], "1.1188"), (days[1], "1.1188")])
            .await;
    }
}

/// The report without the parts of Emma's bucket N2 lets a rate change.
fn without_what_a_rate_changes(mut report: Value) -> Value {
    let bucket = &mut report["buckets"][0];
    bucket["status"] = Value::Null;
    bucket["cautions"] = Value::Null;
    for point in bucket["points"].as_array_mut().unwrap() {
        point["total"] = Value::Null;
        point["by_liquidity"] = Value::Null;
        point["unconverted"] = Value::Null;
    }
    for point in bucket["accounts"][0]["points"].as_array_mut().unwrap() {
        point["amount"] = Value::Null;
        point["rate_on"] = Value::Null;
        point["conversion"] = Value::Null;
    }
    report
}

/// The same rates give the same bytes, whoever caused them: one hidden account in the visible
/// currency or two, of any size. So the rate shows no count and no amount.
#[tokio::test]
async fn a_rate_a_hidden_account_caused_reads_like_any_other() {
    let bare = in_yen().await;
    bare.world.fresh_rates().await;
    for hidden in [vec![HIDDEN_MINOR], vec![1, 2]] {
        let held = in_yen().await;
        for minor_units in hidden {
            held.world.hidden_in("JPY", minor_units).await;
        }
        held.world.fresh_rates().await;
        assert_eq!(
            held.report(&held.token).await["buckets"][0]["status"],
            "firm"
        );
        assert_twins(&bare, &held).await;
    }
}

/// N2's bit, whole: before any fetch a bucket in yen reads `not_fetched_yet`, and with rates a
/// hidden yen account caused it has an amount and a date. Nothing else in the report differs,
/// and nothing of the hidden account shows.
#[tokio::test]
async fn a_rate_reaches_only_the_visible_row_in_that_currency() {
    let (alone, caused) = (in_yen().await, in_yen().await);
    caused.world.hidden_in("JPY", HIDDEN_MINOR).await;
    caused.world.fresh_rates().await;

    let before = alone.report(&alone.token).await;
    let after = caused.report(&caused.token).await;
    assert_eq!(before["buckets"][0]["status"], "provisional");
    let reasons: Vec<(&str, &str)> = before["buckets"][0]["cautions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|one| {
            (
                one["currency"].as_str().unwrap(),
                one["reason"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        reasons,
        [("JPY", "not_fetched_yet"), ("USD", "not_fetched_yet")]
    );
    assert_eq!(after["buckets"][0]["status"], "firm");
    assert_eq!(after["buckets"][0]["cautions"], json!([]));
    let amount = &after["buckets"][0]["accounts"][0]["points"][0];
    assert_eq!(amount["conversion"], "converted");
    assert_eq!(amount["rate_on"], "2026-10-25");
    assert_eq!(amount["balance"]["minor_units"], BUCKET_MINOR);

    assert_eq!(
        ids_by_first_use(&without_what_a_rate_changes(before).to_string()),
        ids_by_first_use(&without_what_a_rate_changes(after.clone()).to_string()),
        "everything else is the same"
    );
    let text = after.to_string();
    for hidden in [HIDDEN_LABEL, HIDDEN_NAME, &HIDDEN_MINOR.to_string()] {
        assert!(!text.contains(hidden), "{hidden}");
    }

    // The household's own section never needed a rate, so it never moved.
    assert_eq!(after["status"], "firm");
}

/// A rate of a currency only a hidden account uses reaches nothing: the bucket in yen still reads
/// `not_fetched_yet`, and no answer holds the franc.
#[tokio::test]
async fn a_rate_for_a_hidden_only_currency_changes_no_byte() {
    let (bare, held) = (in_yen().await, in_yen().await);
    held.world.hidden_in("CHF", HIDDEN_MINOR).await;
    held.world.rates("CHF", &[("2026-10-20", "0.9412")]).await;
    assert_twins(&bare, &held).await;
    assert!(!held.text(RANGE, &held.token).await.contains("CHF"));
    assert!(held.text(RANGE, &held.admin).await.contains("CHF"));
}

/// A display currency's rate a hidden account caused is that same bit about a currency the token
/// sees: with only that rate held, the caution for the display currency goes, the one for the
/// visible currency stays, and neither names anything else.
#[tokio::test]
async fn a_display_rate_a_hidden_account_caused_is_that_same_bit() {
    let held = in_yen().await;
    held.world.hidden_in("CHF", HIDDEN_MINOR).await;
    held.world.rates("USD", &[("2026-10-20", "1.1188")]).await;
    let report = held.report(&held.token).await;
    assert_eq!(
        report["buckets"][0]["cautions"],
        json!([{
            "kind": "rate_missing",
            "currency": "JPY",
            "reason": "not_fetched_yet",
            "first_on": "2026-10-26",
            "last_on": "2026-11-02",
        }])
    );
    assert!(!report.to_string().contains("CHF"));
}

/// What a fetch's failure says of the visible currency is the same whoever caused the fetch, and
/// stays on the row needing it: N2's `fetch_failing`, `not_published` and an old `rate_on`.
#[tokio::test]
async fn a_failed_fetch_of_a_visible_currency_reads_alike_whoever_caused_it() {
    for (status, reason) in [
        (FxFetchStatus::Failing, "fetch_failing"),
        (FxFetchStatus::NotPublished, "not_published"),
    ] {
        let (bare, held) = (in_yen().await, in_yen().await);
        held.world.hidden_in("JPY", HIDDEN_MINOR).await;
        for scene in [&bare, &held] {
            scene.world.fetch_failed("JPY", status).await;
        }
        let report = held.report(&held.token).await;
        assert!(
            report["buckets"][0]["cautions"]
                .to_string()
                .contains(reason)
        );
        assert_eq!(report["status"], "firm", "the household's own is untouched");
        assert_twins(&bare, &held).await;
    }

    let (bare, held) = (in_yen().await, in_yen().await);
    held.world.hidden_in("JPY", HIDDEN_MINOR).await;
    for scene in [&bare, &held] {
        scene.world.rates("JPY", &[("2026-01-02", "175.68")]).await;
        scene.world.rates("USD", &[("2026-01-02", "1.1188")]).await;
        scene
            .world
            .fetch_failed("JPY", FxFetchStatus::Failing)
            .await;
    }
    let report = held.report(&held.token).await;
    assert!(
        report["buckets"][0]["cautions"]
            .to_string()
            .contains("rate_stale")
    );
    assert_twins(&bare, &held).await;
}

/// The endpoints that say which rates are stored, and the one change that could probe them, are
/// refused alike to a token without `all` at `balances`, with or without the hidden currency.
#[tokio::test]
async fn no_named_grant_reaches_what_says_which_rates_are_held() {
    let (bare, held) = (in_yen().await, in_yen().await);
    held.world.hidden_in("CHF", HIDDEN_MINOR).await;
    held.world.rates("CHF", &[("2026-10-20", "0.9412")]).await;
    let reads = [
        "/api/v1/fx/coverage",
        "/api/v1/fx/rates?currency=CHF&from=2026-01-01&to=2026-12-31",
    ];
    let mut answers = Vec::new();
    for scene in [&bare, &held] {
        let (_, token) = scene
            .world
            .token(
                Scope::Admin,
                vec![
                    on(scene.home(), AccountLevel::Write),
                    on(scene.emma(), AccountLevel::Write),
                ],
            )
            .await;
        let mut these = Vec::new();
        for uri in reads {
            let answer = scene.answer(uri, &token).await;
            assert_eq!(answer.1, StatusCode::FORBIDDEN);
            these.push((answer.1, answer.3));
        }
        let probe = json!({"display_currency": "CHF"});
        let (status, body) = scene
            .world
            .send(
                Method::PATCH,
                "/api/v1/household/settings",
                &token,
                Some(&probe),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        these.push((status, body));
        answers.push(these);
    }
    assert_eq!(answers[0], answers[1], "byte-identical");

    let (_, wide) = held
        .world
        .token(Scope::Read, vec![all(AccountLevel::Balances)])
        .await;
    let coverage = held.text("/api/v1/fx/coverage", &wide).await;
    assert!(coverage.contains("CHF"), "{coverage}");
    assert!(!coverage.contains(LABEL) && !coverage.contains(HIDDEN_LABEL));
}

/// Answers every currency with one rate, as the ECB would.
struct OneRate;

impl RateProvider for OneRate {
    fn source(&self) -> FxSource {
        FxSource::Ecb
    }

    fn fetch(
        &self,
        _currency: Currency,
        _since: Option<Date>,
        _today: Date,
    ) -> ProviderFuture<'_, Result<Vec<RateObservation>, RateError>> {
        Box::pin(async move {
            Ok(vec![RateObservation {
                on: day("2026-10-30"),
                rate: "1.5".parse().unwrap(),
            }])
        })
    }
}

async fn fetch_rates(world: &World) {
    FxService::new(
        world.server.store.clone(),
        world.server.clock.clone(),
        world.server.config.timezone.clone(),
        Some(Arc::new(OneRate) as Arc<dyn RateProvider>),
    )
    .fetch_due()
    .await
    .unwrap();
}

/// The real job, not rates stored by hand. A household in one currency fetches nothing, and a
/// hidden bucket in another makes it fetch both (N7): the rates held differ, and a token whose
/// rows need none of them reads the same.
#[tokio::test]
async fn a_fetch_only_a_hidden_bucket_caused_changes_no_byte() {
    let (bare, held) = (
        family_with(Setup::default(), async |_| {}).await,
        family_with(Setup::default(), async |_| {}).await,
    );
    held.world.hidden_in("CHF", HIDDEN_MINOR).await;
    for scene in [&bare, &held] {
        fetch_rates(&scene.world).await;
    }

    let coverage = |text: String| text.contains("CHF") && text.contains("USD");
    assert!(coverage(
        held.text("/api/v1/fx/coverage", &held.admin).await
    ));
    assert!(!coverage(
        bare.text("/api/v1/fx/coverage", &bare.admin).await
    ));
    assert_twins(&bare, &held).await;
}

/// The real job for a currency the token sees: its row needs the rates in both households, so
/// they are fetched with or without the hidden account, and the same rates give the same bytes.
/// That is why N2 accepts this: only the order could differ.
#[tokio::test]
async fn a_fetch_of_a_visible_currency_reads_alike_with_a_hidden_bucket() {
    let (bare, held) = (in_yen().await, in_yen().await);
    held.world.hidden_in("JPY", HIDDEN_MINOR).await;
    held.world.hidden_in("CHF", HIDDEN_MINOR).await;
    for scene in [&bare, &held] {
        fetch_rates(&scene.world).await;
    }
    let report = held.report(&held.token).await;
    let last = report["buckets"][0]["accounts"][0]["points"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(last["conversion"], "converted");
    assert_eq!(last["rate_on"], "2026-10-30");
    assert_twins(&bare, &held).await;
}
