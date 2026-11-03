//! Spec §8.10 N2 under attack: a token's net worth is the sum of what it can see, and
//! nothing in it depends on anything else. Each comparison is with a second household that
//! really lacks the hidden account, on the same clock and settings, compared whole: every
//! header and every byte of every read, with only the visible ids, which each household
//! generates for itself, given a name. Timing is out of scope.

use std::time::Duration;

use axum::http::{HeaderMap, StatusCode};
use coffer_domain::accounts::{AccountKind, LiquidityClass};
use coffer_domain::fx::{FxErrorCode, FxFetchStatus, FxSource, RateObservation};
use coffer_domain::identity::Scope;
use coffer_domain::ids::AccountId;
use coffer_domain::money::Currency;
use coffer_domain::policy::{AccountLevel, GrantTarget, TokenGrant};
use coffer_domain::time::Clock as _;
use coffer_server::ledger::accounts::{AccountPatch, NewAccountInput};
use coffer_server::secrets::SecretToken;
use jiff::Timestamp;
use jiff::civil::Date;
use serde_json::{Value, json};

use super::{World, grant, on, world};

pub(super) const TODAY: &str = "2026-11-02";
pub(super) const REPORT: &str = "/api/v1/reports/net-worth";
pub(super) const RANGE: &str =
    "/api/v1/reports/net-worth?from=2026-10-26&to=2026-11-02&interval=day";
pub(super) const HIDDEN_NAME: &str = "Quiet fund";
pub(super) const HIDDEN_MINOR: i64 = 7_346_195_280;
pub(super) const VISIBLE_MINOR: i64 = 100_000;

/// Every shape of report: days, weeks, months from the first day a row can have, without
/// points, past today and by default.
const REPORT_READS: [&str; 6] = [
    RANGE,
    "/api/v1/reports/net-worth?from=2026-01-01&interval=week",
    "/api/v1/reports/net-worth?from=1970-01-01&interval=month",
    "/api/v1/reports/net-worth?from=2026-10-26&interval=day&account_points=false",
    "/api/v1/reports/net-worth?from=2026-10-26&to=2026-12-31&interval=day",
    REPORT,
];

/// The other reads a restricted token might compare.
const OTHER_READS: [&str; 5] = [
    "/api/v1/accounts?include_closed=true",
    "/api/v1/household/settings",
    "/api/v1/fx/coverage",
    "/api/v1/fx/rates?currency=JPY&from=2026-01-01&to=2026-12-31",
    "/api/v1/fx/rates?currency=USD&from=2026-01-01&to=2026-12-31",
];

/// A whole answer, with its uri.
pub(super) type Answer = (String, StatusCode, HeaderMap, String);

fn noon(day: &str) -> Timestamp {
    format!("{day}T12:00:00Z").parse().unwrap()
}

pub(super) fn day(text: &str) -> Date {
    text.parse().unwrap()
}

pub(super) fn code(text: &str) -> Currency {
    Currency::parse(text).unwrap()
}

pub(super) fn money(minor_units: i64, currency: &str) -> Value {
    json!({"minor_units": minor_units, "currency": currency})
}

pub(super) fn all(level: AccountLevel) -> TokenGrant {
    grant(GrantTarget::All, level)
}

impl World {
    /// Sets today, as every household compared here must.
    pub(super) fn on_today(&self) {
        self.server.clock.set(noon(TODAY));
    }

    pub(super) async fn holding_as(
        &self,
        name: &str,
        kind: AccountKind,
        currency: &str,
        liquidity: Option<LiquidityClass>,
        counted: bool,
    ) -> AccountId {
        self.server
            .ledger
            .create_account(
                &self.alice,
                NewAccountInput {
                    name: name.into(),
                    kind,
                    currency: code(currency),
                    liquidity,
                    include_in_net_worth: Some(counted),
                    include_in_forecast: Some(true),
                    belongs_to: None,
                    beneficiary: None,
                    opening_balance: None,
                },
                None,
            )
            .await
            .unwrap()
            .id
    }

    pub(super) async fn holding(&self, name: &str, currency: &str) -> AccountId {
        self.holding_as(name, AccountKind::Checking, currency, None, true)
            .await
    }

    pub(super) async fn balance_on(&self, holder: AccountId, on_day: &str, minor_units: i64) {
        self.server
            .ledger
            .set_manual_balance(&self.alice, holder, day(on_day), minor_units)
            .await
            .unwrap();
    }

    pub(super) async fn display(&self, currency: &str) {
        self.server
            .ledger
            .update_forecast_settings(
                &self.alice,
                serde_json::from_value(json!({"display_currency": currency})).unwrap(),
            )
            .await
            .unwrap();
    }

    /// Stores `rates` of `currency`, as a fetch does.
    pub(super) async fn rates(&self, currency: &str, rates: &[(&str, &str)]) {
        let rates: Vec<RateObservation> = rates
            .iter()
            .map(|(on_day, rate)| RateObservation {
                on: day(on_day),
                rate: rate.parse().unwrap(),
            })
            .collect();
        let mut tx = self.server.store.begin().await.unwrap();
        tx.upsert_fx_rates(
            code(currency),
            FxSource::Ecb,
            &rates,
            self.server.clock.now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }

    /// Records that fetching `currency`'s rates gave none, as a fetch does.
    pub(super) async fn fetch_failed(&self, currency: &str, status: FxFetchStatus) {
        let error = (status == FxFetchStatus::Failing).then_some(FxErrorCode::Network);
        let mut tx = self.server.store.begin().await.unwrap();
        tx.record_fx_failure(code(currency), status, error, self.server.clock.now())
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    pub(super) async fn rebuild(&self, through: &str) {
        self.server
            .store
            .rebuild_net_worth(day(through))
            .await
            .unwrap();
    }

    async fn fill(&self, through: &str) {
        let mut tx = self.server.store.begin().await.unwrap();
        tx.fill_net_worth(day(through)).await.unwrap();
        tx.commit().await.unwrap();
    }

    pub(super) async fn close(&self, holder: AccountId) {
        self.server
            .ledger
            .update_account(
                &self.alice,
                holder,
                AccountPatch {
                    closed: Some(true),
                    ..AccountPatch::default()
                },
            )
            .await
            .unwrap();
    }

    /// Takes one day's row out of the table, as a bug or a lost write would.
    pub(super) async fn hole_today(&self, holder: AccountId) {
        self.server
            .store
            .execute_unchecked_for_tests(
                "DELETE FROM account_daily_values WHERE account_id = ? AND on_date = '2026-11-02'",
                &holder.to_string(),
            )
            .await
            .unwrap();
    }

    /// Rewrites its rows in another display currency, which the store refuses as corruption.
    pub(super) async fn row_in_chf(&self, holder: AccountId) {
        self.server
            .store
            .execute_unchecked_for_tests(
                "UPDATE account_daily_values SET display_currency = 'CHF' WHERE account_id = ?",
                &holder.to_string(),
            )
            .await
            .unwrap();
    }
}

/// One household: display USD, a visible USD account holding 1,000.00 since 2026-10-26, a
/// read token seeing only it at `balances`, and an admin token seeing everything.
pub(super) struct Scene {
    pub(super) world: World,
    pub(super) visible: AccountId,
    pub(super) token: SecretToken,
    pub(super) admin: SecretToken,
}

/// As [`scene`], with the table as the migration leaves it.
async fn unbuilt_scene() -> Scene {
    scene_over(world().await).await
}

pub(super) async fn scene_over(world: World) -> Scene {
    world.on_today();
    world.display("USD").await;
    let visible = world.holding("Visible", "USD").await;
    world.balance_on(visible, "2026-10-26", VISIBLE_MINOR).await;
    let (_, token) = world
        .token(Scope::Read, vec![on(visible, AccountLevel::Balances)])
        .await;
    let (_, admin) = world.token(Scope::Admin, Vec::new()).await;
    Scene {
        world,
        visible,
        token,
        admin,
    }
}

pub(super) async fn scene() -> Scene {
    let scene = unbuilt_scene().await;
    scene.world.rebuild(TODAY).await;
    scene
}

impl Scene {
    /// `uri` as `token` gets it, the visible id named.
    pub(super) async fn whole(&self, uri: &str, token: &SecretToken) -> Answer {
        let (status, headers, body) = self.world.get_whole(uri, token).await;
        let body = body.replace(&self.visible.to_string(), "<visible>");
        (uri.to_owned(), status, headers, body)
    }

    async fn every_read(&self, token: &SecretToken) -> Vec<Answer> {
        let mut answers = Vec::new();
        for uri in REPORT_READS.iter().chain(&OTHER_READS) {
            answers.push(self.whole(uri, token).await);
        }
        answers
    }

    pub(super) async fn report(&self, token: &SecretToken) -> Value {
        let (_, status, _, body) = self.whole(RANGE, token).await;
        assert_eq!(status, StatusCode::OK, "the report answers");
        serde_json::from_str(&body).unwrap()
    }
}

/// Fails naming the first read to differ.
pub(super) fn assert_identical(bare: &[Answer], held: &[Answer]) {
    assert_eq!(bare.len(), held.len());
    for (one, other) in bare.iter().zip(held) {
        assert!(one == other, "not byte-identical: {}", one.0);
    }
}

/// The restricted tokens of the two households get the same answers to everything.
pub(super) async fn assert_same_for_the_token(bare: &Scene, held: &Scene) {
    assert_identical(
        &bare.every_read(&bare.token).await,
        &held.every_read(&held.token).await,
    );
}

/// What a hidden account can be.
#[derive(Clone, Copy)]
enum Hidden {
    LargeBalance,
    Currency,
    Closed,
    ClosedOwing,
    Excluded,
    Hole,
    RowInAnotherCurrency,
    FetchFailing,
    NotPublished,
    TooLargeToConvert,
    TooLargeTotal,
    FutureSnapshot,
    BackDated,
    BeforeTheFirstDay,
    StaleBalance,
    PositiveLiability,
}

impl Hidden {
    /// Puts it in the household, and answers what its effect on the whole household's
    /// report holds, so a case that stopped doing anything fails.
    async fn place(self, world: &World) -> &'static str {
        let hidden = world.holding(HIDDEN_NAME, "USD").await;
        match self {
            Self::LargeBalance => {
                world.balance_on(hidden, "2026-10-26", HIDDEN_MINOR).await;
                "7346195280"
            }
            Self::Currency => {
                let foreign = world.holding("Quiet yen", "JPY").await;
                world.balance_on(foreign, "2026-10-26", HIDDEN_MINOR).await;
                "not_fetched_yet"
            }
            Self::Closed => {
                world.balance_on(hidden, "2026-10-26", HIDDEN_MINOR).await;
                world.close(hidden).await;
                "\"closed_on\":\"2026-11-02\""
            }
            Self::ClosedOwing => {
                world.balance_on(hidden, "2026-10-26", -HIDDEN_MINOR).await;
                world.close(hidden).await;
                "closed_account_owing"
            }
            Self::Excluded => {
                let off = world
                    .holding_as("Off", AccountKind::Checking, "USD", None, false)
                    .await;
                world.balance_on(off, "2026-10-26", HIDDEN_MINOR).await;
                "excluded_by_flag"
            }
            Self::Hole => {
                world.balance_on(hidden, "2026-10-26", HIDDEN_MINOR).await;
                world.hole_today(hidden).await;
                "daily_value_missing"
            }
            Self::RowInAnotherCurrency => {
                world.balance_on(hidden, "2026-10-26", HIDDEN_MINOR).await;
                world.row_in_chf(hidden).await;
                "internal"
            }
            Self::FetchFailing => {
                let foreign = world.holding("Quiet baht", "THB").await;
                world.balance_on(foreign, "2026-10-26", HIDDEN_MINOR).await;
                world.fetch_failed("THB", FxFetchStatus::Failing).await;
                "fetch_failing"
            }
            Self::NotPublished => {
                let foreign = world.holding("Quiet baht", "THB").await;
                world.balance_on(foreign, "2026-10-26", HIDDEN_MINOR).await;
                world.fetch_failed("THB", FxFetchStatus::NotPublished).await;
                "not_published"
            }
            Self::TooLargeToConvert => {
                let foreign = world.holding("Quiet forint", "HUF").await;
                world.balance_on(foreign, "2026-10-26", i64::MAX).await;
                world.rates("USD", &[("2026-10-01", "1.1")]).await;
                world.rates("HUF", &[("2026-10-01", "0.0001")]).await;
                "amount_too_large"
            }
            Self::TooLargeTotal => {
                let other = world.holding("Vast", "USD").await;
                world.balance_on(hidden, "2026-10-26", i64::MAX).await;
                world.balance_on(other, "2026-10-26", i64::MAX).await;
                "too large"
            }
            Self::FutureSnapshot => {
                world.balance_on(hidden, "2027-03-01", HIDDEN_MINOR).await;
                HIDDEN_NAME
            }
            Self::BackDated => {
                world.balance_on(hidden, "1970-01-01", HIDDEN_MINOR).await;
                "\"first_on\":\"1970-01-01\""
            }
            Self::BeforeTheFirstDay => {
                world.balance_on(hidden, "1961-06-01", HIDDEN_MINOR).await;
                "\"first_on\":\"1970-01-01\""
            }
            Self::StaleBalance => {
                world.balance_on(hidden, "2020-01-01", HIDDEN_MINOR).await;
                "balance_stale"
            }
            Self::PositiveLiability => {
                let card = world
                    .holding_as(
                        "Quiet card",
                        AccountKind::CreditCard,
                        "USD",
                        Some(LiquidityClass::ShortTermLiability),
                        true,
                    )
                    .await;
                world.balance_on(card, "2026-10-26", HIDDEN_MINOR).await;
                "liability_balance_positive"
            }
        }
    }
}

async fn changes_no_byte(hidden: Hidden) {
    let (bare, held) = (scene().await, scene().await);

    let effect = hidden.place(&held.world).await;

    let (_, _, _, whole_household) = held.whole(RANGE, &held.admin).await;
    assert!(
        whole_household.contains(effect),
        "the case must reach the whole household's answer"
    );
    assert_same_for_the_token(&bare, &held).await;
}

macro_rules! changes_no_byte {
    ($($test:ident => $case:ident,)*) => {$(
        #[tokio::test]
        async fn $test() {
            changes_no_byte(Hidden::$case).await;
        }
    )*};
}

changes_no_byte! {
    hidden_account_changes_no_byte => LargeBalance,
    a_hidden_currency_raises_no_caution => Currency,
    a_hidden_closed_holding_changes_no_byte => Closed,
    a_hidden_closed_debt_raises_no_caution => ClosedOwing,
    a_hidden_excluded_holding_changes_no_byte => Excluded,
    a_hidden_hole_raises_no_caution => Hole,
    a_hidden_corrupt_row_fails_no_read => RowInAnotherCurrency,
    a_hidden_failing_fetch_raises_no_caution => FetchFailing,
    a_hidden_unpublished_rate_raises_no_caution => NotPublished,
    a_hidden_amount_too_large_raises_no_caution => TooLargeToConvert,
    a_hidden_total_too_large_refuses_nothing => TooLargeTotal,
    a_hidden_future_snapshot_changes_no_byte => FutureSnapshot,
    a_hidden_balance_back_dated_to_1970_extends_no_range => BackDated,
    a_hidden_balance_before_1970_extends_no_range => BeforeTheFirstDay,
    a_hidden_stale_balance_raises_no_caution => StaleBalance,
    a_hidden_positive_liability_raises_no_caution => PositiveLiability,
}

/// Display USD, a visible USD account, a hidden JPY account: the household fetches USD rates
/// only because of the hidden account (N7), and the visible row needs none of them.
async fn with_a_coverage_entry_only_the_hidden_yen_caused(entry: impl AsyncFnOnce(&World)) {
    let (bare, held) = (scene().await, scene().await);

    let yen = held.world.holding(HIDDEN_NAME, "JPY").await;
    held.world.balance_on(yen, "2026-10-26", HIDDEN_MINOR).await;
    entry(&held.world).await;

    let coverage = held.whole("/api/v1/fx/coverage", &held.admin).await;
    assert!(coverage.3.contains("USD"), "the entry for USD exists");
    assert_same_for_the_token(&bare, &held).await;
}

#[tokio::test]
async fn a_usd_entry_only_a_hidden_yen_caused_changes_no_byte() {
    with_a_coverage_entry_only_the_hidden_yen_caused(async |world| {
        world.rates("JPY", &[("2026-10-01", "175.68")]).await;
        world.rates("USD", &[("2026-10-01", "1.1188")]).await;
    })
    .await;
}

#[tokio::test]
async fn a_failing_usd_entry_only_a_hidden_yen_caused_changes_no_byte() {
    with_a_coverage_entry_only_the_hidden_yen_caused(async |world| {
        world.fetch_failed("USD", FxFetchStatus::Failing).await;
    })
    .await;
}

#[tokio::test]
async fn an_unpublished_usd_entry_only_a_hidden_yen_caused_changes_no_byte() {
    with_a_coverage_entry_only_the_hidden_yen_caused(async |world| {
        world.fetch_failed("USD", FxFetchStatus::NotPublished).await;
    })
    .await;
}

#[tokio::test]
async fn a_token_s_sum_is_what_it_can_see() {
    let held = scene().await;
    let hidden = held.world.holding(HIDDEN_NAME, "USD").await;
    held.world
        .balance_on(hidden, "2026-10-26", HIDDEN_MINOR)
        .await;

    let report = held.report(&held.token).await;
    let whole_household = held.report(&held.admin).await;

    let last = |report: &Value| report["points"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last(&report)["total"], money(VISIBLE_MINOR, "USD"));
    assert_eq!(
        last(&whole_household)["total"],
        money(VISIBLE_MINOR + HIDDEN_MINOR, "USD")
    );
    assert_eq!(report["status"], "firm");
}

/// Two visible accounts with a hidden one created between them read in the same order, and
/// nothing skips a place.
#[tokio::test]
async fn hidden_accounts_change_no_order() {
    let (bare, held) = (world().await, world().await);
    bare.on_today();
    held.on_today();
    let mut answers = Vec::new();
    for (world, hide) in [(&bare, false), (&held, true)] {
        world.rebuild(TODAY).await;
        world.display("USD").await;
        let first = world.holding("First", "USD").await;
        tokio::time::sleep(Duration::from_millis(2)).await;
        if hide {
            let hidden = world.holding(HIDDEN_NAME, "EUR").await;
            world.balance_on(hidden, "2026-10-26", HIDDEN_MINOR).await;
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let second = world.holding("Second", "USD").await;
        for holder in [first, second] {
            world.balance_on(holder, "2026-10-26", VISIBLE_MINOR).await;
        }
        let (_, token) = world
            .token(
                Scope::Read,
                vec![
                    on(second, AccountLevel::Balances),
                    on(first, AccountLevel::Balances),
                ],
            )
            .await;
        let (_, _, body) = world.get_whole(RANGE, &token).await;
        answers.push(
            body.replace(&first.to_string(), "<first>")
                .replace(&second.to_string(), "<second>"),
        );
    }
    assert!(answers[0] == answers[1], "byte-identical");
    let first_at = answers[0].find("\"name\":\"First\"").unwrap();
    assert!(first_at < answers[0].find("\"name\":\"Second\"").unwrap());
}

/// `through` is the household's: a day filled is filled, with or without anything hidden, and
/// no snapshot moves it.
#[tokio::test]
async fn through_is_the_same_with_hidden_snapshots_on_either_side_of_it() {
    let (bare, held) = (unbuilt_scene().await, unbuilt_scene().await);
    for scene in [&bare, &held] {
        scene.world.rebuild("2026-10-30").await;
    }
    let hidden = held.world.holding(HIDDEN_NAME, "USD").await;
    held.world
        .balance_on(hidden, "2026-11-01", HIDDEN_MINOR)
        .await;
    held.world
        .balance_on(hidden, "1990-01-01", HIDDEN_MINOR)
        .await;

    let report = held.report(&held.token).await;
    assert_eq!(report["through"], "2026-10-30");
    assert_eq!(
        report["cautions"],
        json!([{"kind": "to_date_not_yet_filled", "through": "2026-10-30"}])
    );
    assert_same_for_the_token(&bare, &held).await;
}

/// A daily fill moves `through` the same for a household holding nothing else.
#[tokio::test]
async fn a_fill_moves_through_the_same_with_a_hidden_holding() {
    let (bare, held) = (unbuilt_scene().await, unbuilt_scene().await);
    let hidden = held.world.holding(HIDDEN_NAME, "USD").await;
    // Later than the visible balance, and past the first fill's start.
    held.world
        .balance_on(hidden, "2026-10-31", HIDDEN_MINOR)
        .await;
    for scene in [&bare, &held] {
        scene.world.rebuild("2026-10-30").await;
        scene.world.fill("2026-11-01").await;
    }

    assert_eq!(held.report(&held.token).await["through"], "2026-11-01");
    assert_same_for_the_token(&bare, &held).await;
}

/// While a rebuild is needed nobody gets a number, and hidden data doesn't change that.
#[tokio::test]
async fn rebuild_needed_reads_the_same_with_a_hidden_holding() {
    let (bare, held) = (unbuilt_scene().await, unbuilt_scene().await);
    let hidden = held.world.holding(HIDDEN_NAME, "USD").await;
    held.world
        .balance_on(hidden, "2026-10-01", HIDDEN_MINOR)
        .await;

    assert_eq!(
        held.report(&held.token).await["blockers"],
        json!([{"kind": "rebuilding"}])
    );
    assert_same_for_the_token(&bare, &held).await;
}

/// No display currency: the blocker is the same, whoever else holds what.
#[tokio::test]
async fn no_display_currency_reads_the_same_with_a_hidden_holding() {
    let mut scenes = Vec::new();
    for hide in [false, true] {
        let world = world().await;
        world.on_today();
        world.rebuild(TODAY).await;
        let visible = world.holding("Visible", "USD").await;
        world.balance_on(visible, "2026-10-26", VISIBLE_MINOR).await;
        if hide {
            let hidden = world.holding(HIDDEN_NAME, "JPY").await;
            world.balance_on(hidden, "2026-10-26", HIDDEN_MINOR).await;
            world.rates("USD", &[("2026-10-01", "1.1188")]).await;
        }
        let (_, token) = world
            .token(Scope::Read, vec![on(visible, AccountLevel::Balances)])
            .await;
        let (_, admin) = world.token(Scope::Admin, Vec::new()).await;
        scenes.push(Scene {
            world,
            visible,
            token,
            admin,
        });
    }

    assert_eq!(
        scenes[1].report(&scenes[1].token).await["blockers"],
        json!([{"kind": "display_currency_not_set"}])
    );
    assert_same_for_the_token(&scenes[0], &scenes[1]).await;
}

/// A grant naming nothing that exists can't be made through the API, and is the one way to
/// see nothing at all. It reads as an empty household does to a token seeing everything.
#[tokio::test]
async fn a_token_seeing_nothing_reads_like_an_empty_household() {
    let (empty, held) = (world().await, world().await);
    for world in [&empty, &held] {
        world.on_today();
        world.rebuild(TODAY).await;
        world.display("USD").await;
    }
    let hidden = held.holding(HIDDEN_NAME, "JPY").await;
    held.balance_on(hidden, "2026-10-26", HIDDEN_MINOR).await;
    held.rates("JPY", &[("2026-10-01", "175.68")]).await;
    let (id, nothing) = held
        .token(Scope::Read, vec![on(hidden, AccountLevel::Balances)])
        .await;
    let mut tx = held.server.store.begin().await.unwrap();
    tx.replace_token_grants(id, &[on(AccountId::new(), AccountLevel::Balances)])
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let (_, everything) = empty.token(Scope::Read, Vec::new()).await;

    for uri in REPORT_READS.iter().chain(&["/api/v1/accounts"]) {
        assert!(
            empty.get_whole(uri, &everything).await == held.get_whole(uri, &nothing).await,
            "not byte-identical: {uri}"
        );
    }
    let (_, _, text) = held.get_whole(RANGE, &nothing).await;
    let report: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(report["status"], "unavailable");
    assert_eq!(report["blockers"], json!([{"kind": "no_counted_accounts"}]));
}

/// `balances` is all a report needs, and a report holds nothing of a transaction: its
/// description, amount or id, whether the token could read them or not.
#[tokio::test]
async fn balances_level_is_enough_and_shows_no_transaction_data() {
    let (bare, busy) = (scene().await, scene().await);

    let transaction = busy
        .world
        .transaction(busy.visible, 1, "Quiet purchase")
        .await;

    assert_same_for_the_token(&bare, &busy).await;
    let text = busy.whole(RANGE, &busy.token).await.3;
    assert_eq!(busy.report(&busy.token).await["status"], "firm");
    assert!(!text.contains("Quiet purchase"), "no description");
    assert!(!text.contains(&transaction.to_string()), "no id");
    assert!(!text.contains("-1000"), "no amount");
}

/// One request is enough: the access comes from the grants as they are now, and the answer
/// is the one any token holding those grants gets.
#[tokio::test]
async fn grant_change_applies_at_the_next_request() {
    let held = scene().await;
    let other = held.world.holding(HIDDEN_NAME, "USD").await;
    held.world
        .balance_on(other, "2026-10-26", HIDDEN_MINOR)
        .await;
    let (id, token) = held
        .world
        .token(Scope::Read, vec![on(held.visible, AccountLevel::Balances)])
        .await;
    let (_, _, before) = held.world.get_whole(RANGE, &token).await;
    assert!(!before.contains(HIDDEN_NAME));

    held.world
        .server
        .agent_tokens
        .replace_grants(
            &held.world.alice,
            id,
            vec![on(other, AccountLevel::Balances)],
        )
        .await
        .unwrap();

    let (_, fresh) = held
        .world
        .token(Scope::Read, vec![on(other, AccountLevel::Balances)])
        .await;
    let (_, _, after) = held.world.get_whole(RANGE, &token).await;
    let (_, _, expected) = held.world.get_whole(RANGE, &fresh).await;
    assert!(after == expected, "byte-identical");
    assert!(after.contains(HIDDEN_NAME));
    assert!(!after.contains("Visible"));
}

/// An account created after a token counts for it only through an `all` grant (§8.2).
#[tokio::test]
async fn new_account_needs_an_all_grant_to_count() {
    let held = scene().await;
    let (_, wide) = held
        .world
        .token(Scope::Read, vec![all(AccountLevel::Balances)])
        .await;
    let before = held.whole(RANGE, &held.token).await;

    let new = held.world.holding(HIDDEN_NAME, "USD").await;
    held.world.balance_on(new, "2026-10-26", HIDDEN_MINOR).await;

    assert!(
        held.whole(RANGE, &held.token).await == before,
        "byte-identical"
    );
    let report = held.report(&wide).await;
    let last = report["points"].as_array().unwrap().last().unwrap();
    assert_eq!(last["total"], money(VISIBLE_MINOR + HIDDEN_MINOR, "USD"));
}

/// Passkey sessions have uniform access: the sum of everything, whatever grants exist.
#[tokio::test]
async fn a_session_sees_everything() {
    let held = scene().await;
    let hidden = held.world.holding(HIDDEN_NAME, "JPY").await;
    held.world
        .balance_on(hidden, "2026-10-26", HIDDEN_MINOR)
        .await;

    let report = held
        .world
        .server
        .ledger
        .net_worth(
            &held.world.alice,
            serde_json::from_value(json!({"from": "2026-10-26", "to": TODAY, "interval": "day"}))
                .unwrap(),
        )
        .await
        .unwrap();
    let report = serde_json::to_string(&report).unwrap();

    let (_, _, for_the_admin_token) = held.world.get_whole(RANGE, &held.admin).await;
    assert!(report == for_the_admin_token, "the same answer");
    assert!(report.contains(HIDDEN_NAME));
    assert!(report.contains(&hidden.to_string()));
}

/// A refused report says nothing of what is held, and reads the same for every token.
#[tokio::test]
async fn no_problem_body_names_an_account() {
    let (bare, held) = (scene().await, scene().await);
    let hidden = held.world.holding(HIDDEN_NAME, "JPY").await;
    held.world
        .balance_on(hidden, "2026-10-26", HIDDEN_MINOR)
        .await;
    let (_, wide) = held
        .world
        .token(Scope::Read, vec![all(AccountLevel::Transactions)])
        .await;

    let refused = [
        ("?from=2020-01-01&to=2026-11-02&interval=day", 422),
        ("?from=2026-11-02&to=2026-10-01", 422),
        ("?from=1969-12-31", 422),
        ("?from=2026-1-1", 400),
        ("?interval=year", 400),
        ("?unknown=1", 400),
    ];
    for (query, status) in refused {
        let uri = format!("{REPORT}{query}");
        let (_, got, _, body) = held.whole(&uri, &held.token).await;
        assert_eq!(got.as_u16(), status, "{query}");
        assert!(!body.contains(HIDDEN_NAME) && !body.contains("JPY"));
        assert!(!body.contains("Visible") && !body.contains("<visible>"));
        for other in [
            bare.whole(&uri, &bare.token).await,
            held.whole(&uri, &held.admin).await,
            held.whole(&uri, &wide).await,
        ] {
            assert!(other.1 == got && other.3 == body, "byte-identical: {query}");
        }
    }
}

/// A total past `i64` of the token's own is its own 422, and hidden money on top neither
/// causes nor cures one.
#[tokio::test]
async fn a_token_s_own_total_too_large_reads_the_same_beside_hidden_money() {
    let (bare, held) = (scene().await, scene().await);
    let mut answers = Vec::new();
    for (scene, hide) in [(&bare, false), (&held, true)] {
        let world = &scene.world;
        let second = world.holding("Second", "USD").await;
        world.balance_on(second, "2026-10-26", i64::MAX).await;
        world
            .balance_on(scene.visible, "2026-10-26", i64::MAX)
            .await;
        if hide {
            let hidden = world.holding(HIDDEN_NAME, "USD").await;
            world.balance_on(hidden, "2026-10-26", i64::MIN).await;
        }
        let (_, token) = world
            .token(
                Scope::Read,
                vec![
                    on(scene.visible, AccountLevel::Balances),
                    on(second, AccountLevel::Balances),
                ],
            )
            .await;
        answers.push(world.get_whole(RANGE, &token).await);
    }
    assert_eq!(answers[0].0, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(answers[0] == answers[1], "byte-identical");
}
