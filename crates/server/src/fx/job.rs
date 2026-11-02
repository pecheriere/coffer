//! The daily jobs (spec §8.10 N3, N7): fetching the rates a household needs and
//! keeping the daily table up to today.
//!
//! Two supervised tasks, sharing nothing with each other or with the sync
//! worker but the database: one fetch can take a minute, and a day must not wait
//! for it, nor a sync. A failure here is logged and tried again, and stops
//! nothing else. Nothing a source says reaches a log or the store except rates
//! and dates read strictly, and a status.

use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use coffer_domain::fx::{FxCoverage, FxErrorCode, FxFetchStatus, FxSource, RateObservation};
use coffer_domain::money::Currency;
use coffer_domain::time::Clock;
use coffer_store::{Store, StoreError};
use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Span, Timestamp};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use super::{RateError, RateProvider};
use crate::household_time::HouseholdTime;
use crate::tasks::{Backoff, pause, stopped, supervise};

/// How often each task looks for something to do.
const TICK: Duration = Duration::from_mins(1);

/// How long until a currency is asked for again, by how the last try went.
const AFTER_READY: SignedDuration = SignedDuration::from_hours(6);
const AFTER_FAILING: SignedDuration = SignedDuration::from_hours(1);
const AFTER_NOT_PUBLISHED: SignedDuration = SignedDuration::from_hours(24);

/// The wait between two requests of one pass, a courtesy to the source. A stop
/// during it drops the pass, as during a fetch.
const REQUEST_PAUSE: Duration = Duration::from_secs(1);

/// How far before its last rate a currency is asked for, so a missed day or a
/// correction arrives by itself.
const LOOK_BACK_DAYS: i64 = 7;

/// The currencies whose rates a household needs: its accounts' and its display
/// currency, without the euro, and none when those are one currency (N7).
#[must_use]
pub fn needed(accounts: &[Currency], display: Option<Currency>) -> Vec<Currency> {
    let mut currencies: Vec<_> = accounts.iter().copied().chain(display).collect();
    currencies.sort_unstable();
    currencies.dedup();
    if currencies.len() < 2 {
        return Vec::new();
    }
    currencies.retain(|currency| currency.as_str() != "EUR");
    currencies
}

/// Those of `needed` to ask for at `now`: never tried, tried long enough ago for
/// how that went, or tried at a time later than now. That last came from a clock
/// that ran ahead, and must not silence a currency until that date.
#[must_use]
pub fn due(now: Timestamp, needed: &[Currency], coverage: &[FxCoverage]) -> Vec<Currency> {
    needed
        .iter()
        .copied()
        .filter(|currency| {
            coverage
                .iter()
                .find(|row| row.currency == *currency)
                .is_none_or(|row| {
                    let wait = match row.status {
                        FxFetchStatus::Ready => AFTER_READY,
                        FxFetchStatus::Failing => AFTER_FAILING,
                        FxFetchStatus::NotPublished => AFTER_NOT_PUBLISHED,
                    };
                    row.last_attempt_at > now
                        || row
                            .last_attempt_at
                            .checked_add(wait)
                            .is_ok_and(|next| next <= now)
                })
        })
        .collect()
}

/// Where a currency's next answer starts: a week before its last rate, or before
/// today if that rate is later, since the source refuses a window in the future.
fn window_start(last_on: Date, today: Date) -> Date {
    last_on
        .min(today)
        .saturating_sub(Span::new().days(LOOK_BACK_DAYS))
}

/// How fetching a currency stood, as far as what the log has said goes. A row
/// failing with no code is an attempt whose answer never came, of which nothing
/// was said, so it counts as nothing known.
fn said_before(row: &FxCoverage) -> Option<(FxFetchStatus, Option<FxErrorCode>)> {
    let state = (row.status, row.last_error);
    (state != (FxFetchStatus::Failing, None)).then_some(state)
}

/// Says that the source doesn't have `currency`, once until that changes.
fn say_not_published(currency: Currency, before: Option<(FxFetchStatus, Option<FxErrorCode>)>) {
    const TEXT: &str = "a currency's exchange rates are not published";
    let status = FxFetchStatus::NotPublished.as_str();
    if before == Some((FxFetchStatus::NotPublished, None)) {
        tracing::debug!(currency = %currency, status, "{TEXT}");
    } else {
        tracing::warn!(currency = %currency, status, "{TEXT}");
    }
}

/// Says that `currency` failed, once until the failure or the status changes. A
/// blocked host fails every hour, and an hourly warning buries everything else.
fn say_failed(
    currency: Currency,
    before: Option<(FxFetchStatus, Option<FxErrorCode>)>,
    error: RateError,
) {
    const TEXT: &str = "could not fetch a currency's exchange rates";
    let status = FxFetchStatus::Failing.as_str();
    if before == Some((FxFetchStatus::Failing, error.code())) {
        tracing::debug!(currency = %currency, status, reason = %error, "{TEXT}");
    } else {
        tracing::warn!(currency = %currency, status, reason = %error, "{TEXT}");
    }
}

/// Says that `currency` has rates again, if it was said that it hadn't.
fn say_fetched(currency: Currency, before: Option<(FxFetchStatus, Option<FxErrorCode>)>) {
    if before.is_some_and(|(status, _)| status != FxFetchStatus::Ready) {
        tracing::warn!(
            currency = %currency,
            status = FxFetchStatus::Ready.as_str(),
            "a currency's exchange rates can be fetched again"
        );
    }
}

/// Logs that a currency's turn couldn't be stored, and hands the error on.
fn not_stored(currency: Currency, err: StoreError) -> StoreError {
    tracing::warn!(currency = %currency, error = ?err, "could not store an exchange rate fetch");
    err
}

/// Exchange rates and the daily table's days.
pub struct FxService {
    store: Store,
    clock: Arc<dyn Clock>,
    /// The household's zone and day.
    time: HouseholdTime,
    provider: Option<Arc<dyn RateProvider>>,
    tick: Duration,
    /// The household's day it was last said that the table runs ahead of it.
    ahead_said_on: Mutex<Option<Date>>,
}

impl std::fmt::Debug for FxService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FxService").finish_non_exhaustive()
    }
}

impl FxService {
    /// The service over `store`, asking `provider` for rates if there is one.
    #[must_use]
    pub fn new(
        store: Store,
        clock: Arc<dyn Clock>,
        timezone: TimeZone,
        provider: Option<Arc<dyn RateProvider>>,
    ) -> Self {
        Self {
            time: HouseholdTime::new(store.clone(), Arc::clone(&clock), timezone),
            store,
            clock,
            provider,
            tick: TICK,
            ahead_said_on: Mutex::new(None),
        }
    }

    /// Makes each task look for work every `tick`, for tests that watch several
    /// passes.
    #[cfg(feature = "testkit")]
    #[must_use]
    pub fn with_tick(self, tick: Duration) -> Self {
        Self { tick, ..self }
    }

    /// Who publishes the rates asked for, if anyone is asked.
    #[cfg(feature = "testkit")]
    #[must_use]
    pub fn rate_source(&self) -> Option<FxSource> {
        self.provider.as_ref().map(|provider| provider.source())
    }

    /// Asks for each currency that is due, one at a time, and answers how many
    /// were asked for. The attempt commits before the call, so a store that
    /// can't write means no request, and a crash during one or an answer that
    /// can't be stored waits its interval like any other try. No transaction is
    /// open during the call; the answer then commits in one, with its rates, its
    /// rows and its coverage.
    ///
    /// # Errors
    ///
    /// Backend failures, the first of them once every currency has had its turn.
    /// A provider's failure isn't one: it is recorded.
    pub async fn fetch_due(&self) -> Result<u32, StoreError> {
        let Some(provider) = &self.provider else {
            return Ok(0);
        };
        let accounts = self.store.currencies_in_use().await?;
        let display = self.store.display_currency().await?;
        let coverage = self.store.fx_coverage().await?;
        let wanted = needed(&accounts, display);
        let today = self.time.today().await?;
        let mut asked = 0;
        let mut failure = None;
        for currency in due(self.clock.now(), &wanted, &coverage) {
            if asked > 0 {
                tokio::time::sleep(REQUEST_PAUSE).await;
            }
            let known = coverage.iter().find(|row| row.currency == currency);
            let since = known
                .and_then(|row| row.last_on)
                .map(|last_on| window_start(last_on, today));
            let before = known.and_then(said_before);
            if let Err(err) = self.record_attempt(currency).await {
                failure.get_or_insert(not_stored(currency, err));
                continue;
            }
            let answer = provider.fetch(currency, since, today).await;
            asked += 1;
            if let Err(err) = self
                .record(provider.source(), currency, since, before, answer)
                .await
            {
                failure.get_or_insert(not_stored(currency, err));
            }
        }
        failure.map_or(Ok(asked), Err)
    }

    async fn record_attempt(&self, currency: Currency) -> Result<(), StoreError> {
        let mut tx = self.store.begin().await?;
        tx.record_fx_attempt(currency, self.clock.now()).await?;
        tx.commit().await
    }

    /// Commits one answer about `currency`. An answer with `since` is about a
    /// window of days that held a rate, and replaces what it speaks for; with no
    /// `since` nothing was stored for it to replace.
    async fn record(
        &self,
        source: FxSource,
        currency: Currency,
        since: Option<Date>,
        before: Option<(FxFetchStatus, Option<FxErrorCode>)>,
        answer: Result<Vec<RateObservation>, RateError>,
    ) -> Result<(), StoreError> {
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        match answer {
            Ok(rates) => {
                if !rates.is_empty() {
                    say_fetched(currency, before);
                } else if since.is_some() {
                    tracing::warn!(
                        currency = %currency,
                        "the rate source gave no rates where some are stored; they are kept"
                    );
                } else {
                    say_not_published(currency, before);
                }
                if let Some(since) = since {
                    tx.replace_fx_rates(currency, source, since, &rates, now)
                        .await?;
                } else {
                    tx.upsert_fx_rates(currency, source, &rates, now).await?;
                }
            }
            Err(RateError::NotPublished) => {
                say_not_published(currency, before);
                tx.record_fx_failure(currency, FxFetchStatus::NotPublished, None, now)
                    .await?;
            }
            Err(error) => {
                say_failed(currency, before, error);
                tx.record_fx_failure(currency, FxFetchStatus::Failing, error.code(), now)
                    .await?;
            }
        }
        tx.commit().await
    }

    /// Brings the table up to today: the rebuild it is waiting for, or the days
    /// since it was last filled. Reads one row when there is nothing to do.
    ///
    /// # Errors
    ///
    /// Backend failures, including a stored row that no longer parses.
    pub async fn keep_table_current(&self) -> Result<(), StoreError> {
        let today = self.time.today().await?;
        let state = self.store.net_worth_state().await?;
        if let (false, Some(filled)) = (state.rebuild_needed, state.filled_through)
            && filled >= today
        {
            self.say_if_far_ahead(filled, today);
            return Ok(());
        }
        let mut tx = self.store.begin().await?;
        // Asked again under the write lock: `coffer rebuild-net-worth` may have
        // got there first, and a change of zone may have moved today back.
        let today = self.time.today_seen_by(&mut tx).await?;
        if tx.net_worth_state().await?.rebuild_needed {
            let rows = tx.rebuild_net_worth(today).await?;
            tx.commit().await?;
            tracing::info!(rows, through = %today, "the net worth table was rebuilt");
            Ok(())
        } else {
            tx.fill_net_worth(today).await?;
            tx.commit().await
        }
    }
}

impl FxService {
    /// Says, at most once a day, that the table's last day is more than a day
    /// past today. A `COFFER_TIMEZONE` changed at a restart can do one day; more is
    /// a clock that ran ahead, which nothing here undoes, since a fill never moves
    /// back. A zone chosen in the settings leaves no such table.
    fn say_if_far_ahead(&self, filled: Date, today: Date) {
        if today.tomorrow().is_ok_and(|tomorrow| filled <= tomorrow) {
            return;
        }
        let mut said_on = self
            .ahead_said_on
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if *said_on != Some(today) {
            *said_on = Some(today);
            tracing::warn!(
                through = %filled,
                today = %today,
                "the net worth table runs past today, as after a clock that ran ahead; \
                 `coffer rebuild-net-worth` resets it"
            );
        }
    }
}

/// Starts the jobs, the table's first; they stop once `shutdown` resolves, each
/// dropped at its next await point, a fetch or a transaction included. With no
/// provider, the rate task doesn't start. Nothing here delays the caller: a
/// rebuild waiting since a migration runs in its task while the server serves.
pub fn spawn(
    fx: Arc<FxService>,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let (stop, stop_signal) = watch::channel(false);
        let table = tokio::spawn(supervise("net worth table", stop_signal.clone(), {
            let fx = Arc::clone(&fx);
            let stop = stop_signal.clone();
            move |_| {
                let fx = Arc::clone(&fx);
                every_tick("net worth table", stop.clone(), fx.tick, move || {
                    let fx = Arc::clone(&fx);
                    async move { fx.keep_table_current().await }
                })
            }
        }));
        let rates = fx.provider.is_some().then(|| {
            let stop = stop_signal.clone();
            tokio::spawn(supervise("exchange rates", stop_signal, move |_| {
                let fx = Arc::clone(&fx);
                every_tick("exchange rates", stop.clone(), fx.tick, move || {
                    let fx = Arc::clone(&fx);
                    async move { fx.fetch_due().await.map(drop) }
                })
            }))
        });
        shutdown.await;
        // No receiver left means every task has already ended.
        let _ = stop.send(true);
        for (task, handle) in [("net worth table", Some(table)), ("exchange rates", rates)] {
            let Some(handle) = handle else { continue };
            if let Err(err) = handle.await {
                tracing::error!(
                    task,
                    panicked = err.is_panic(),
                    "a background task ended abnormally"
                );
            }
        }
    })
}

/// Runs `pass` at once and then every `tick` until told to stop. After an error,
/// logs it and tries again with a backoff.
async fn every_tick<P, F>(
    task: &'static str,
    mut stop: watch::Receiver<bool>,
    tick: Duration,
    pass: P,
) where
    P: Fn() -> F,
    F: Future<Output = Result<(), StoreError>>,
{
    let mut backoff = Backoff::default();
    loop {
        let outcome = tokio::select! {
            () = stopped(&mut stop) => return,
            outcome = pass() => outcome,
        };
        let delay = match outcome {
            Ok(()) => {
                backoff.reset();
                tick
            }
            Err(err) => {
                tracing::error!(task, error = ?err, "a background job failed; trying again");
                backoff.next()
            }
        };
        if !pause(&mut stop, delay).await {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::time::Duration;

    use coffer_domain::fx::{FxCoverage, FxFetchStatus};
    use coffer_domain::money::Currency;
    use coffer_store::StoreError;
    use jiff::{SignedDuration, Timestamp};
    use tokio::sync::watch;
    use tokio::time::Instant;

    use super::{due, every_tick, needed};

    fn cur(code: &str) -> Currency {
        Currency::parse(code).unwrap()
    }

    fn codes(list: &[&str]) -> Vec<Currency> {
        list.iter().map(|code| cur(code)).collect()
    }

    /// Each error waits longer, a success waits a tick and starts over.
    #[tokio::test(start_paused = true)]
    async fn errors_back_off_and_a_success_starts_over() {
        let (stop, stopped) = watch::channel(false);
        let script = Mutex::new(VecDeque::from([false, false, true, false, true]));
        let passes = Mutex::new(Vec::new());

        every_tick("test", stopped, Duration::from_secs(60), || {
            passes.lock().unwrap().push(Instant::now());
            let succeeded = {
                let mut script = script.lock().unwrap();
                let succeeded = script.pop_front().unwrap();
                if script.is_empty() {
                    stop.send(true).unwrap();
                }
                succeeded
            };
            async move {
                if succeeded {
                    Ok(())
                } else {
                    Err(StoreError::NotFound)
                }
            }
        })
        .await;

        let passes = passes.lock().unwrap();
        let waits: Vec<_> = passes
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).as_secs())
            .collect();
        assert_eq!(waits, [1, 2, 60, 1]);
    }

    #[test]
    fn one_currency_needs_nothing() {
        assert_eq!(needed(&codes(&["USD"]), Some(cur("USD"))), []);
        assert_eq!(needed(&codes(&["JPY"]), None), []);
        assert_eq!(needed(&codes(&["USD", "USD"]), None), []);
        assert_eq!(needed(&codes(&["EUR"]), Some(cur("EUR"))), []);
        assert_eq!(needed(&[], None), []);
        assert_eq!(needed(&[], Some(cur("USD"))), []);
    }

    #[test]
    fn two_currencies_need_both_without_the_euro() {
        assert_eq!(needed(&codes(&["USD", "EUR"]), None), codes(&["USD"]));
        assert_eq!(
            needed(&codes(&["USD", "GBP"]), Some(cur("EUR"))),
            codes(&["GBP", "USD"])
        );
        assert_eq!(
            needed(&codes(&["USD", "GBP"]), None),
            codes(&["GBP", "USD"])
        );
    }

    #[test]
    fn display_currency_counts() {
        assert_eq!(needed(&codes(&["USD"]), Some(cur("EUR"))), codes(&["USD"]));
        assert_eq!(needed(&codes(&["EUR"]), Some(cur("USD"))), codes(&["USD"]));
        assert_eq!(
            needed(&codes(&["USD"]), Some(cur("GBP"))),
            codes(&["GBP", "USD"])
        );
    }

    #[test]
    fn due_rules() {
        let now = Timestamp::from_second(1_800_000_000).unwrap();
        let usd = cur("USD");
        let second = SignedDuration::from_secs(1);
        let row = |status, ago: SignedDuration| FxCoverage {
            currency: usd,
            status,
            first_on: None,
            last_on: None,
            last_attempt_at: now - ago,
            last_success_at: None,
            last_error: None,
        };
        let hours = SignedDuration::from_hours;
        let cases = [
            (FxFetchStatus::Ready, hours(6), true),
            (FxFetchStatus::Ready, hours(6) - second, false),
            (FxFetchStatus::Ready, hours(0), false),
            (FxFetchStatus::Failing, hours(1), true),
            (FxFetchStatus::Failing, hours(1) - second, false),
            (FxFetchStatus::NotPublished, hours(24), true),
            (FxFetchStatus::NotPublished, hours(24) - second, false),
            (FxFetchStatus::NotPublished, hours(6), false),
        ];
        for (status, ago, expected) in cases {
            let coverage = [row(status, ago)];
            assert_eq!(
                due(now, &[usd], &coverage) == vec![usd],
                expected,
                "{status:?} {ago:?}"
            );
        }
        // Never tried: due at once.
        assert_eq!(due(now, &[usd], &[]), vec![usd]);
        // No longer needed: never due, however old its row.
        assert_eq!(due(now, &[], &[row(FxFetchStatus::Failing, hours(48))]), []);
    }

    /// An attempt later than now came from a clock that ran ahead. It says nothing
    /// of when to ask next, and must not silence the currency until that date.
    #[test]
    fn an_attempt_in_the_future_is_due() {
        let now = Timestamp::from_second(1_800_000_000).unwrap();
        let usd = cur("USD");
        for last_attempt_at in [now + SignedDuration::from_secs(1), Timestamp::MAX] {
            let ahead = FxCoverage {
                currency: usd,
                status: FxFetchStatus::Ready,
                first_on: None,
                last_on: None,
                last_attempt_at,
                last_success_at: None,
                last_error: None,
            };
            assert_eq!(due(now, &[usd], &[ahead]), vec![usd], "{last_attempt_at}");
        }
    }
}
