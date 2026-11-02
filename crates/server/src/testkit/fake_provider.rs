//! A scripted data provider, whose calls a test can hold, fail or panic.

use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use jiff::SignedDuration;
use jiff::civil::Date;
use secrecy::SecretString;
use tokio::sync::Notify;

use super::lock;
use crate::sync::provider::{
    Capabilities, ConnectInput, FetchMode, FetchRequest, FetchResult, Provider, ProviderError,
    ProviderFuture, ProviderKind,
};

/// One fetch a [`FakeProvider`] received.
#[derive(Debug, Clone)]
pub struct FakeFetch {
    /// The opened credentials it was given.
    pub credentials: SecretString,
    pub window_start: Option<Date>,
    pub cursor: Option<String>,
    pub today: Date,
    /// The IANA name of the time zone it was given.
    pub timezone: String,
}

/// A [`ProviderKind::SimpleFin`] provider that answers fetches from a
/// script and records each request. Its capabilities are those of a
/// windowed provider: 88 days at most, 14 days of overlap.
pub struct FakeProvider {
    capabilities: Capabilities,
    responses: Mutex<VecDeque<Result<FetchResult, ProviderError>>>,
    requests: Mutex<Vec<FakeFetch>>,
    fetch_gate: Mutex<Option<CallGate>>,
    fetch_panics: Mutex<bool>,
    connect_gate: Mutex<Option<CallGate>>,
    disconnect_gate: Mutex<Option<CallGate>>,
    disconnect_error: Mutex<Option<ProviderError>>,
    disconnects: Mutex<Vec<SecretString>>,
}

/// Holds the next call of one kind until released: to a [`FakeProvider`]
/// (see [`FakeProvider::hold_next_fetch`]), or to a test's own mock upstream.
#[derive(Debug, Clone, Default)]
pub struct CallGate {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

impl CallGate {
    /// Signals that the held call started, then waits for its release.
    pub async fn hold(self) {
        self.entered.notify_one();
        self.release.notified().await;
    }

    /// Waits until the held call has started.
    pub async fn entered(&self) {
        self.entered.notified().await;
    }

    /// Lets the held call answer.
    pub fn release(&self) {
        self.release.notify_one();
    }
}

impl fmt::Debug for FakeProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FakeProvider").finish_non_exhaustive()
    }
}

impl Default for FakeProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeProvider {
    /// A provider with an empty script.
    #[must_use]
    pub fn new() -> Self {
        Self::with_capabilities(Capabilities {
            pending: true,
            mode: FetchMode::Window {
                max_days: 88,
                overlap_days: 14,
            },
            min_interval: SignedDuration::from_hours(1),
            min_scheduled_interval: SignedDuration::from_hours(2),
            daily_budget: 16,
        })
    }

    /// A provider with an empty script and `capabilities`.
    #[must_use]
    pub fn with_capabilities(capabilities: Capabilities) -> Self {
        Self {
            capabilities,
            responses: Mutex::new(VecDeque::new()),
            requests: Mutex::new(Vec::new()),
            fetch_gate: Mutex::new(None),
            fetch_panics: Mutex::new(false),
            connect_gate: Mutex::new(None),
            disconnect_gate: Mutex::new(None),
            disconnect_error: Mutex::new(None),
            disconnects: Mutex::new(Vec::new()),
        }
    }

    /// Makes the next fetch wait, once it has started, until the returned
    /// gate is released, so a test can act while a sync is mid-fetch.
    #[must_use]
    pub fn hold_next_fetch(&self) -> CallGate {
        hold(&self.fetch_gate)
    }

    /// Makes the next fetch panic, as a bug in a provider would.
    pub fn panic_next_fetch(&self) {
        *lock(&self.fetch_panics) = true;
    }

    /// Makes the next `connect` wait, once it has started, until the
    /// returned gate is released.
    #[must_use]
    pub fn hold_next_connect(&self) -> CallGate {
        hold(&self.connect_gate)
    }

    /// Makes the next `disconnect` wait, once it has started, until the
    /// returned gate is released.
    #[must_use]
    pub fn hold_next_disconnect(&self) -> CallGate {
        hold(&self.disconnect_gate)
    }

    /// Makes the next `disconnect` fail with `error`.
    pub fn fail_next_disconnect(&self, error: ProviderError) {
        *lock(&self.disconnect_error) = Some(error);
    }

    /// The credentials of every `disconnect` received so far, oldest first.
    #[must_use]
    pub fn disconnects(&self) -> Vec<SecretString> {
        lock(&self.disconnects).clone()
    }

    /// Adds the answer to the next fetch not yet scripted.
    pub fn push(&self, response: Result<FetchResult, ProviderError>) {
        self.responses
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_back(response);
    }

    /// Every fetch received so far, oldest first.
    #[must_use]
    pub fn requests(&self) -> Vec<FakeFetch> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Provider for FakeProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::SimpleFin
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities
    }

    fn connect(
        &self,
        input: ConnectInput,
    ) -> ProviderFuture<'_, Result<SecretString, ProviderError>> {
        let (ConnectInput::SimpleFinSetupToken(token) | ConnectInput::PlaidPublicToken(token)) =
            input;
        let gate = lock(&self.connect_gate).take();
        Box::pin(async move {
            if let Some(gate) = gate {
                gate.hold().await;
            }
            Ok(token)
        })
    }

    fn fetch<'a>(
        &'a self,
        request: FetchRequest<'a>,
    ) -> ProviderFuture<'a, Result<FetchResult, ProviderError>> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(FakeFetch {
                credentials: request.credentials.clone(),
                window_start: request.window_start,
                cursor: request.cursor.map(str::to_owned),
                today: request.today,
                timezone: request.timezone.iana_name().unwrap_or_default().to_owned(),
            });
        // A panicking fetch leaves the script for the next one.
        let panics = std::mem::take(&mut *lock(&self.fetch_panics));
        let response = (!panics).then(|| {
            self.responses
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .pop_front()
                .unwrap_or_else(|| Err(ProviderError::Unavailable("no scripted response".into())))
        });
        let gate = lock(&self.fetch_gate).take();
        Box::pin(async move {
            if let Some(gate) = gate {
                gate.hold().await;
            }
            match response {
                Some(response) => response,
                None => panic!("scripted provider panic"),
            }
        })
    }

    fn disconnect<'a>(
        &'a self,
        credentials: &'a SecretString,
    ) -> ProviderFuture<'a, Result<(), ProviderError>> {
        lock(&self.disconnects).push(credentials.clone());
        let result = match lock(&self.disconnect_error).take() {
            Some(error) => Err(error),
            None => Ok(()),
        };
        let gate = lock(&self.disconnect_gate).take();
        Box::pin(async move {
            if let Some(gate) = gate {
                gate.hold().await;
            }
            result
        })
    }
}

/// Installs a new gate in `slot` and returns it.
fn hold(slot: &Mutex<Option<CallGate>>) -> CallGate {
    let gate = CallGate::default();
    *lock(slot) = Some(gate.clone());
    gate
}
