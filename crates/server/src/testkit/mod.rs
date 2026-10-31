//! Test helpers: a temporary server with a manual clock, software passkeys
//! that drive identity ceremonies without HTTP, and a signal for when code
//! under test logs a given event.
//!
//! Compiled only with the `testkit` feature; never part of a release build.

#![expect(
    clippy::expect_used,
    reason = "test harness: a setup step that fails must abort the test that called it"
)]

pub mod browser;

use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use coffer_domain::identity::{DisplayName, TokenName, Username};
use coffer_domain::ids::MemberId;
use coffer_domain::time::{Clock as _, ManualClock, SystemClock};
use coffer_store::Store;
use jiff::Timestamp;
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

use crate::auth::{AgentTokens, Identity, Sessions};
use crate::config::Config;
use crate::secrets::{SecretToken, ServerKey};

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
        let dir = tempfile::tempdir().expect("temporary directory");
        let clock = ManualClock::new(Timestamp::from_second(TEST_START).expect("valid start time"));
        let path = dir.path().to_owned();
        Self::build(&path, Some(dir), clock).await
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
        Self::build(data_dir, None, ManualClock::new(now)).await
    }

    async fn build(data_dir: &Path, dir: Option<TempDir>, clock: ManualClock) -> Self {
        let vars = HashMap::from([
            ("COFFER_PUBLIC_URL", TEST_PUBLIC_URL.to_owned()),
            (
                "COFFER_DATA_DIR",
                data_dir.to_str().expect("UTF-8 temp path").to_owned(),
            ),
        ]);
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
        Self {
            clock,
            store,
            key,
            config,
            identity,
            sessions,
            agent_tokens,
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
            store: self.store.clone(),
            config: self.config.clone(),
            clock: self.clock.clone(),
        }
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
/// The first call installs a process-wide subscriber that only watches for
/// these events. It is global rather than attached to the future under test
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
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        tracing::subscriber::set_global_default(tracing_subscriber::registry().with(Signals))
            .expect("no other global subscriber in a test process");
    });
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

/// Fires the [`Waiter`]s an event matches, then forgets them.
struct Signals;

impl<S: Subscriber> Layer<S> for Signals {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut waiters = waiters();
        if waiters.is_empty() {
            return;
        }
        let mut fields = Fields(HashMap::new());
        event.record(&mut fields);
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
