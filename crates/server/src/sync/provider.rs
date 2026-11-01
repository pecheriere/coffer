//! The provider interface (spec §6.2): what a data source implements so the
//! sync engine can use it, and the registry of the providers this server
//! offers.
//!
//! Providers turn setup input into credentials, fetch accounts, balances
//! and transactions in a normalized shape, and report their errors and
//! limits. They know nothing of categories, review, audit or storage, and
//! never read a clock: the engine hands them the household's `today`.

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;

use coffer_domain::accounts::AccountKind;
use coffer_domain::ids::MemberId;
use coffer_domain::money::DecimalAmount;
use jiff::SignedDuration;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};

use super::apply::Coverage;
use super::plaid::client::PlaidClient;
use super::plaid::provider::Plaid;
use super::simplefin::SimpleFin;
use crate::config::{Config, ConfigError};
use crate::egress::{Egress, EgressError};

/// The longest provider message kept, in characters.
pub const MAX_PROVIDER_MESSAGE_CHARS: usize = 500;

const NO_LINK_FLOW: &str = "this provider is not set up through a browser flow";

/// A data provider coffer can connect to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum ProviderKind {
    /// `SimpleFin` Bridge.
    SimpleFin,
    /// Plaid.
    Plaid,
}

impl ProviderKind {
    /// The provider's name, as stored on its connections.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SimpleFin => "simplefin",
            Self::Plaid => "plaid",
        }
    }
}

impl fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A provider name this server does not know.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("unknown provider")]
pub struct UnknownProvider;

impl FromStr for ProviderKind {
    type Err = UnknownProvider;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "simplefin" => Ok(Self::SimpleFin),
            "plaid" => Ok(Self::Plaid),
            _ => Err(UnknownProvider),
        }
    }
}

/// How a provider reports transactions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchMode {
    /// Every record of a date window. The first sync asks for `max_days`;
    /// later ones start `overlap_days` before the last success, never more
    /// than `max_days` ago.
    Window { max_days: i64, overlap_days: i64 },
    /// Changes since an opaque cursor the provider returned last time.
    Cursor,
}

/// What a provider supports and the limits it imposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// Whether it reports pending transactions.
    pub pending: bool,
    pub mode: FetchMode,
    /// The shortest time between two requested syncs.
    pub min_interval: SignedDuration,
    /// The shortest time between two scheduled syncs.
    pub min_scheduled_interval: SignedDuration,
    /// The most syncs in 24 hours.
    pub daily_budget: u32,
}

/// What a member supplies to connect a provider. Never logged.
#[derive(Debug)]
#[non_exhaustive]
pub enum ConnectInput {
    /// A `SimpleFin` setup token.
    SimpleFinSetupToken(SecretString),
    /// The public token Plaid Link produced.
    PlaidPublicToken(SecretString),
}

/// One fetch, as the engine asks for it.
#[derive(Debug)]
pub struct FetchRequest<'a> {
    /// The opened credentials; they exist in clear only for this call.
    pub credentials: &'a SecretString,
    /// The first day to fetch, for [`FetchMode::Window`].
    pub window_start: Option<Date>,
    /// The provider's cursor from the last success, for
    /// [`FetchMode::Cursor`].
    pub cursor: Option<&'a str>,
    /// The household's current day.
    pub today: Date,
    /// The household's time zone, in which provider timestamps become days.
    pub timezone: &'a TimeZone,
}

/// What a fetch returned.
#[derive(Debug, Clone)]
pub struct FetchResult {
    pub accounts: Vec<ProviderAccountData>,
    /// Errors about the connection as a whole, reported beside the data.
    pub connection_errors: Vec<String>,
    /// The cursor to send next time, for [`FetchMode::Cursor`].
    pub cursor: Option<String>,
}

/// One account as the provider reports it.
#[derive(Debug, Clone)]
pub struct ProviderAccountData {
    /// The provider's id for the account, stable across fetches.
    pub provider_account_id: String,
    pub name: String,
    pub institution: Option<String>,
    /// The currency as given; checked by the engine.
    pub currency: String,
    pub kind_hint: Option<AccountKind>,
    /// The balance and the day it describes.
    pub balance: Option<(Date, DecimalAmount)>,
    pub transactions: Vec<ProviderTransaction>,
    /// Provider ids of records the provider says are gone.
    pub removed: Vec<String>,
    /// Which records this fetch covers.
    pub coverage: Coverage,
    /// The provider's error for this account; its data is then ignored.
    pub error: Option<String>,
    /// Records the provider sent in a form that cannot be used, by their
    /// provider id when it could be read. They are skipped, and a known
    /// record among them still counts as reported, so it never vanishes.
    pub unusable: Vec<Option<String>>,
}

/// One transaction record as the provider reports it.
#[derive(Debug, Clone)]
pub struct ProviderTransaction {
    /// The provider's id, unique within the connection.
    pub provider_id: String,
    /// The day it is dated. `None` only for a pending record the provider
    /// gave no date for: it is dated the day it is first seen and keeps
    /// that day afterwards; a posted record without one is unusable.
    pub posted_on: Option<Date>,
    pub transacted_on: Option<Date>,
    /// Negative is money out of the account.
    pub amount: DecimalAmount,
    /// The amount exactly as the provider wrote it, in its own sign.
    pub amount_raw: String,
    pub description: String,
    pub payee: Option<String>,
    pub memo: Option<String>,
    pub pending: bool,
    /// The provider id of an earlier record this one replaces.
    pub replaces_provider_id: Option<String>,
}

/// Why a provider call failed. Messages are fixed; the texts some
/// variants carry come from the provider and pass through
/// [`sanitize_provider_message`] before they are stored or shown, and its
/// `Debug` output shows them sanitized too, so logging it is safe.
#[derive(thiserror::Error)]
#[non_exhaustive]
pub enum ProviderError {
    /// The provider refuses the credentials; a member must reconnect.
    #[error("the provider refused the credentials")]
    ReauthRequired,
    /// The provider asks coffer to slow down.
    #[error("the provider is rate limiting requests")]
    RateLimited,
    /// The provider could not be reached or failed.
    #[error("the provider is unavailable")]
    Unavailable(String),
    /// The provider refused the input or sent something unusable.
    #[error("the provider refused the request")]
    Invalid(String),
    /// The request was refused or failed on the way out.
    #[error("the provider could not be reached")]
    Egress(#[source] EgressError),
}

impl fmt::Debug for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReauthRequired => f.write_str("ReauthRequired"),
            Self::RateLimited => f.write_str("RateLimited"),
            Self::Unavailable(message) => f
                .debug_tuple("Unavailable")
                .field(&sanitize_provider_message(message))
                .finish(),
            Self::Invalid(message) => f
                .debug_tuple("Invalid")
                .field(&sanitize_provider_message(message))
                .finish(),
            Self::Egress(err) => f.debug_tuple("Egress").field(err).finish(),
        }
    }
}

impl From<EgressError> for ProviderError {
    fn from(err: EgressError) -> Self {
        Self::Egress(err)
    }
}

impl ProviderError {
    /// What to record about this error on the connection and the run:
    /// sanitized, never the credentials.
    #[must_use]
    pub fn record_text(&self) -> String {
        match self {
            Self::Unavailable(message) | Self::Invalid(message) => {
                let sanitized = sanitize_provider_message(message);
                if sanitized.is_empty() {
                    self.to_string()
                } else {
                    sanitized
                }
            }
            Self::ReauthRequired | Self::RateLimited | Self::Egress(_) => self.to_string(),
        }
    }
}

/// A boxed future a provider returns; boxed so that [`Provider`] can be a
/// trait object.
pub type ProviderFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A data provider (spec §6.2).
pub trait Provider: Send + Sync {
    /// Which provider this is.
    fn kind(&self) -> ProviderKind;

    /// What it supports and its limits.
    fn capabilities(&self) -> Capabilities;

    /// Turns setup input into the credentials to seal and store.
    fn connect(
        &self,
        input: ConnectInput,
    ) -> ProviderFuture<'_, Result<SecretString, ProviderError>>;

    /// Turns setup input into new credentials for an existing connection.
    /// A provider whose credentials cannot be replaced refuses.
    fn reconnect(
        &self,
        input: ConnectInput,
    ) -> ProviderFuture<'_, Result<SecretString, ProviderError>> {
        self.connect(input)
    }

    /// Fetches accounts, balances and transactions.
    fn fetch<'a>(
        &'a self,
        request: FetchRequest<'a>,
    ) -> ProviderFuture<'a, Result<FetchResult, ProviderError>>;

    /// Gives up the credentials at the provider, before a connection is
    /// removed.
    fn disconnect<'a>(
        &'a self,
        credentials: &'a SecretString,
    ) -> ProviderFuture<'a, Result<(), ProviderError>>;

    /// A short-lived token that opens the provider's own setup flow in
    /// `member`'s browser: for a new connection, or with `credentials` to
    /// log an existing one in again. A provider without such a flow refuses.
    fn link_token<'a>(
        &'a self,
        _member: MemberId,
        _credentials: Option<&'a SecretString>,
    ) -> ProviderFuture<'a, Result<SecretString, ProviderError>> {
        Box::pin(std::future::ready(Err(ProviderError::Invalid(
            NO_LINK_FLOW.into(),
        ))))
    }
}

/// The providers this server offers, by kind.
#[derive(Default, Clone)]
pub struct ProviderRegistry {
    providers: HashMap<ProviderKind, Arc<dyn Provider>>,
}

impl fmt::Debug for ProviderRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.providers.keys()).finish()
    }
}

impl ProviderRegistry {
    /// The providers `config` enables, each sending through `egress`. The
    /// one place providers are registered.
    ///
    /// # Errors
    ///
    /// [`ConfigError`] when a provider's configuration is unusable.
    pub fn from_config(config: &Config, egress: &Egress) -> Result<ProviderRegistry, ConfigError> {
        let mut registry = ProviderRegistry::default();
        registry.insert(Arc::new(SimpleFin::new(
            egress.clone(),
            config.simplefin_hosts.clone(),
        )));
        if let Some(plaid) = &config.plaid {
            let client = PlaidClient::new(egress.clone(), plaid.clone()).map_err(|_| {
                ConfigError::Invalid {
                    var: "COFFER_PLAID_ENV",
                    reason: "its host could not be used",
                }
            })?;
            registry.insert(Arc::new(Plaid::new(client)));
        }
        Ok(registry)
    }

    fn insert(&mut self, provider: Arc<dyn Provider>) {
        self.providers.insert(provider.kind(), provider);
    }

    /// The provider of `kind`, if this server offers it.
    #[must_use]
    pub fn get(&self, kind: ProviderKind) -> Option<Arc<dyn Provider>> {
        self.providers.get(&kind).cloned()
    }

    /// This registry with `provider` added, replacing any of its kind.
    #[cfg(feature = "testkit")]
    #[must_use]
    pub fn with(mut self, provider: Arc<dyn Provider>) -> ProviderRegistry {
        self.insert(provider);
        self
    }
}

/// `raw` made safe to store, log or show:
/// - control characters and Unicode format characters (zero-width
///   characters, bidi overrides, the byte order mark) are removed, so none
///   can hide a pattern below or spoof what a reader sees;
/// - in every whitespace-separated word holding an `@` (or `%40`), the
///   user information before it becomes `***`: everything after a scheme
///   separator (`://`, JSON-escaped `:\/\/`, percent-encoded `%3A%2F%2F`)
///   up to the last `@`, or, without a separator, a leading `user:pass`;
/// - the result is truncated to [`MAX_PROVIDER_MESSAGE_CHARS`] characters.
///
/// It hides more than the user information when a path or query holds an
/// `@`. Every provider text that reaches storage, logs or responses passes
/// through it.
#[must_use]
pub fn sanitize_provider_message(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|&c| !c.is_control() && !is_format_char(c))
        .collect();
    let mut out = String::with_capacity(cleaned.len());
    let mut rest = cleaned.as_str();
    while !rest.is_empty() {
        let word_start = rest
            .find(|c: char| !c.is_whitespace())
            .unwrap_or(rest.len());
        out.push_str(&rest[..word_start]);
        rest = &rest[word_start..];
        let word_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        out.push_str(&redact_word(&rest[..word_end]));
        rest = &rest[word_end..];
    }
    out.chars().take(MAX_PROVIDER_MESSAGE_CHARS).collect()
}

/// Scheme separators, lowercase: plain, JSON-escaped and percent-encoded.
const SCHEME_SEPARATORS: [&str; 3] = ["://", ":\\/\\/", "%3a%2f%2f"];

/// `word` with any user information before its last `@` replaced by `***`.
fn redact_word(word: &str) -> std::borrow::Cow<'_, str> {
    use std::borrow::Cow;
    // ASCII lowercasing keeps every byte offset.
    let lower = word.to_ascii_lowercase();
    let Some(at) = [lower.rfind('@'), lower.rfind("%40")]
        .into_iter()
        .flatten()
        .max()
    else {
        return Cow::Borrowed(word);
    };
    let separator_end = SCHEME_SEPARATORS
        .iter()
        .filter_map(|separator| lower[..at].find(separator).map(|i| i + separator.len()))
        .min();
    let start = if let Some(end) = separator_end {
        end
    } else {
        // Without a scheme, only `user:pass@` is a credential (the user may
        // be empty: `:pass@`); an address such as `ops@example.org` is
        // kept.
        let start = word
            .find(|c: char| c.is_alphanumeric() || c == ':' || c == '%')
            .unwrap_or(at)
            .min(at);
        let user = &lower[start..at];
        if !user.contains(':') && !user.contains("%3a") {
            return Cow::Borrowed(word);
        }
        start
    };
    if start >= at {
        return Cow::Borrowed(word);
    }
    Cow::Owned(format!("{}***{}", &word[..start], &word[at..]))
}

/// Whether `c` is a Unicode format character (general category Cf, as of
/// Unicode 15.1).
fn is_format_char(c: char) -> bool {
    matches!(
        c,
        '\u{ad}'
            | '\u{600}'..='\u{605}'
            | '\u{61c}'
            | '\u{6dd}'
            | '\u{70f}'
            | '\u{890}'..='\u{891}'
            | '\u{8e2}'
            | '\u{180e}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206f}'
            | '\u{feff}'
            | '\u{fff9}'..='\u{fffb}'
            | '\u{110bd}'
            | '\u{110cd}'
            | '\u{13430}'..='\u{1343f}'
            | '\u{1bca0}'..='\u{1bca3}'
            | '\u{1d173}'..='\u{1d17a}'
            | '\u{e0001}'
            | '\u{e0020}'..='\u{e007f}'
    )
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_PROVIDER_MESSAGE_CHARS, ProviderError, ProviderKind, sanitize_provider_message,
    };

    #[test]
    fn sanitize_removes_user_information_and_controls() {
        assert_eq!(
            sanitize_provider_message("failed for https://u:p@bridge.example/x\u{7}"),
            "failed for https://***@bridge.example/x"
        );
        assert_eq!(
            sanitize_provider_message("a https://token@h.example?x b http://u:p@h2.example:8443"),
            "a https://***@h.example?x b http://***@h2.example:8443"
        );
        // An `@` or a `/` in the user information itself is covered too.
        assert_eq!(
            sanitize_provider_message("x https://u:p@ss@h.example/y z"),
            "x https://***@h.example/y z"
        );
        assert_eq!(
            sanitize_provider_message("x https://u:pa/ss@h.example/y"),
            "x https://***@h.example/y"
        );
        // A control character cannot split the pattern to hide it.
        assert_eq!(
            sanitize_provider_message("https://u:\np@h.example"),
            "https://***@h.example"
        );
        // A scheme-less credential with an empty user is hidden too.
        assert_eq!(
            sanitize_provider_message("refused :pw@bridge.example/x"),
            "refused ***@bridge.example/x"
        );
        assert_eq!(
            sanitize_provider_message("refused %3Apw@bridge.example"),
            "refused ***@bridge.example"
        );
        // Text without user information is kept.
        for kept in [
            "plain message",
            "see https://bridge.example/status",
            "mail ops@example.org",
            "",
        ] {
            assert_eq!(sanitize_provider_message(kept), kept);
        }
    }

    /// Credentials echoed back in an altered form are hidden too.
    #[test]
    fn sanitize_redacts_altered_credential_forms() {
        for (raw, expected) in [
            // JSON-escaped slashes.
            (
                r#"{"url":"https:\/\/u:pw1@h.example\/x"}"#,
                r#"{"url":"https:\/\/***@h.example\/x"}"#,
            ),
            // Percent-encoded, the `@` encoded too.
            (
                "see https%3A%2F%2Fu%3Apw2%40h.example%2Fx",
                "see https%3A%2F%2F***%40h.example%2Fx",
            ),
            (
                "see https%3a%2f%2fu%3apw3@h.example",
                "see https%3a%2f%2f***@h.example",
            ),
            // No scheme at all.
            ("failed for u:pw4@h.example/x", "failed for ***@h.example/x"),
            ("(u:pw5@h.example)", "(***@h.example)"),
            // A zero-width character inside the separator.
            ("https:/\u{200b}/u:pw6@h.example", "https://***@h.example"),
            ("https\u{2060}://u:pw7@h.example", "https://***@h.example"),
        ] {
            let sanitized = sanitize_provider_message(raw);
            assert_eq!(sanitized, expected);
            assert!(!sanitized.contains("pw"), "no password survives");
        }
        // An address without a password is not a credential.
        assert_eq!(
            sanitize_provider_message("write to ops@example.org"),
            "write to ops@example.org"
        );
    }

    /// Unicode format characters (zero-width, bidi overrides, BOM) cannot
    /// spoof what a reader sees.
    #[test]
    fn sanitize_strips_format_characters() {
        assert_eq!(
            sanitize_provider_message("Bank\u{202e}gnp.exe\u{202c} \u{feff}ok\u{200d}\u{ad}"),
            "Bankgnp.exe ok"
        );
        for format in [
            '\u{600}',
            '\u{61c}',
            '\u{180e}',
            '\u{200b}',
            '\u{200f}',
            '\u{2066}',
            '\u{2069}',
            '\u{206f}',
            '\u{fff9}',
            '\u{110bd}',
            '\u{1d173}',
            '\u{e0001}',
            '\u{e007f}',
        ] {
            assert_eq!(
                sanitize_provider_message(&format!("a{format}b")),
                "ab",
                "{format:?}"
            );
        }
        assert_eq!(sanitize_provider_message("café ✓"), "café ✓");
    }

    #[test]
    fn sanitize_truncates_on_a_character_boundary() {
        let long = "é".repeat(MAX_PROVIDER_MESSAGE_CHARS + 20);
        let sanitized = sanitize_provider_message(&long);
        assert_eq!(sanitized.chars().count(), MAX_PROVIDER_MESSAGE_CHARS);
        // Redaction happens before truncation, so a cut never reveals a
        // password.
        let tail = format!("{} https://user:secret@h.example", "a".repeat(480));
        assert!(!sanitize_provider_message(&tail).contains("secret"));
    }

    #[test]
    fn provider_error_debug_is_sanitized() {
        let err = ProviderError::Invalid("bad https://user:secret@h.example/x".into());
        let debug = format!("{err:?}");
        assert!(!debug.contains("secret"));
        assert!(debug.contains("https://***@h.example/x"));
        assert_eq!(err.record_text(), "bad https://***@h.example/x");
        assert_eq!(
            ProviderError::Unavailable("\u{7}".into()).record_text(),
            "the provider is unavailable"
        );
    }

    #[test]
    fn provider_kind_names() {
        assert_eq!(
            "simplefin".parse::<ProviderKind>(),
            Ok(ProviderKind::SimpleFin)
        );
        assert_eq!(ProviderKind::SimpleFin.as_str(), "simplefin");
        assert_eq!("plaid".parse::<ProviderKind>(), Ok(ProviderKind::Plaid));
        assert_eq!(ProviderKind::Plaid.as_str(), "plaid");
        assert_eq!(
            serde_json::to_value(ProviderKind::Plaid).unwrap(),
            serde_json::Value::from("plaid")
        );
        assert_eq!(
            serde_json::to_value(ProviderKind::SimpleFin).unwrap(),
            serde_json::Value::from("simplefin")
        );
        assert!("SimpleFin".parse::<ProviderKind>().is_err());
    }
}
