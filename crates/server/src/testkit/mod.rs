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

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use coffer_domain::accounts::ConnectionLabel;
use coffer_domain::identity::{DisplayName, TokenName, Username};
use coffer_domain::ids::{ConnectionId, MemberId};
use coffer_domain::time::{Clock as _, ManualClock, SystemClock};
pub use coffer_store::INJECTED_FAILURE_RECORD_ID;
use coffer_store::{NewConnection, Store};
use jiff::civil::Date;
use jiff::{SignedDuration, Timestamp};
use secrecy::SecretString;
use tempfile::TempDir;
use tokio::sync::Notify;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt as _};
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
use crate::config::Config;
use crate::egress::{Egress, HostAllowList};
use crate::http::idempotency::RequestHasher;
use crate::ledger::Ledger;
use crate::secrets::{SecretToken, ServerKey};
use crate::sync::engine::SyncService;
use crate::sync::provider::{
    Capabilities, ConnectInput, FetchMode, FetchRequest, FetchResult, Provider, ProviderError,
    ProviderFuture, ProviderKind, ProviderRegistry,
};
use crate::sync::simplefin::SimpleFin;
use crate::vault::CredentialVault;

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
        self.use_registry(registry(&self.config).with(provider));
    }

    /// Makes the sync engine offer no provider at all, as a server whose
    /// configuration no longer enables a connection's provider.
    pub fn offer_no_provider(&mut self) {
        self.use_registry(ProviderRegistry::default());
    }

    fn use_registry(&mut self, registry: ProviderRegistry) {
        self.sync = Arc::new(SyncService::new(
            self.store.clone(),
            self.clock.clone(),
            Arc::clone(&self.vault),
            Arc::new(registry),
            Arc::clone(&self.access),
            self.config.clone(),
        ));
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
        let access = Arc::new(AccessResolver::new());
        let ledger = Arc::new(Ledger::new(
            store.clone(),
            clock.clone(),
            Arc::clone(&access),
        ));
        let sync = Arc::new(SyncService::new(
            store.clone(),
            clock.clone(),
            Arc::clone(&vault),
            Arc::new(registry(&config)),
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
/// test egress client.
fn registry(config: &Config) -> ProviderRegistry {
    ProviderRegistry::from_config(config, &Egress::for_tests()).expect("provider registry")
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
/// windowed provider: 90 days at most, 14 days of overlap.
pub struct FakeProvider {
    capabilities: Capabilities,
    responses: Mutex<VecDeque<Result<FetchResult, ProviderError>>>,
    requests: Mutex<Vec<FakeFetch>>,
    fetch_gate: Mutex<Option<CallGate>>,
    connect_gate: Mutex<Option<CallGate>>,
    disconnect_gate: Mutex<Option<CallGate>>,
    disconnect_error: Mutex<Option<ProviderError>>,
    disconnects: Mutex<Vec<SecretString>>,
}

/// Holds the next call of one kind to a [`FakeProvider`] until released
/// (see [`FakeProvider::hold_next_fetch`]).
#[derive(Debug, Clone)]
pub struct CallGate {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

impl CallGate {
    fn new() -> Self {
        Self {
            entered: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
        }
    }

    /// Signals that the held call started, then waits for its release.
    async fn hold(self) {
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
                max_days: 90,
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
        let ConnectInput::SimpleFinSetupToken(token) = input;
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
        let response = self
            .responses
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front()
            .unwrap_or_else(|| Err(ProviderError::Unavailable("no scripted response".into())));
        let gate = lock(&self.fetch_gate).take();
        Box::pin(async move {
            if let Some(gate) = gate {
                gate.hold().await;
            }
            response
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
    let gate = CallGate::new();
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

/// A signal that fires once code under test, on any thread, logs the event
/// `message` whose field `field` displays as `value`. Await it to know that
/// the code has reached that point, instead of sleeping. Matching a field
/// that identifies the record under test keeps tests running in parallel
/// from tripping each other's signals.
///
/// It relies on the process-wide subscriber [`capture_logs`] installs
/// (installing it if needed). It is global rather than attached to the future under test
/// because tracing caches whether a log statement is enabled the first time
/// any thread reaches it: a scoped subscriber can miss statements first
/// reached by another test. Each call also rebuilds that cache, so a
/// statement reached before the subscriber was installed is watched too.
///
/// # Panics
///
/// If another global subscriber was installed in this process.
#[must_use]
pub fn event_signal(
    message: &'static str,
    field: &'static str,
    value: impl fmt::Display,
) -> Arc<Notify> {
    capture_logs();
    let reached = Arc::new(Notify::new());
    waiters().push(Waiter {
        message,
        field,
        value: value.to_string(),
        reached: Arc::clone(&reached),
    });
    tracing::callsite::rebuild_interest_cache();
    reached
}

/// Installs, once per process, the global subscriber that captures every
/// log event (read with [`captured_log_text`]) and fires
/// [`event_signal`]s. Every [`TestServer`] constructor calls it.
///
/// # Panics
///
/// If another global subscriber was installed in this process.
pub fn capture_logs() {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        tracing::subscriber::set_global_default(tracing_subscriber::registry().with(Signals))
            .expect("no other global subscriber in a test process");
    });
}

/// Every event and span logged in this process since [`capture_logs`]
/// ran, one per line: an event's level, message and other fields; a span's
/// name and fields, and fields recorded on it later. Only the newest 16 MiB
/// are kept. Tests running in parallel share it, so a test looks for what
/// identifies its own records.
#[must_use]
pub fn captured_log_text() -> String {
    captured().text().to_owned()
}

/// The most text the capture keeps, in bytes.
const CAPTURE_LIMIT: usize = 16 * 1024 * 1024;

fn captured() -> MutexGuard<'static, CapturedLog> {
    static CAPTURED: Mutex<CapturedLog> = Mutex::new(CapturedLog::with_limit(CAPTURE_LIMIT));
    CAPTURED.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Captured log lines, of which only the newest `limit` bytes are kept.
struct CapturedLog {
    text: String,
    limit: usize,
}

impl CapturedLog {
    const fn with_limit(limit: usize) -> Self {
        Self {
            text: String::new(),
            limit,
        }
    }

    fn text(&self) -> &str {
        &self.text
    }

    /// Appends `line` and a newline, then drops the oldest whole lines
    /// beyond the limit. A line longer than the limit is cut to fit.
    fn push_line(&mut self, line: &str) {
        let room = self.limit.saturating_sub(1);
        let mut end = line.len().min(room);
        while !line.is_char_boundary(end) {
            end -= 1;
        }
        self.text.push_str(&line[..end]);
        self.text.push('\n');
        if let Some(excess) = self.text.len().checked_sub(self.limit).filter(|&e| e > 0) {
            // Newlines are always character boundaries.
            let cut = self.text.as_bytes()[excess..]
                .iter()
                .position(|&byte| byte == b'\n')
                .map_or(self.text.len(), |at| excess + at + 1);
            self.text.drain(..cut);
        }
    }
}

/// One capture line: `head`, then the message, then the other fields.
fn capture_line(head: &str, fields: &Fields) -> String {
    let mut line = head.to_owned();
    if let Some(message) = fields.0.get("message") {
        line.push(' ');
        line.push_str(message);
    }
    for (name, value) in &fields.0 {
        if *name != "message" {
            line.push(' ');
            line.push_str(name);
            line.push('=');
            line.push_str(value);
        }
    }
    line
}

/// An [`event_signal`] not yet fired.
struct Waiter {
    message: &'static str,
    field: &'static str,
    value: String,
    reached: Arc<Notify>,
}

fn waiters() -> MutexGuard<'static, Vec<Waiter>> {
    static WAITERS: Mutex<Vec<Waiter>> = Mutex::new(Vec::new());
    WAITERS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Appends every event and span to the capture, and fires the
/// [`Waiter`]s an event matches, then forgets them.
struct Signals;

impl<S: Subscriber> Layer<S> for Signals {
    fn on_new_span(&self, attrs: &Attributes<'_>, _id: &Id, _ctx: Context<'_, S>) {
        let mut fields = Fields(HashMap::new());
        attrs.record(&mut fields);
        let head = format!("SPAN {}", attrs.metadata().name());
        captured().push_line(&capture_line(&head, &fields));
    }

    fn on_record(&self, _id: &Id, values: &Record<'_>, _ctx: Context<'_, S>) {
        let mut fields = Fields(HashMap::new());
        values.record(&mut fields);
        captured().push_line(&capture_line("SPAN RECORD", &fields));
    }

    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut fields = Fields(HashMap::new());
        event.record(&mut fields);
        let head = event.metadata().level().to_string();
        captured().push_line(&capture_line(&head, &fields));
        let mut waiters = waiters();
        if waiters.is_empty() {
            return;
        }
        waiters.retain(|waiter| {
            let matched = fields.0.get("message").map(String::as_str) == Some(waiter.message)
                && fields.0.get(waiter.field) == Some(&waiter.value);
            if matched {
                waiter.reached.notify_one();
            }
            !matched
        });
    }
}

/// An event's fields, by name, as their `Debug` text (a `%`-captured value
/// shows its `Display` text).
struct Fields(HashMap<&'static str, String>);

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.0.insert(field.name(), format!("{value:?}"));
    }
}

#[cfg(test)]
mod tests {
    use super::CapturedLog;

    #[test]
    fn capture_keeps_the_newest_lines_within_its_bound() {
        let mut log = CapturedLog::with_limit(100);
        for i in 0..50 {
            log.push_line(&format!("line {i:03}"));
        }
        assert!(log.text().len() <= 100, "the bound holds");
        assert!(
            log.text().ends_with("line 049\n"),
            "the newest line is kept"
        );
        assert!(log.text().starts_with("line "), "cut at a line boundary");
        assert!(!log.text().contains("line 000"), "the oldest lines go");
        // A line longer than the bound is kept alone, cut to it.
        log.push_line(&"x".repeat(300));
        assert!(log.text().len() <= 100);
    }
}
