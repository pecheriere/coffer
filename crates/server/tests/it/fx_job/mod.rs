//! The daily jobs: when rates are asked for, what an answer does to what is
//! stored, the table's days, and that neither job can hold up serving or a
//! sync. Rates come from a scripted provider, or the real ECB code over a
//! loopback mock. No test reaches the real service.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use coffer_domain::accounts::{AccountKind, AccountName, BalanceSource, LiquidityClass};
use coffer_domain::fx::{FxCoverage, FxRate, FxSource, RateObservation};
use coffer_domain::ids::AccountId;
use coffer_domain::money::Currency;
use coffer_domain::net_worth::DailyValue;
use coffer_server::egress::{Egress, HostAllowList};
use coffer_server::fx::ecb::Ecb;
use coffer_server::fx::{self, FxService, RateError, RateProvider};
use coffer_server::sync::provider::ProviderFuture;
use coffer_server::testkit::{MockUpstream, TestServer};
use coffer_store::{BalanceSnapshot, NewAccount, Store};
use jiff::civil::{Date, date};
use jiff::{SignedDuration, Timestamp};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

mod answers;
mod running;

/// How long a test waits for a background task before failing.
const WAIT: Duration = Duration::from_secs(10);
/// Quick enough that a test sees several passes.
const FAST_TICK: Duration = Duration::from_millis(20);

/// A Monday.
const MONDAY: Date = date(2026, 11, 2);
const FRIDAY: Date = date(2026, 10, 30);
const SATURDAY: Date = date(2026, 10, 31);
const SUNDAY: Date = date(2026, 11, 1);

fn cur(code: &str) -> Currency {
    Currency::parse(code).unwrap()
}

fn hours(count: i64) -> SignedDuration {
    SignedDuration::from_hours(count)
}

fn rate(on: Date, value: &str) -> RateObservation {
    RateObservation {
        on,
        rate: value.parse::<FxRate>().unwrap(),
    }
}

/// One call a [`Scripted`] provider received.
type Call = (Currency, Option<Date>, Date);

/// Answers each currency as told, an empty answer when not told, and keeps
/// every call.
#[derive(Default)]
struct Scripted {
    answers: Mutex<HashMap<Currency, Result<Vec<RateObservation>, RateError>>>,
    calls: Mutex<Vec<Call>>,
    panic_next: AtomicBool,
}

impl Scripted {
    fn answer(&self, code: &str, answer: Result<Vec<RateObservation>, RateError>) {
        self.answers.lock().unwrap().insert(cur(code), answer);
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    fn forget_calls(&self) {
        self.calls.lock().unwrap().clear();
    }
}

impl RateProvider for Scripted {
    fn source(&self) -> FxSource {
        FxSource::Ecb
    }

    fn fetch(
        &self,
        currency: Currency,
        since: Option<Date>,
        today: Date,
    ) -> ProviderFuture<'_, Result<Vec<RateObservation>, RateError>> {
        Box::pin(async move {
            assert!(
                !self.panic_next.swap(false, Ordering::SeqCst),
                "scripted panic marker 7c1e"
            );
            self.calls.lock().unwrap().push((currency, since, today));
            self.answers
                .lock()
                .unwrap()
                .get(&currency)
                .cloned()
                .unwrap_or_else(|| Ok(Vec::new()))
        })
    }
}

/// Never answers, and says when it was asked.
#[derive(Default)]
struct Stalled {
    asked: AtomicBool,
}

impl RateProvider for Stalled {
    fn source(&self) -> FxSource {
        FxSource::Ecb
    }

    fn fetch(
        &self,
        _currency: Currency,
        _since: Option<Date>,
        _today: Date,
    ) -> ProviderFuture<'_, Result<Vec<RateObservation>, RateError>> {
        self.asked.store(true, Ordering::SeqCst);
        Box::pin(std::future::pending())
    }
}

/// `server`'s services with `provider` as the rate source.
fn service(server: &TestServer, provider: Option<Arc<dyn RateProvider>>) -> FxService {
    FxService::new(
        server.store.clone(),
        server.clock.clone(),
        server.config.timezone.clone(),
        provider,
    )
}

/// A service over a scripted provider, on [`MONDAY`] at noon.
async fn scripted() -> (TestServer, Arc<Scripted>, FxService) {
    let server = TestServer::new().await;
    server.clock.set(noon(MONDAY));
    let provider = Arc::new(Scripted::default());
    let fx = service(
        &server,
        Some(Arc::clone(&provider) as Arc<dyn RateProvider>),
    );
    (server, provider, fx)
}

fn noon(day: Date) -> Timestamp {
    day.at(12, 0, 0, 0)
        .to_zoned(jiff::tz::TimeZone::UTC)
        .unwrap()
        .timestamp()
}

async fn holding(store: &Store, code: &str) -> AccountId {
    let id = AccountId::new();
    let mut tx = store.begin().await.unwrap();
    tx.insert_account(NewAccount {
        id,
        name: AccountName::parse("Holding").unwrap(),
        kind: AccountKind::Checking,
        currency: cur(code),
        liquidity: LiquidityClass::Cash,
        liquidity_overridden: false,
        include_in_net_worth: true,
        include_in_forecast: true,
        created_at: Timestamp::from_second(1_800_000_000).unwrap(),
    })
    .await
    .unwrap();
    tx.commit().await.unwrap();
    id
}

async fn snap(store: &Store, account: AccountId, on: Date, minor: i64) {
    let mut tx = store.begin().await.unwrap();
    tx.upsert_balance(BalanceSnapshot {
        account,
        on,
        balance_minor: minor,
        source: BalanceSource::Manual,
        recorded_at: Timestamp::from_second(1_800_000_000).unwrap(),
    })
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

async fn show_in(store: &Store, code: &str) {
    let mut tx = store.begin().await.unwrap();
    tx.set_display_currency(cur(code)).await.unwrap();
    tx.commit().await.unwrap();
}

async fn coverage_of(store: &Store, code: &str) -> Option<FxCoverage> {
    store
        .fx_coverage()
        .await
        .unwrap()
        .into_iter()
        .find(|row| row.currency == cur(code))
}

async fn rates_of(store: &Store, code: &str) -> Vec<(Date, String)> {
    let mut found: Vec<_> = store
        .fx_rates(cur(code), date(1990, 1, 1), date(2100, 1, 1))
        .await
        .unwrap()
        .into_iter()
        .map(|one| (one.on, one.rate.to_string()))
        .collect();
    found.reverse();
    found
}

async fn rows_of(store: &Store, account: AccountId) -> Vec<DailyValue> {
    store
        .daily_values_for_tests()
        .await
        .unwrap()
        .into_iter()
        .filter(|row| row.account == account)
        .collect()
}

/// What `account`'s row of `on` comes to in the display currency.
async fn display_minor(store: &Store, account: AccountId, on: Date) -> Option<i64> {
    rows_of(store, account)
        .await
        .into_iter()
        .find(|row| row.on == on)
        .unwrap()
        .display_minor
}

/// A holding of USD 1,000.00 from `first`, with the table current and the euro
/// as display currency.
async fn usd_holding_in_euros(server: &TestServer, fx: &FxService, first: Date) -> AccountId {
    let usd = holding(&server.store, "USD").await;
    snap(&server.store, usd, first, 100_000).await;
    show_in(&server.store, "EUR").await;
    fx.keep_table_current().await.unwrap();
    usd
}

/// Waits for `check` to hold.
async fn until<F, C>(mut check: C)
where
    C: FnMut() -> F,
    F: Future<Output = bool>,
{
    tokio::time::timeout(WAIT, async {
        while !check().await {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

/// Waits until the table's last day is `day`.
async fn filled_through(store: &Store, day: Date) {
    until(|| async { store.net_worth_state().await.unwrap().filled_through == Some(day) }).await;
}

/// The running jobs and the means to stop them.
struct Running {
    stop: oneshot::Sender<()>,
    handle: JoinHandle<()>,
}

impl Running {
    async fn stop(self) {
        self.stop.send(()).unwrap();
        tokio::time::timeout(WAIT, self.handle)
            .await
            .unwrap()
            .unwrap();
    }
}

fn start(fx: FxService) -> Running {
    let (stop, stopped) = oneshot::channel::<()>();
    let handle = fx::spawn(Arc::new(fx.with_tick(FAST_TICK)), async {
        let _ = stopped.await;
    });
    Running { stop, handle }
}

#[tokio::test]
async fn first_use_fetches_the_whole_history_once() {
    let (server, provider, fx) = scripted().await;
    holding(&server.store, "USD").await;
    show_in(&server.store, "EUR").await;
    provider.answer("USD", Ok(vec![rate(FRIDAY, "1.1")]));

    assert_eq!(fx.fetch_due().await.unwrap(), 1);
    assert_eq!(provider.calls(), [(cur("USD"), None, MONDAY)]);

    server
        .clock
        .advance(hours(6) - SignedDuration::from_secs(1));
    assert_eq!(fx.fetch_due().await.unwrap(), 0);
    assert_eq!(provider.calls().len(), 1, "inside six hours asks nothing");

    server.clock.advance(SignedDuration::from_secs(1));
    assert_eq!(fx.fetch_due().await.unwrap(), 1);
    assert_eq!(
        provider.calls()[1],
        (cur("USD"), Some(date(2026, 10, 23)), MONDAY),
        "from a week before its last rate"
    );
}

#[tokio::test]
async fn a_new_account_in_a_new_currency_is_backfilled_on_the_next_tick() {
    let (server, provider, fx) = scripted().await;
    holding(&server.store, "USD").await;
    show_in(&server.store, "EUR").await;
    provider.answer("USD", Ok(vec![rate(FRIDAY, "1.1")]));
    fx.fetch_due().await.unwrap();
    provider.forget_calls();

    holding(&server.store, "JPY").await;
    fx.fetch_due().await.unwrap();

    assert_eq!(provider.calls(), [(cur("JPY"), None, MONDAY)]);
}

#[tokio::test]
async fn single_currency_household_contacts_nobody() {
    for (held, display) in [
        ("USD", Some("USD")),
        ("USD", None),
        ("JPY", None),
        ("EUR", Some("EUR")),
    ] {
        let (server, provider, fx) = scripted().await;
        holding(&server.store, held).await;
        if let Some(display) = display {
            show_in(&server.store, display).await;
        }

        assert_eq!(fx.fetch_due().await.unwrap(), 0);

        assert_eq!(provider.calls(), []);
        assert_eq!(server.store.fx_coverage().await.unwrap(), []);
    }
}

/// The same household through the real provider: the mock sees no request.
#[tokio::test]
async fn single_currency_household_sends_no_request() {
    let server = TestServer::new().await;
    let mock = MockUpstream::start(Router::new()).await;
    let fx = service(&server, Some(ecb_at(&mock)));
    holding(&server.store, "USD").await;
    show_in(&server.store, "USD").await;

    fx.fetch_due().await.unwrap();

    assert_eq!(mock.requests(), []);
}

fn ecb_at(mock: &MockUpstream) -> Arc<dyn RateProvider> {
    Arc::new(Ecb::with_base(
        Egress::for_tests(),
        HostAllowList::loopback(mock.base.port().unwrap()),
        mock.base.clone(),
    ))
}

/// A last rate after today, from a clock that once ran ahead or a bad row, must
/// not ask for a window in the future, which the ECB answers with an error page.
#[tokio::test]
async fn a_last_rate_after_today_asks_from_a_week_before_today() {
    let (server, provider, fx) = scripted().await;
    holding(&server.store, "USD").await;
    show_in(&server.store, "EUR").await;
    let later = date(2027, 6, 1);
    provider.answer("USD", Ok(vec![rate(later, "1.1")]));
    fx.fetch_due().await.unwrap();
    provider.forget_calls();

    server.clock.advance(hours(6));
    fx.fetch_due().await.unwrap();

    assert_eq!(
        provider.calls(),
        [(cur("USD"), Some(date(2026, 10, 26)), MONDAY)]
    );
}
