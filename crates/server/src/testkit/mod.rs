//! Test helpers: a temporary server with a manual clock, software passkeys
//! that drive identity ceremonies without HTTP, a scripted data provider,
//! a capture of every log event, and a signal for when code under test logs
//! a given event.
//!
//! Compiled only with the `testkit` feature; never part of a release build.

#![expect(
    clippy::expect_used,
    reason = "test harness: a setup step that fails must abort the test that called it"
)]

pub mod browser;

use std::collections::{HashMap, VecDeque};
use std::fmt::{self, Write as _};
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
use crate::egress::Egress;
use crate::http::idempotency::RequestHasher;
use crate::ledger::Ledger;
use crate::secrets::{SecretToken, ServerKey};
use crate::sync::engine::SyncService;
use crate::sync::provider::{
    Capabilities, ConnectInput, FetchMode, FetchRequest, FetchResult, Provider, ProviderError,
    ProviderFuture, ProviderKind, ProviderRegistry,
};
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
        let registry = registry(&self.config).with(provider);
        self.sync = Arc::new(SyncService::new(
            self.store.clone(),
            self.clock.clone(),
            Arc::clone(&self.vault),
            Arc::new(registry),
            self.config.clone(),
        ));
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
        let sync = Arc::new(SyncService::new(
            store.clone(),
            clock.clone(),
            Arc::clone(&vault),
            Arc::new(registry(&config)),
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
            access: Arc::new(AccessResolver::new()),
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
            ledger: Arc::new(Ledger::new(
                self.store.clone(),
                self.clock.clone(),
                Arc::clone(&self.access),
            )),
            access: Arc::clone(&self.access),
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
        }
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
        Box::pin(std::future::ready(Ok(token)))
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
        Box::pin(std::future::ready(response))
    }

    fn disconnect<'a>(
        &'a self,
        _credentials: &'a SecretString,
    ) -> ProviderFuture<'a, Result<(), ProviderError>> {
        Box::pin(std::future::ready(Ok(())))
    }
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

/// Every event logged in this process since [`capture_logs`] ran, one per
/// line: its level, its message and its other fields. Tests running in
/// parallel share it, so a test looks for what identifies its own records.
#[must_use]
pub fn captured_log_text() -> String {
    captured().clone()
}

fn captured() -> MutexGuard<'static, String> {
    static CAPTURED: Mutex<String> = Mutex::new(String::new());
    CAPTURED.lock().unwrap_or_else(PoisonError::into_inner)
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

/// Appends every event to the capture, then fires the [`Waiter`]s it
/// matches and forgets them.
struct Signals;

impl<S: Subscriber> Layer<S> for Signals {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut fields = Fields(HashMap::new());
        event.record(&mut fields);
        {
            let mut captured = captured();
            let message = fields.0.get("message").map_or("", String::as_str);
            let _ = write!(captured, "{} {message}", event.metadata().level());
            for (name, value) in &fields.0 {
                if *name != "message" {
                    let _ = write!(captured, " {name}={value}");
                }
            }
            captured.push('\n');
        }
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
