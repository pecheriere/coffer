//! Test helpers: a temporary server with a manual clock, software passkeys
//! that drive identity ceremonies without HTTP, a scripted data provider,
//! a loopback mock of a provider's upstream, a capture of every log event,
//! and a signal for when code under test logs a given event.
//!
//! Compiled only with the `testkit` feature; never part of a release build.

#![expect(
    clippy::expect_used,
    reason = "test harness: a setup step that fails must abort the test that called it"
)]

pub mod browser;
mod log_capture;

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use coffer_domain::accounts::ConnectionLabel;
use coffer_domain::identity::{DisplayName, TokenName, Username};
use coffer_domain::ids::{ConnectionId, MemberId};
use coffer_domain::time::{Clock as _, ManualClock, SystemClock};
pub use coffer_store::{
    INJECTED_BIND_RECORD_ID, INJECTED_FAILURE_LABEL, INJECTED_FAILURE_RECORD_ID,
    INJECTED_FAULT_RECORD_ID,
};
use coffer_store::{NewConnection, Store};
use jiff::civil::Date;
use jiff::{SignedDuration, Timestamp};
use secrecy::SecretString;
use tempfile::TempDir;
use tokio::sync::Notify;
use url::Url;
use webauthn_authenticator_rs::WebauthnAuthenticator;
use webauthn_authenticator_rs::error::WebauthnCError;
use webauthn_authenticator_rs::softpasskey::SoftPasskey;
use webauthn_rs::prelude::{
    CreationChallengeResponse, PublicKeyCredential, RegisterPublicKeyCredential,
    RequestChallengeResponse,
};

use crate::appearance::Appearance;
use crate::auth::{AccessResolver, AgentTokens, Identity, Sessions};
use crate::config::{Config, PlaidConfig, PlaidEnvironment};
use crate::egress::{Egress, HostAllowList};
use crate::http::idempotency::RequestHasher;
use crate::ledger::Ledger;
use crate::secrets::{SecretToken, ServerKey};
use crate::sync::engine::{FETCH_DEADLINE, SyncService};
use crate::sync::plaid::client::PlaidClient;
use crate::sync::plaid::provider::Plaid;
use crate::sync::provider::{
    Capabilities, ConnectInput, FetchMode, FetchRequest, FetchResult, Provider, ProviderError,
    ProviderFuture, ProviderKind, ProviderRegistry,
};
use crate::sync::simplefin::SimpleFin;
use crate::vault::CredentialVault;
pub use log_capture::{capture_logs, captured_log_text, event_signal, log_marker};

/// Public URL (and `WebAuthn` origin) of the test server.
pub const TEST_PUBLIC_URL: &str = "https://coffer.test";

/// Time the test clock starts at, Unix seconds.
pub const TEST_START: i64 = 1_800_000_000;

/// A server's state in a temporary directory, with a clock that only moves
/// when told to.
pub struct TestServer {
    /// Advance this to move time for every service.
    pub clock: Arc<ManualClock>,
    /// The database.
    pub store: Store,
    /// The server key.
    pub key: Arc<ServerKey>,
    /// Configuration as read from a test environment.
    pub config: Config,
    /// The identity service.
    pub identity: Arc<Identity>,
    /// The session service.
    pub sessions: Arc<Sessions>,
    /// The agent token service.
    pub agent_tokens: Arc<AgentTokens>,
    /// The appearance service.
    pub appearance: Arc<Appearance>,
    /// The account access resolver.
    pub access: Arc<AccessResolver>,
    /// The ledger service.
    pub ledger: Arc<Ledger>,
    /// The provider credential vault, under the server key.
    pub vault: Arc<CredentialVault>,
    /// The sync engine, over the providers registered so far.
    pub sync: Arc<SyncService>,
    registry: ProviderRegistry,
    fetch_deadline: Duration,
    _dir: Option<TempDir>,
}

impl TestServer {
    /// A fresh, empty server.
    ///
    /// # Panics
    ///
    /// If the temporary directory, database, key or service cannot be
    /// created.
    pub async fn new() -> Self {
        Self::with_env(&[]).await
    }

    /// A fresh, empty server whose configuration also reads `vars`, such
    /// as `COFFER_TIMEZONE`.
    ///
    /// # Panics
    ///
    /// If the temporary directory, database, key or service cannot be
    /// created, or `vars` make the configuration invalid.
    pub async fn with_env(vars: &[(&str, &str)]) -> Self {
        let dir = tempfile::tempdir().expect("temporary directory");
        let clock = ManualClock::new(Timestamp::from_second(TEST_START).expect("valid start time"));
        let path = dir.path().to_owned();
        Self::build(&path, Some(dir), clock, vars).await
    }

    /// A fresh, empty server whose sync engine uses a [`FakeProvider`] for
    /// [`ProviderKind::SimpleFin`] connections.
    ///
    /// # Panics
    ///
    /// As [`TestServer::new`].
    pub async fn with_fake_provider() -> (Self, Arc<FakeProvider>) {
        let mut server = Self::new().await;
        let fake = server.use_fake_provider();
        (server, fake)
    }

    /// Registers a new [`FakeProvider`] with the sync engine, replacing any
    /// provider of its kind, and returns it for scripting.
    pub fn use_fake_provider(&mut self) -> Arc<FakeProvider> {
        let fake = Arc::new(FakeProvider::new());
        self.use_provider(Arc::clone(&fake) as Arc<dyn Provider>);
        fake
    }

    /// Registers `provider` with the sync engine, replacing any provider of
    /// its kind.
    pub fn use_provider(&mut self, provider: Arc<dyn Provider>) {
        self.use_registry(self.registry.clone().with(provider));
    }

    /// Makes the sync engine offer no provider at all, as a server whose
    /// configuration no longer enables a connection's provider.
    pub fn offer_no_provider(&mut self) {
        self.use_registry(ProviderRegistry::default());
    }

    /// Limits the sync engine's provider fetches to `deadline`, for tests
    /// that wait for them to run out of time.
    pub fn limit_fetch_time(&mut self, deadline: Duration) {
        self.fetch_deadline = deadline;
        self.use_registry(self.registry.clone());
    }

    fn use_registry(&mut self, registry: ProviderRegistry) {
        self.registry = registry.clone();
        self.sync = Arc::new(
            SyncService::new(
                self.store.clone(),
                self.clock.clone(),
                Arc::clone(&self.vault),
                Arc::new(registry),
                Arc::clone(&self.access),
                self.config.clone(),
            )
            .with_fetch_deadline(self.fetch_deadline),
        );
    }

    /// This server with its `SimpleFin` provider talking to `mock` only: a
    /// production [`SimpleFin`] over [`Egress::for_tests`], whose
    /// allow-list is the mock's loopback port.
    ///
    /// # Panics
    ///
    /// If the mock's URL has no port.
    #[must_use]
    pub fn with_simplefin(mut self, mock: &MockUpstream) -> Self {
        let port = mock.base.port().expect("the mock listens on a port");
        self.use_provider(Arc::new(SimpleFin::new(
            Egress::for_tests(),
            HostAllowList::loopback(port),
        )));
        self
    }

    /// This server configured for Plaid (sandbox), with a provider talking
    /// to `mock` only, beside the providers registered so far.
    #[must_use]
    pub fn with_plaid(mut self, mock: &MockUpstream) -> Self {
        let config = PlaidConfig {
            client_id: "test-client-id".to_owned(),
            secret: SecretString::from("test-plaid-secret"),
            environment: PlaidEnvironment::Sandbox,
            country_codes: vec!["US".to_owned()],
        };
        self.config.plaid = Some(config.clone());
        self.use_provider(Arc::new(Plaid::new(PlaidClient::for_tests(mock, config))));
        self
    }

    /// Creates a connection to `kind` owned by `owner`, with `plaintext`
    /// sealed as its credentials under this server's vault.
    ///
    /// # Panics
    ///
    /// If the label is invalid or a write fails.
    pub async fn seed_connection(
        &self,
        kind: ProviderKind,
        label: &str,
        owner: MemberId,
        plaintext: &str,
    ) -> ConnectionId {
        let mut tx = self.store.begin().await.expect("begin");
        let connection = tx
            .insert_connection(NewConnection {
                provider: kind.as_str(),
                label: ConnectionLabel::parse(label).expect("valid connection label"),
                owner,
                credentials: Vec::new(),
                created_at: self.clock.now(),
            })
            .await
            .expect("insert connection");
        let sealed = self
            .vault
            .seal(connection.id, &SecretString::from(plaintext))
            .expect("seal credentials");
        tx.set_connection_credentials(connection.id, Some(sealed.into()))
            .await
            .expect("store sealed credentials");
        tx.commit().await.expect("commit");
        connection.id
    }

    /// Makes every later write of a provider record with the id
    /// [`INJECTED_FAILURE_RECORD_ID`] fail, as a storage failure would.
    ///
    /// # Panics
    ///
    /// If the failure cannot be installed.
    pub async fn inject_source_insert_failure(&self) {
        self.store
            .inject_source_insert_failure_for_tests()
            .await
            .expect("install the injected failure");
    }

    /// Makes every later insertion of a source record with the provider id
    /// [`INJECTED_FAULT_RECORD_ID`] fail as a database fault would, until
    /// [`Self::clear_source_insert_fault`].
    ///
    /// # Panics
    ///
    /// If the failure cannot be installed.
    pub async fn inject_source_insert_fault(&self) {
        self.store
            .inject_source_insert_fault_for_tests()
            .await
            .expect("install the injected failure");
    }

    /// Ends [`Self::inject_source_insert_fault`].
    ///
    /// # Panics
    ///
    /// If the failure cannot be removed.
    pub async fn clear_source_insert_fault(&self) {
        self.store
            .clear_source_insert_fault_for_tests()
            .await
            .expect("remove the injected failure");
    }

    /// Makes the record [`INJECTED_BIND_RECORD_ID`] bind its provider account
    /// to the database's one account as it is stored.
    ///
    /// # Panics
    ///
    /// If the trigger cannot be installed.
    pub async fn inject_bind_on_source_insert(&self) {
        self.store
            .inject_bind_on_source_insert_for_tests()
            .await
            .expect("install the injected binding");
    }

    /// Makes every later insertion of a connection labelled
    /// [`INJECTED_FAILURE_LABEL`] fail, as a storage failure would.
    ///
    /// # Panics
    ///
    /// If the failure cannot be installed.
    pub async fn inject_connection_insert_failure(&self) {
        self.store
            .inject_connection_insert_failure_for_tests()
            .await
            .expect("install the injected failure");
    }

    /// A server over an existing data directory (for example one a CLI
    /// command just initialised), with the clock set to the real time so
    /// that links issued by the real clock are still valid.
    ///
    /// # Panics
    ///
    /// If the database, key or service cannot be opened.
    pub async fn open_at(data_dir: &Path) -> Self {
        let now = SystemClock.now();
        Self::build(data_dir, None, ManualClock::new(now), &[]).await
    }

    async fn build(
        data_dir: &Path,
        dir: Option<TempDir>,
        clock: ManualClock,
        extra: &[(&str, &str)],
    ) -> Self {
        capture_logs();
        let mut vars = HashMap::from([
            ("COFFER_PUBLIC_URL", TEST_PUBLIC_URL.to_owned()),
            (
                "COFFER_DATA_DIR",
                data_dir.to_str().expect("UTF-8 temp path").to_owned(),
            ),
        ]);
        vars.extend(extra.iter().map(|&(name, value)| (name, value.to_owned())));
        let config = Config::from_env(|name| vars.get(name).cloned()).expect("test config");
        let store = Store::open(&config.database_path())
            .await
            .expect("open store");
        let key = Arc::new(ServerKey::load_or_create(&config.secret_key_file).expect("server key"));
        let clock = Arc::new(clock);
        let identity = Arc::new(
            Identity::new(
                store.clone(),
                Arc::clone(&key),
                clock.clone(),
                config.clone(),
            )
            .expect("identity service"),
        );
        let sessions = Arc::new(Sessions::new(
            store.clone(),
            Arc::clone(&key),
            clock.clone(),
            config.clone(),
        ));
        let agent_tokens = Arc::new(AgentTokens::new(
            store.clone(),
            Arc::clone(&key),
            clock.clone(),
        ));
        let appearance = Arc::new(Appearance::new(store.clone(), clock.clone()));
        let vault = Arc::new(CredentialVault::new(&key));
        let access = Arc::new(AccessResolver::new(store.clone()));
        let ledger = Arc::new(Ledger::new(
            store.clone(),
            clock.clone(),
            Arc::clone(&access),
        ));
        let registry = registry(&config);
        let sync = Arc::new(SyncService::new(
            store.clone(),
            clock.clone(),
            Arc::clone(&vault),
            Arc::new(registry.clone()),
            Arc::clone(&access),
            config.clone(),
        ));
        Self {
            clock,
            store,
            key,
            config,
            identity,
            sessions,
            agent_tokens,
            appearance,
            access,
            ledger,
            vault,
            sync,
            registry,
            fetch_deadline: FETCH_DEADLINE,
            _dir: dir,
        }
    }

    /// The HTTP application state over this server's services.
    #[must_use]
    pub fn app_state(&self) -> crate::http::AppState {
        crate::http::AppState {
            sessions: Arc::clone(&self.sessions),
            agent_tokens: Arc::clone(&self.agent_tokens),
            identity: Arc::clone(&self.identity),
            appearance: Arc::clone(&self.appearance),
            ledger: Arc::clone(&self.ledger),
            access: Arc::clone(&self.access),
            sync: Arc::clone(&self.sync),
            idempotency: Arc::new(RequestHasher::new(&self.key)),
            store: self.store.clone(),
            config: self.config.clone(),
            clock: self.clock.clone(),
        }
    }

    /// Stores a theme name the schema would refuse (`Mochi`, upper case)
    /// for `member`, as a database edited with constraint checks off could
    /// hold.
    ///
    /// # Panics
    ///
    /// If the write fails.
    pub async fn corrupt_member_theme(&self, member: MemberId) {
        self.store
            .execute_unchecked_for_tests(
                "UPDATE members SET theme = 'Mochi' WHERE id = ?",
                &member.to_string(),
            )
            .await
            .expect("write an unchecked theme");
    }

    /// Stores a colour scheme the schema would refuse (`purple`) for
    /// `member`, as a database edited with constraint checks off could hold.
    ///
    /// # Panics
    ///
    /// If the write fails.
    pub async fn corrupt_member_color_scheme(&self, member: MemberId) {
        self.store
            .execute_unchecked_for_tests(
                "UPDATE members SET color_scheme = 'purple' WHERE id = ?",
                &member.to_string(),
            )
            .await
            .expect("write an unchecked colour scheme");
    }

    /// A software passkey that verifies the user, as Face ID would.
    #[must_use]
    pub fn authenticator(&self) -> SoftAuthenticator {
        SoftAuthenticator::new(Arc::clone(&self.identity), Arc::clone(&self.sessions), true)
    }

    /// A software passkey that cannot verify the user.
    #[must_use]
    pub fn authenticator_without_user_verification(&self) -> SoftAuthenticator {
        SoftAuthenticator::new(
            Arc::clone(&self.identity),
            Arc::clone(&self.sessions),
            false,
        )
    }
}

/// The providers the configuration registers, as in production, over a
/// test egress client, each with an allow-list that allows nothing: no
/// test reaches a real provider host. A test points a provider at a
/// loopback mock instead ([`TestServer::with_simplefin`],
/// [`TestServer::with_plaid`]).
fn registry(config: &Config) -> ProviderRegistry {
    let config = Config {
        simplefin_hosts: HostAllowList::none(),
        plaid: None,
        ..config.clone()
    };
    ProviderRegistry::from_config(&config, &Egress::for_tests()).expect("provider registry")
}

/// The message of every panic a [`FakeProvider`] is scripted to raise.
pub const SCRIPTED_PANIC: &str = "scripted provider panic";

/// The message of the panic of [`panic_with_marker`].
pub const PANIC_MARKER: &str = "panic-marker-do-not-log";

/// Panics with [`PANIC_MARKER`], for the `panic-for-tests` command.
///
/// # Panics
///
/// Always.
pub fn panic_with_marker() -> ! {
    panic!("{PANIC_MARKER}")
}

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
    capabilities_panic: Mutex<bool>,
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
            capabilities_panic: Mutex::new(false),
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

    /// Makes the next look at the capabilities panic: on the scheduler's
    /// ticker, outside any run, when nothing else is using the provider.
    pub fn panic_next_capabilities(&self) {
        *lock(&self.capabilities_panic) = true;
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
        assert!(
            !std::mem::take(&mut *lock(&self.capabilities_panic)),
            "{SCRIPTED_PANIC}"
        );
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
                None => panic!("{SCRIPTED_PANIC}"),
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

/// One request a [`MockUpstream`] received. Whether it carried an
/// `Authorization` header is recorded, never its value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedRequest {
    pub method: axum::http::Method,
    pub path: String,
    pub query: Option<String>,
    pub authorization: bool,
}

/// A provider's upstream, mocked: `router` served over plain HTTP on
/// `127.0.0.1` at an ephemeral port, recording every request. Stops when
/// dropped.
pub struct MockUpstream {
    /// `http://127.0.0.1:<port>/`.
    pub base: Url,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    task: tokio::task::JoinHandle<()>,
}

impl fmt::Debug for MockUpstream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MockUpstream")
            .field("base", &self.base.as_str())
            .finish_non_exhaustive()
    }
}

impl MockUpstream {
    /// Starts serving `router`.
    ///
    /// # Panics
    ///
    /// If no loopback port can be bound.
    pub async fn start(router: axum::Router) -> MockUpstream {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind a loopback port");
        let addr = listener.local_addr().expect("bound address");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&requests);
        let app = router.layer(axum::middleware::from_fn(
            move |request: axum::extract::Request, next: axum::middleware::Next| {
                lock(&recorder).push(RecordedRequest {
                    method: request.method().clone(),
                    path: request.uri().path().to_owned(),
                    query: request.uri().query().map(str::to_owned),
                    authorization: request
                        .headers()
                        .contains_key(axum::http::header::AUTHORIZATION),
                });
                next.run(request)
            },
        ));
        let task = tokio::spawn(async move {
            if axum::serve(listener, app).await.is_err() {
                tracing::warn!("the mock upstream stopped");
            }
        });
        MockUpstream {
            base: Url::parse(&format!("http://{addr}/")).expect("loopback URL"),
            requests,
            task,
        }
    }

    /// Every request received so far, oldest first.
    #[must_use]
    pub fn requests(&self) -> Vec<RecordedRequest> {
        lock(&self.requests).clone()
    }
}

impl Drop for MockUpstream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Installs a new gate in `slot` and returns it.
fn hold(slot: &Mutex<Option<CallGate>>) -> CallGate {
    let gate = CallGate::default();
    *lock(slot) = Some(gate.clone());
    gate
}

/// Locks `mutex`, recovering the data of a test that panicked holding it.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The token at the end of a setup, invite or re-enroll link.
///
/// # Panics
///
/// If the link has no path segment.
#[must_use]
pub fn link_token(link: &Url) -> String {
    link.path_segments()
        .and_then(|mut segments| segments.next_back())
        .expect("link ends with a token")
        .to_owned()
}

fn origin() -> Url {
    Url::parse(TEST_PUBLIC_URL).expect("valid test origin")
}

/// A software authenticator bound to a test server's identity service.
pub struct SoftAuthenticator {
    inner: WebauthnAuthenticator<SoftPasskey>,
    identity: Arc<Identity>,
    sessions: Arc<Sessions>,
}

impl SoftAuthenticator {
    fn new(identity: Arc<Identity>, sessions: Arc<Sessions>, verifies_user: bool) -> Self {
        Self {
            inner: WebauthnAuthenticator::new(SoftPasskey::new(verifies_user)),
            identity,
            sessions,
        }
    }

    /// Answers registration `options` as a browser would.
    ///
    /// # Errors
    ///
    /// When the authenticator declines (for example, it cannot verify the
    /// user, or it already holds an excluded credential).
    pub fn register(
        &mut self,
        options: CreationChallengeResponse,
    ) -> Result<RegisterPublicKeyCredential, WebauthnCError> {
        self.inner.do_registration(origin(), options)
    }

    /// Signs authentication `options` as a browser would.
    ///
    /// # Panics
    ///
    /// If the authenticator holds none of the allowed credentials.
    pub fn sign(&mut self, options: RequestChallengeResponse) -> PublicKeyCredential {
        self.inner
            .do_authentication(origin(), options)
            .expect("soft passkey assertion")
    }

    /// Redeems `link` with this authenticator: begin, register, finish.
    /// `username` and `display_name` are ignored for re-enroll links.
    ///
    /// # Panics
    ///
    /// If any step fails.
    pub async fn enroll(&mut self, link: &Url, username: &str, display_name: &str) -> MemberId {
        let (ceremony, options) = self
            .identity
            .begin_enrollment(
                &link_token(link),
                Some(Username::parse(username).expect("valid username")),
                Some(DisplayName::parse(display_name).expect("valid display name")),
            )
            .await
            .expect("begin enrollment");
        let credential = self.register(options).expect("soft passkey registration");
        self.identity
            .finish_enrollment(
                ceremony,
                TokenName::parse("soft passkey").expect("valid label"),
                credential,
            )
            .await
            .expect("finish enrollment")
    }

    /// Signs in as `username`: begin, sign, finish.
    ///
    /// # Panics
    ///
    /// If any step fails.
    pub async fn login(&mut self, username: &str) -> MemberId {
        let (ceremony, options) = self
            .identity
            .begin_login(&Username::parse(username).expect("valid username"))
            .await
            .expect("begin login");
        let credential = self.sign(options);
        self.identity
            .finish_login(ceremony, credential)
            .await
            .expect("finish login")
    }

    /// Signs in as `username` and starts a session, as the login endpoint
    /// does: returns the member and the cookie value.
    ///
    /// # Panics
    ///
    /// If sign-in or session creation fails.
    pub async fn login_with_session(&mut self, username: &str) -> (MemberId, SecretToken) {
        let member = self.login(username).await;
        let cookie = self.sessions.create(member).await.expect("create session");
        (member, cookie)
    }
}
