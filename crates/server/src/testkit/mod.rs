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
mod fake_provider;
mod faults;
mod log_capture;
mod passkeys;
mod upstream;

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use coffer_domain::accounts::ConnectionLabel;
use coffer_domain::ids::{ConnectionId, MemberId};
use coffer_domain::time::{Clock, ManualClock, SystemClock};
pub use coffer_store::{
    INJECTED_BIND_RECORD_ID, INJECTED_FAILURE_LABEL, INJECTED_FAILURE_RECORD_ID,
    INJECTED_FAULT_RECORD_ID, InjectedRunUpdate,
};
use coffer_store::{NewConnection, Store};
use jiff::Timestamp;
use secrecy::SecretString;
use tempfile::TempDir;

use crate::appearance::Appearance;
use crate::auth::{AccessResolver, AgentTokens, Identity, Sessions};
use crate::config::{Config, PlaidConfig, PlaidEnvironment};
use crate::egress::{Egress, HostAllowList};
use crate::fx::FxService;
use crate::http::idempotency::RequestHasher;
use crate::ledger::Ledger;
use crate::secrets::ServerKey;
use crate::sync::engine::{FETCH_DEADLINE, SyncService};
use crate::sync::plaid::client::PlaidClient;
use crate::sync::plaid::provider::Plaid;
use crate::sync::provider::{Provider, ProviderKind, ProviderRegistry};
use crate::sync::simplefin::SimpleFin;
use crate::vault::CredentialVault;

pub use fake_provider::{CallGate, FakeFetch, FakeProvider, SCRIPTED_PANIC};
pub use log_capture::{capture_logs, captured_log_text, event_signal, log_marker};
pub use passkeys::{SoftAuthenticator, link_token};
pub use upstream::{MockUpstream, RecordedRequest};

/// Public URL (and `WebAuthn` origin) of the test server.
pub const TEST_PUBLIC_URL: &str = "https://coffer.test";

/// Time the test clock starts at, Unix seconds.
pub const TEST_START: i64 = 1_800_000_000;

/// A server's state in a temporary directory, with a clock that only moves
/// when told to.
pub struct TestServer {
    /// Advance this to move time for every service. On a server from
    /// [`TestServer::with_system_clock`] the services don't read it.
    pub clock: Arc<ManualClock>,
    services_clock: Arc<dyn Clock>,
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
        Self::build(&path, Some(dir), clock, false, vars).await
    }

    /// A fresh, empty server whose services read the real clock, as in production, so what
    /// a save answers can be checked against what the store rounds. Moving
    /// [`TestServer::clock`] changes nothing here.
    ///
    /// # Panics
    ///
    /// As [`TestServer::new`].
    pub async fn with_system_clock() -> Self {
        let dir = tempfile::tempdir().expect("temporary directory");
        let clock = ManualClock::new(SystemClock.now());
        let path = dir.path().to_owned();
        Self::build(&path, Some(dir), clock, true, &[]).await
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
                Arc::clone(&self.services_clock),
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
                created_at: self.services_clock.now(),
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

    /// A server over an existing data directory (for example one a CLI
    /// command just initialised), with the clock set to the real time so
    /// that links issued by the real clock are still valid.
    ///
    /// # Panics
    ///
    /// If the database, key or service cannot be opened.
    pub async fn open_at(data_dir: &Path) -> Self {
        let now = SystemClock.now();
        Self::build(data_dir, None, ManualClock::new(now), false, &[]).await
    }

    async fn build(
        data_dir: &Path,
        dir: Option<TempDir>,
        clock: ManualClock,
        real_time: bool,
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
        let manual = Arc::new(clock);
        let clock: Arc<dyn Clock> = if real_time {
            Arc::new(SystemClock)
        } else {
            manual.clone()
        };
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
            config.default_timezone.clone(),
            config.fx_provider,
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
            clock: manual,
            services_clock: clock,
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
            fx: Arc::new(FxService::new(
                self.store.clone(),
                Arc::clone(&self.services_clock),
                self.config.default_timezone.clone(),
                None,
            )),
            idempotency: Arc::new(RequestHasher::new(&self.key)),
            store: self.store.clone(),
            config: self.config.clone(),
            clock: Arc::clone(&self.services_clock),
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

    /// Stores a category name the schema would refuse (empty) for the category with this
    /// id, as a database edited with constraint checks off could hold, so listing the
    /// categories fails.
    ///
    /// # Panics
    ///
    /// If the write fails.
    pub async fn corrupt_category_name(&self, category: &str) {
        self.store
            .execute_unchecked_for_tests("UPDATE categories SET name = '' WHERE id = ?", category)
            .await
            .expect("write an unchecked category name");
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

/// Locks `mutex`, recovering the data of a test that panicked holding it.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
