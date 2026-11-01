//! Server configuration read from `COFFER_*` environment variables.

use std::fmt;
use std::io::Read as _;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use jiff::SignedDuration;
use jiff::tz::{TimeZone, TimeZoneDatabase};
use secrecy::SecretString;
use thiserror::Error;
use url::{Host, Url};
use zeroize::Zeroizing;

use crate::egress::HostAllowList;

const DEFAULT_LISTEN: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8080);
const DEFAULT_INVITE_TTL_MINUTES: i64 = 30;
const DEFAULT_SESSION_IDLE_HOURS: i64 = 168;
const DEFAULT_SESSION_ABSOLUTE_DAYS: i64 = 30;
const DEFAULT_SYNC_INTERVAL_HOURS: i64 = 6;
const DEFAULT_SIMPLEFIN_HOSTS: &str = "bridge.simplefin.org,beta-bridge.simplefin.org";
const DEFAULT_PLAID_COUNTRY_CODES: &str = "US";
/// The longest Plaid secret file accepted, in bytes.
const MAX_PLAID_SECRET_BYTES: usize = 4096;

/// A configuration problem. Messages name the offending variable and never
/// echo its value, which may be sensitive.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// A required variable is unset.
    #[error("{var} is required")]
    Missing {
        /// Variable name.
        var: &'static str,
    },
    /// A variable is set to a value that is not acceptable.
    #[error("{var} is invalid: {reason}")]
    Invalid {
        /// Variable name.
        var: &'static str,
        /// What is wrong, without quoting the value.
        reason: &'static str,
    },
}

/// Validated server configuration.
#[derive(Clone)]
pub struct Config {
    /// Address the HTTP server binds to.
    pub listen: SocketAddr,
    /// Externally visible base URL (used as the `WebAuthn` origin).
    pub public_url: Url,
    /// Directory holding the database and key material.
    pub data_dir: PathBuf,
    /// Path of the server secret key file.
    pub secret_key_file: PathBuf,
    /// How long an invite stays valid.
    pub invite_ttl: SignedDuration,
    /// Session lifetime without activity.
    pub session_idle: SignedDuration,
    /// Session lifetime regardless of activity.
    pub session_absolute: SignedDuration,
    /// Proxy for every outbound https request, from `HTTPS_PROXY` (else
    /// `https_proxy`). It may carry credentials, which `Debug` redacts.
    pub https_proxy: Option<Url>,
    /// The household's time zone, from `COFFER_TIMEZONE` (an IANA name,
    /// default `UTC`): provider timestamps become calendar days in it, and
    /// it decides which day is today.
    pub timezone: TimeZone,
    /// How often each connection syncs on schedule, from
    /// `COFFER_SYNC_INTERVAL_HOURS` (1 to 24, default 6). A provider's own
    /// minimum interval can lengthen it.
    pub sync_interval: SignedDuration,
    /// The hosts a `SimpleFin` setup token's claim URL and the access URL it
    /// yields may name, from `COFFER_SIMPLEFIN_HOSTS` (comma-separated DNS
    /// names, default `bridge.simplefin.org,beta-bridge.simplefin.org`).
    pub simplefin_hosts: HostAllowList,
    /// Plaid's credentials and environment, when Plaid is configured.
    pub plaid: Option<PlaidConfig>,
}

/// Plaid's API environment, from `COFFER_PLAID_ENV`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaidEnvironment {
    /// Test data, `sandbox.plaid.com`.
    Sandbox,
    /// Real institutions, `production.plaid.com`.
    Production,
}

impl PlaidEnvironment {
    /// The one host this environment's API lives on.
    #[must_use]
    pub fn host(self) -> &'static str {
        match self {
            Self::Sandbox => "sandbox.plaid.com",
            Self::Production => "production.plaid.com",
        }
    }
}

/// How coffer reaches Plaid. The client id and the secret are credentials:
/// `Debug` shows neither.
#[derive(Clone)]
pub struct PlaidConfig {
    /// The client id, from `COFFER_PLAID_CLIENT_ID`.
    pub client_id: String,
    /// The secret, from the file `COFFER_PLAID_SECRET_FILE` names.
    pub secret: SecretString,
    /// The environment, from `COFFER_PLAID_ENV`.
    pub environment: PlaidEnvironment,
    /// The countries Link offers institutions from, from
    /// `COFFER_PLAID_COUNTRY_CODES` (comma-separated ISO 3166-1 alpha-2,
    /// default `US`).
    pub country_codes: Vec<String>,
}

impl fmt::Debug for PlaidConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlaidConfig")
            .field("environment", &self.environment)
            .field("country_codes", &self.country_codes)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let https_proxy = self.https_proxy.as_ref().map(RedactedProxy);
        f.debug_struct("Config")
            .field("listen", &self.listen)
            .field("public_url", &self.public_url)
            .field("data_dir", &self.data_dir)
            .field("secret_key_file", &self.secret_key_file)
            .field("invite_ttl", &self.invite_ttl)
            .field("session_idle", &self.session_idle)
            .field("session_absolute", &self.session_absolute)
            .field("https_proxy", &https_proxy)
            .field("timezone", &self.timezone.iana_name())
            .field("sync_interval", &self.sync_interval)
            .field("simplefin_hosts", &self.simplefin_hosts)
            .field("plaid", &self.plaid)
            .finish()
    }
}

/// Shows a proxy URL as scheme, host and port, never its user information.
struct RedactedProxy<'a>(&'a Url);

impl fmt::Debug for RedactedProxy<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let url = self.0;
        let host = url.host_str().unwrap_or_default();
        let credentials = if url.username().is_empty() && url.password().is_none() {
            ""
        } else {
            "<credentials redacted>@"
        };
        match url.port() {
            Some(port) => write!(f, "\"{}://{credentials}{host}:{port}\"", url.scheme()),
            None => write!(f, "\"{}://{credentials}{host}\"", url.scheme()),
        }
    }
}

impl Config {
    /// Builds the configuration from a variable lookup (the process
    /// environment in production, a map in tests).
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when a required variable is missing or a value
    /// is malformed or out of range.
    pub fn from_env(vars: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let listen = Self::listen_from_env(&vars)?;
        let public_url = parse_public_url(&vars)?;

        let data_dir = match vars("COFFER_DATA_DIR") {
            Some(raw) if !raw.is_empty() => PathBuf::from(raw),
            Some(_) => {
                return Err(ConfigError::Invalid {
                    var: "COFFER_DATA_DIR",
                    reason: "must not be empty",
                });
            }
            None => {
                return Err(ConfigError::Missing {
                    var: "COFFER_DATA_DIR",
                });
            }
        };

        let secret_key_file = match vars("COFFER_SECRET_KEY_FILE") {
            Some(raw) if !raw.is_empty() => PathBuf::from(raw),
            Some(_) => {
                return Err(ConfigError::Invalid {
                    var: "COFFER_SECRET_KEY_FILE",
                    reason: "must not be empty",
                });
            }
            None => data_dir.join("secret.key"),
        };

        let invite_minutes = bounded(
            &vars,
            "COFFER_INVITE_TTL_MINUTES",
            DEFAULT_INVITE_TTL_MINUTES,
            1..=1440,
        )?;
        let idle_hours = bounded(
            &vars,
            "COFFER_SESSION_IDLE_HOURS",
            DEFAULT_SESSION_IDLE_HOURS,
            1..=8760,
        )?;
        let absolute_days = bounded(
            &vars,
            "COFFER_SESSION_ABSOLUTE_DAYS",
            DEFAULT_SESSION_ABSOLUTE_DAYS,
            1..=365,
        )?;
        let sync_hours = bounded(
            &vars,
            "COFFER_SYNC_INTERVAL_HOURS",
            DEFAULT_SYNC_INTERVAL_HOURS,
            1..=24,
        )?;

        Ok(Self {
            listen,
            public_url,
            data_dir,
            secret_key_file,
            invite_ttl: SignedDuration::from_mins(invite_minutes),
            session_idle: SignedDuration::from_hours(idle_hours),
            session_absolute: SignedDuration::from_hours(absolute_days * 24),
            https_proxy: parse_https_proxy(&vars)?,
            timezone: parse_timezone(&vars)?,
            sync_interval: SignedDuration::from_hours(sync_hours),
            simplefin_hosts: parse_simplefin_hosts(&vars)?,
            plaid: parse_plaid(&vars)?,
        })
    }

    /// Reads only `COFFER_LISTEN`, for commands such as `healthcheck` that
    /// need nothing else and must work without the data directory.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Invalid`] when the value is not a socket
    /// address.
    pub fn listen_from_env(
        vars: impl Fn(&str) -> Option<String>,
    ) -> Result<SocketAddr, ConfigError> {
        match vars("COFFER_LISTEN") {
            Some(raw) => raw.parse().map_err(|_| ConfigError::Invalid {
                var: "COFFER_LISTEN",
                reason: "expected host:port socket address",
            }),
            None => Ok(DEFAULT_LISTEN),
        }
    }

    /// Path of the `SQLite` database file inside the data directory.
    #[must_use]
    pub fn database_path(&self) -> PathBuf {
        self.data_dir.join("coffer.db")
    }
}

fn parse_public_url(vars: &impl Fn(&str) -> Option<String>) -> Result<Url, ConfigError> {
    const VAR: &str = "COFFER_PUBLIC_URL";
    let invalid = |reason| ConfigError::Invalid { var: VAR, reason };
    let raw = vars(VAR).ok_or(ConfigError::Missing { var: VAR })?;
    let url = Url::parse(&raw).map_err(|_| invalid("not a valid URL"))?;
    // The URL is the WebAuthn origin and the base of every setup and invite
    // link, so it must be exactly an origin: a WebAuthn relying party id is a domain,
    // never an IP address, and plain http is tolerated only for localhost
    // development.
    let Some(Host::Domain(domain)) = url.host() else {
        return Err(invalid("must name a domain, not an IP address"));
    };
    let local_dev = url.scheme() == "http" && domain == "localhost";
    if url.scheme() != "https" && !local_dev {
        return Err(invalid(
            "must use https (http is allowed only for localhost)",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(invalid("must not contain user information"));
    }
    if url.path() != "/" {
        return Err(invalid("must not have a path"));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(invalid("must not have a query or fragment"));
    }
    Ok(url)
}

/// Reads `HTTPS_PROXY`, else `https_proxy`; an empty value counts as unset.
/// No other proxy variable (`HTTP_PROXY`, `ALL_PROXY`, `NO_PROXY`) is read.
fn parse_https_proxy(vars: &impl Fn(&str) -> Option<String>) -> Result<Option<Url>, ConfigError> {
    let Some((var, raw)) = ["HTTPS_PROXY", "https_proxy"].into_iter().find_map(|var| {
        vars(var)
            .filter(|raw| !raw.is_empty())
            .map(|raw| (var, raw))
    }) else {
        return Ok(None);
    };
    let invalid = |reason| ConfigError::Invalid { var, reason };
    let url = Url::parse(&raw).map_err(|_| invalid("not a valid URL"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(invalid("must use http or https"));
    }
    if url.host().is_none() {
        return Err(invalid("must name a host"));
    }
    if url.path() != "/" {
        return Err(invalid("must not have a path"));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(invalid("must not have a query or fragment"));
    }
    Ok(Some(url))
}

/// Reads `COFFER_SIMPLEFIN_HOSTS`; unset means the public `SimpleFin`
/// bridges.
fn parse_simplefin_hosts(
    vars: &impl Fn(&str) -> Option<String>,
) -> Result<HostAllowList, ConfigError> {
    const VAR: &str = "COFFER_SIMPLEFIN_HOSTS";
    let raw = vars(VAR);
    HostAllowList::parse(raw.as_deref().unwrap_or(DEFAULT_SIMPLEFIN_HOSTS)).map_err(|_| {
        ConfigError::Invalid {
            var: VAR,
            reason: "expected comma-separated DNS host names",
        }
    })
}

/// Reads Plaid's variables: `COFFER_PLAID_CLIENT_ID` and
/// `COFFER_PLAID_SECRET_FILE` together, or neither for no Plaid at all, in
/// which case the other variables mean nothing.
fn parse_plaid(vars: &impl Fn(&str) -> Option<String>) -> Result<Option<PlaidConfig>, ConfigError> {
    const CLIENT_ID: &str = "COFFER_PLAID_CLIENT_ID";
    const SECRET_FILE: &str = "COFFER_PLAID_SECRET_FILE";
    let (client_id, secret_file) = match (vars(CLIENT_ID), vars(SECRET_FILE)) {
        (None, None) => return Ok(None),
        (Some(_), None) => return Err(ConfigError::Missing { var: SECRET_FILE }),
        (None, Some(_)) => return Err(ConfigError::Missing { var: CLIENT_ID }),
        (Some(client_id), Some(secret_file)) => (client_id, secret_file),
    };
    for (var, value) in [(CLIENT_ID, &client_id), (SECRET_FILE, &secret_file)] {
        if value.is_empty() {
            return Err(ConfigError::Invalid {
                var,
                reason: "must not be empty",
            });
        }
    }
    let environment = parse_plaid_environment(vars)?;
    let country_codes = parse_plaid_country_codes(vars)?;
    let secret = read_plaid_secret(SECRET_FILE, &secret_file)?;
    Ok(Some(PlaidConfig {
        client_id,
        secret,
        environment,
        country_codes,
    }))
}

fn parse_plaid_environment(
    vars: &impl Fn(&str) -> Option<String>,
) -> Result<PlaidEnvironment, ConfigError> {
    const VAR: &str = "COFFER_PLAID_ENV";
    let raw = vars(VAR).ok_or(ConfigError::Missing { var: VAR })?;
    match raw.as_str() {
        "sandbox" => Ok(PlaidEnvironment::Sandbox),
        "production" => Ok(PlaidEnvironment::Production),
        "development" => Err(ConfigError::Invalid {
            var: VAR,
            reason: "Plaid retired the development environment; use sandbox or production",
        }),
        _ => Err(ConfigError::Invalid {
            var: VAR,
            reason: "expected sandbox or production",
        }),
    }
}

/// Reads `COFFER_PLAID_COUNTRY_CODES`: only the shape of each code is
/// checked, and Plaid refuses a code it does not serve.
fn parse_plaid_country_codes(
    vars: &impl Fn(&str) -> Option<String>,
) -> Result<Vec<String>, ConfigError> {
    const VAR: &str = "COFFER_PLAID_COUNTRY_CODES";
    let raw = vars(VAR);
    raw.as_deref()
        .unwrap_or(DEFAULT_PLAID_COUNTRY_CODES)
        .split(',')
        .map(|code| {
            let code = code.trim();
            if code.len() == 2 && code.bytes().all(|b| b.is_ascii_uppercase()) {
                Ok(code.to_owned())
            } else {
                Err(ConfigError::Invalid {
                    var: VAR,
                    reason: "expected comma-separated ISO 3166-1 alpha-2 codes, such as US,CA",
                })
            }
        })
        .collect()
}

/// Reads the secret from the file at `path`, without its trailing newline.
/// Like the server key, the file must not be accessible to group or others;
/// that is checked on the open file, so it cannot change in between.
fn read_plaid_secret(var: &'static str, path: &str) -> Result<SecretString, ConfigError> {
    let invalid = |reason| ConfigError::Invalid { var, reason };
    let file = std::fs::File::open(path).map_err(|_| invalid("the file could not be read"))?;
    let metadata = file
        .metadata()
        .map_err(|_| invalid("the file could not be read"))?;
    if !metadata.is_file() {
        return Err(invalid("must name a regular file"));
    }
    #[cfg(unix)]
    if std::os::unix::fs::PermissionsExt::mode(&metadata.permissions()) & 0o077 != 0 {
        return Err(invalid(
            "the file must not be group or world accessible (use mode 0600)",
        ));
    }
    // Room for one byte over the limit, so that reading never reallocates
    // and leaves a copy of the secret behind.
    let mut text = Zeroizing::new(String::with_capacity(MAX_PLAID_SECRET_BYTES + 1));
    file.take(u64::try_from(MAX_PLAID_SECRET_BYTES + 1).unwrap_or(u64::MAX))
        .read_to_string(&mut text)
        .map_err(|_| invalid("the file could not be read as UTF-8 text"))?;
    if text.len() > MAX_PLAID_SECRET_BYTES {
        return Err(invalid("the file is too long to hold a Plaid secret"));
    }
    let secret = text.trim_end_matches(['\r', '\n']);
    if secret.is_empty() {
        return Err(invalid("the file is empty"));
    }
    Ok(SecretString::from(secret))
}

/// Reads `COFFER_TIMEZONE`, an IANA time zone name; unset means `UTC`. The
/// name is looked up in the system database when there is one, else in the
/// copy built into the binary, so a container without `/usr/share/zoneinfo`
/// still knows every zone.
fn parse_timezone(vars: &impl Fn(&str) -> Option<String>) -> Result<TimeZone, ConfigError> {
    const VAR: &str = "COFFER_TIMEZONE";
    let Some(name) = vars(VAR) else {
        return Ok(TimeZone::UTC);
    };
    lookup_timezone(&name).ok_or(ConfigError::Invalid {
        var: VAR,
        reason: "not a known IANA time zone name",
    })
}

/// The zone called `name` in the system database, else in the bundled one.
fn lookup_timezone(name: &str) -> Option<TimeZone> {
    if name.is_empty() {
        return None;
    }
    jiff::tz::db()
        .get(name)
        .or_else(|_| TimeZoneDatabase::bundled().get(name))
        .ok()
}

fn bounded(
    vars: &impl Fn(&str) -> Option<String>,
    var: &'static str,
    default: i64,
    range: std::ops::RangeInclusive<i64>,
) -> Result<i64, ConfigError> {
    let Some(raw) = vars(var) else {
        return Ok(default);
    };
    let value: i64 = raw.parse().map_err(|_| ConfigError::Invalid {
        var,
        reason: "expected an integer",
    })?;
    if range.contains(&value) {
        Ok(value)
    } else {
        Err(ConfigError::Invalid {
            var,
            reason: "out of the allowed range",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret as _;
    use std::collections::HashMap;

    fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |k| map.get(k).cloned()
    }

    fn with_public_url(url: &str) -> Result<Config, ConfigError> {
        Config::from_env(vars(&[
            ("COFFER_PUBLIC_URL", url),
            ("COFFER_DATA_DIR", "/data"),
        ]))
    }

    /// Asserts that `url` is refused as `COFFER_PUBLIC_URL`.
    fn assert_public_url_rejected(url: &str) {
        let err = with_public_url(url).unwrap_err();
        assert!(
            matches!(
                err,
                ConfigError::Invalid {
                    var: "COFFER_PUBLIC_URL",
                    ..
                }
            ),
            "{url}: {err:?}"
        );
    }

    #[test]
    fn public_url_accepts_https_domain_and_localhost() {
        for url in [
            "https://coffer.example",
            "https://coffer.example/",
            "https://coffer.example:8443",
            "http://localhost",
            "http://localhost:8080/",
        ] {
            let config = with_public_url(url).unwrap();
            assert_eq!(config.public_url.path(), "/", "{url}");
        }
    }

    #[test]
    fn public_url_rejects_plain_http_for_a_domain() {
        assert_public_url_rejected("http://coffer.example");
    }

    #[test]
    fn public_url_rejects_ipv4_over_http() {
        assert_public_url_rejected("http://127.0.0.1:8080");
    }

    #[test]
    fn public_url_rejects_ipv4_over_https() {
        assert_public_url_rejected("https://192.0.2.10");
    }

    #[test]
    fn public_url_rejects_ipv6_over_https() {
        assert_public_url_rejected("https://[2001:db8::1]");
    }

    #[test]
    fn public_url_rejects_ipv6_loopback_over_http() {
        assert_public_url_rejected("http://[::1]:8080");
    }

    #[test]
    fn public_url_rejects_a_path() {
        assert_public_url_rejected("https://coffer.example/coffer");
    }

    #[test]
    fn public_url_rejects_userinfo() {
        assert_public_url_rejected("https://user:pass@coffer.example");
    }

    #[test]
    fn public_url_rejects_a_query() {
        assert_public_url_rejected("https://coffer.example/?a=b");
    }

    #[test]
    fn public_url_rejects_a_fragment() {
        assert_public_url_rejected("https://coffer.example/#top");
    }

    #[test]
    fn public_url_rejects_other_schemes() {
        assert_public_url_rejected("ftp://coffer.example");
    }

    #[test]
    fn config_defaults() {
        let c = with_public_url("https://coffer.example").unwrap();
        assert_eq!(c.listen, DEFAULT_LISTEN);
        assert_eq!(c.listen.to_string(), "127.0.0.1:8080");
        assert_eq!(c.invite_ttl, SignedDuration::from_mins(30));
        assert_eq!(c.session_idle, SignedDuration::from_hours(168));
        assert_eq!(c.session_absolute, SignedDuration::from_hours(30 * 24));
        assert_eq!(c.secret_key_file, PathBuf::from("/data/secret.key"));
        assert_eq!(c.database_path(), PathBuf::from("/data/coffer.db"));
    }

    #[test]
    fn config_rejects_invite_ttl_zero() {
        let err = Config::from_env(vars(&[
            ("COFFER_PUBLIC_URL", "https://coffer.example"),
            ("COFFER_DATA_DIR", "/data"),
            ("COFFER_INVITE_TTL_MINUTES", "0"),
        ]))
        .unwrap_err();
        assert!(
            matches!(
                err,
                ConfigError::Invalid {
                    var: "COFFER_INVITE_TTL_MINUTES",
                    ..
                }
            ),
            "{err:?}"
        );
    }

    #[test]
    fn config_errors_never_echo_values() {
        let err = with_public_url("http://hunter2.example").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("COFFER_PUBLIC_URL"));
        assert!(!msg.contains("hunter2"));
    }

    #[test]
    fn proxy_config_validation() {
        let base = [
            ("COFFER_PUBLIC_URL", "https://coffer.example"),
            ("COFFER_DATA_DIR", "/data"),
        ];
        let with = |var: &str, value: &str| {
            let mut pairs = base.to_vec();
            pairs.push((var, value));
            Config::from_env(vars(&pairs))
        };

        assert!(
            with_public_url("https://coffer.example")
                .unwrap()
                .https_proxy
                .is_none()
        );

        let config = with("HTTPS_PROXY", "http://user:SECRETPW@proxy:3128").unwrap();
        let proxy = config.https_proxy.as_ref().unwrap();
        assert_eq!(proxy.host_str(), Some("proxy"));
        assert_eq!(proxy.port(), Some(3128));
        assert_eq!(proxy.password(), Some("SECRETPW"));
        let debug = format!("{config:?}");
        assert!(
            !debug.contains("SECRETPW"),
            "the Debug output must not show the proxy password"
        );
        assert!(
            !debug.contains("user:"),
            "the Debug output must not show the proxy user"
        );
        assert!(
            debug.contains("proxy:3128"),
            "the Debug output must show the proxy host"
        );

        // The lowercase spelling is read when the uppercase one is unset.
        let config = with("https_proxy", "https://proxy.example").unwrap();
        assert_eq!(
            config.https_proxy.as_ref().and_then(Url::host_str),
            Some("proxy.example")
        );
        // The uppercase spelling wins when both are set.
        let mut pairs = base.to_vec();
        pairs.push(("HTTPS_PROXY", "http://upper:3128"));
        pairs.push(("https_proxy", "http://lower:3128"));
        let config = Config::from_env(vars(&pairs)).unwrap();
        assert_eq!(
            config.https_proxy.as_ref().and_then(Url::host_str),
            Some("upper")
        );
        // An empty value counts as unset.
        assert!(with("HTTPS_PROXY", "").unwrap().https_proxy.is_none());

        for (var, value) in [
            ("HTTPS_PROXY", "ftp://proxy:21"),
            ("HTTPS_PROXY", "http://proxy/path"),
            ("HTTPS_PROXY", "http://proxy/?a=b"),
            ("HTTPS_PROXY", "http://proxy/#top"),
            ("HTTPS_PROXY", "socks5://proxy:1080"),
            ("HTTPS_PROXY", "not a url"),
            ("https_proxy", "http://proxy/path"),
        ] {
            let err = with(var, value).unwrap_err();
            assert!(
                matches!(err, ConfigError::Invalid { var: v, .. } if v == var),
                "{var}={value}: {err:?}"
            );
            assert!(
                !err.to_string().contains("proxy:"),
                "the error must not show the proxy URL"
            );
        }

        // The other proxy variables have no effect.
        for var in [
            "HTTP_PROXY",
            "http_proxy",
            "ALL_PROXY",
            "all_proxy",
            "NO_PROXY",
        ] {
            assert!(
                with(var, "http://proxy:3128")
                    .unwrap()
                    .https_proxy
                    .is_none(),
                "{var}"
            );
        }
    }

    fn with_var(var: &str, value: &str) -> Result<Config, ConfigError> {
        Config::from_env(vars(&[
            ("COFFER_PUBLIC_URL", "https://coffer.example"),
            ("COFFER_DATA_DIR", "/data"),
            (var, value),
        ]))
    }

    #[test]
    fn timezone_defaults_to_utc_and_reads_iana_names() {
        let config = with_public_url("https://coffer.example").unwrap();
        assert_eq!(config.timezone, TimeZone::UTC);
        let config = with_var("COFFER_TIMEZONE", "Pacific/Auckland").unwrap();
        assert_eq!(config.timezone.iana_name(), Some("Pacific/Auckland"));
        assert!(format!("{config:?}").contains("Pacific/Auckland"));
        for bad in ["", "Not/AZone", "../../etc/passwd", "UTC+25"] {
            let err = with_var("COFFER_TIMEZONE", bad).unwrap_err();
            assert!(
                matches!(
                    err,
                    ConfigError::Invalid {
                        var: "COFFER_TIMEZONE",
                        ..
                    }
                ),
                "{bad:?}: {err:?}"
            );
        }
    }

    /// The zone data is built into the binary, so the container image
    /// needs no `/usr/share/zoneinfo`.
    #[test]
    fn bundled_timezone_database_is_built_in() {
        let bundled = TimeZoneDatabase::bundled();
        assert!(!bundled.is_definitively_empty());
        for name in ["Pacific/Auckland", "America/Los_Angeles", "Europe/Paris"] {
            assert!(bundled.get(name).is_ok(), "{name}");
        }
    }

    #[test]
    fn sync_interval_bounds() {
        let config = with_public_url("https://coffer.example").unwrap();
        assert_eq!(config.sync_interval, SignedDuration::from_hours(6));
        let config = with_var("COFFER_SYNC_INTERVAL_HOURS", "24").unwrap();
        assert_eq!(config.sync_interval, SignedDuration::from_hours(24));
        for bad in ["0", "25", "six"] {
            let err = with_var("COFFER_SYNC_INTERVAL_HOURS", bad).unwrap_err();
            assert!(
                matches!(
                    err,
                    ConfigError::Invalid {
                        var: "COFFER_SYNC_INTERVAL_HOURS",
                        ..
                    }
                ),
                "{bad}: {err:?}"
            );
        }
    }

    #[test]
    fn config_requires_data_dir() {
        let err =
            Config::from_env(vars(&[("COFFER_PUBLIC_URL", "https://coffer.example")])).unwrap_err();
        assert!(
            matches!(
                err,
                ConfigError::Missing {
                    var: "COFFER_DATA_DIR"
                }
            ),
            "{err:?}"
        );
    }

    /// A variable lookup for a `Config` with Plaid's variables as given.
    fn with_plaid(plaid: &[(&str, &str)]) -> Result<Config, ConfigError> {
        let mut pairs = vec![
            ("COFFER_PUBLIC_URL", "https://coffer.example"),
            ("COFFER_DATA_DIR", "/data"),
        ];
        pairs.extend_from_slice(plaid);
        Config::from_env(vars(&pairs))
    }

    /// Writes `contents` to a file of `mode` in `dir` and returns its path.
    #[cfg(unix)]
    fn secret_file(dir: &tempfile::TempDir, mode: u32, contents: &[u8]) -> String {
        use std::os::unix::fs::PermissionsExt as _;
        let path = dir.path().join(format!("plaid-secret-{mode:o}"));
        std::fs::write(&path, contents).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        path.to_str().unwrap().to_owned()
    }

    /// A complete, valid set of Plaid variables but for what `extra` adds.
    #[cfg(unix)]
    fn plaid_with_secret(path: &str, extra: &[(&'static str, &'static str)]) -> Config {
        let mut pairs = vec![
            ("COFFER_PLAID_CLIENT_ID", "client-id-MARKER"),
            ("COFFER_PLAID_SECRET_FILE", path),
            ("COFFER_PLAID_ENV", "sandbox"),
        ];
        pairs.extend_from_slice(extra);
        with_plaid(&pairs).unwrap()
    }

    #[test]
    fn plaid_is_optional() {
        assert!(with_plaid(&[]).unwrap().plaid.is_none());
        // The other variables mean nothing without the credentials.
        let config = with_plaid(&[("COFFER_PLAID_ENV", "sandbox")]).unwrap();
        assert!(config.plaid.is_none());
    }

    #[test]
    fn plaid_requires_both_id_and_secret_file() {
        let err = with_plaid(&[("COFFER_PLAID_CLIENT_ID", "id")]).unwrap_err();
        assert!(
            matches!(
                err,
                ConfigError::Missing {
                    var: "COFFER_PLAID_SECRET_FILE"
                }
            ),
            "the error names the variable"
        );
        let err = with_plaid(&[("COFFER_PLAID_SECRET_FILE", "/run/secret")]).unwrap_err();
        assert!(
            matches!(
                err,
                ConfigError::Missing {
                    var: "COFFER_PLAID_CLIENT_ID"
                }
            ),
            "the error names the variable"
        );
        for (var, id, file) in [
            ("COFFER_PLAID_CLIENT_ID", "", "/run/secret"),
            ("COFFER_PLAID_SECRET_FILE", "id", ""),
        ] {
            let err = with_plaid(&[
                ("COFFER_PLAID_CLIENT_ID", id),
                ("COFFER_PLAID_SECRET_FILE", file),
                ("COFFER_PLAID_ENV", "sandbox"),
            ])
            .unwrap_err();
            assert!(
                matches!(err, ConfigError::Invalid { var: v, .. } if v == var),
                "the error names the variable"
            );
        }
    }

    #[test]
    fn plaid_env_rejects_development_with_explanation() {
        let plaid = |env: Option<&'static str>| {
            let mut pairs = vec![
                ("COFFER_PLAID_CLIENT_ID", "id"),
                ("COFFER_PLAID_SECRET_FILE", "/nonexistent/secret"),
            ];
            pairs.extend(env.map(|env| ("COFFER_PLAID_ENV", env)));
            with_plaid(&pairs).unwrap_err()
        };
        let err = plaid(Some("development"));
        assert!(
            matches!(
                err,
                ConfigError::Invalid {
                    var: "COFFER_PLAID_ENV",
                    reason: "Plaid retired the development environment; use sandbox or production",
                }
            ),
            "the error names the variable"
        );
        let err = plaid(None);
        assert!(
            matches!(
                err,
                ConfigError::Missing {
                    var: "COFFER_PLAID_ENV"
                }
            ),
            "the error names the variable"
        );
        for bad in ["", "Sandbox", "hunter2"] {
            let err = plaid(Some(bad));
            assert!(
                matches!(
                    err,
                    ConfigError::Invalid {
                        var: "COFFER_PLAID_ENV",
                        ..
                    }
                ),
                "the error names the variable"
            );
            assert!(!err.to_string().contains("hunter2"));
        }
    }

    #[test]
    fn plaid_environments_name_one_host_each() {
        assert_eq!(PlaidEnvironment::Sandbox.host(), "sandbox.plaid.com");
        assert_eq!(PlaidEnvironment::Production.host(), "production.plaid.com");
    }

    #[cfg(unix)]
    #[test]
    fn plaid_secret_file_permissions_checked() {
        let dir = tempfile::tempdir().unwrap();
        let contents = b"SECRET-MARKER\n";
        for mode in [0o640, 0o604, 0o660, 0o644, 0o610] {
            let path = secret_file(&dir, mode, contents);
            let err = with_plaid(&[
                ("COFFER_PLAID_CLIENT_ID", "id"),
                ("COFFER_PLAID_SECRET_FILE", &path),
                ("COFFER_PLAID_ENV", "production"),
            ])
            .unwrap_err();
            assert!(
                matches!(
                    err,
                    ConfigError::Invalid {
                        var: "COFFER_PLAID_SECRET_FILE",
                        ..
                    }
                ),
                "a file others can access is refused"
            );
            assert!(!err.to_string().contains("SECRET-MARKER"));
        }
        for mode in [0o600, 0o400] {
            let path = secret_file(&dir, mode, contents);
            let config = plaid_with_secret(&path, &[]);
            let plaid = config.plaid.unwrap();
            assert!(
                plaid.secret.expose_secret() == "SECRET-MARKER",
                "the secret is read without its newline"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn plaid_secret_file_contents_checked() {
        let dir = tempfile::tempdir().unwrap();
        let read = |contents: &[u8]| {
            let path = secret_file(&dir, 0o600, contents);
            with_plaid(&[
                ("COFFER_PLAID_CLIENT_ID", "id"),
                ("COFFER_PLAID_SECRET_FILE", &path),
                ("COFFER_PLAID_ENV", "sandbox"),
            ])
            .map(|config| config.plaid.unwrap().secret.expose_secret().to_owned())
        };
        for contents in [&b"abc"[..], b"abc\n", b"abc\r\n"] {
            assert!(
                read(contents).unwrap() == "abc",
                "a trailing newline is trimmed"
            );
        }
        let too_long = vec![b'a'; 4097];
        for bad in [&b""[..], b"\n", b"\xff\xfe", &too_long] {
            let err = read(bad).unwrap_err();
            assert!(
                matches!(
                    err,
                    ConfigError::Invalid {
                        var: "COFFER_PLAID_SECRET_FILE",
                        ..
                    }
                ),
                "the error names the variable"
            );
        }
        // Neither a missing file nor a directory, and no path in the error.
        for path in [
            dir.path().join("absent").to_str().unwrap().to_owned(),
            dir.path().to_str().unwrap().to_owned(),
        ] {
            let err = with_plaid(&[
                ("COFFER_PLAID_CLIENT_ID", "id"),
                ("COFFER_PLAID_SECRET_FILE", &path),
                ("COFFER_PLAID_ENV", "sandbox"),
            ])
            .unwrap_err();
            assert!(
                !err.to_string().contains(&path),
                "the error must not show the path"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn plaid_country_codes_default_and_validation() {
        let dir = tempfile::tempdir().unwrap();
        let path = secret_file(&dir, 0o600, b"s");
        let codes = |extra: &[(&'static str, &'static str)]| {
            plaid_with_secret(&path, extra).plaid.unwrap().country_codes
        };
        assert_eq!(codes(&[]), ["US"]);
        assert_eq!(
            codes(&[("COFFER_PLAID_COUNTRY_CODES", "US, CA,GB")]),
            ["US", "CA", "GB"]
        );
        for bad in ["", "us", "USA", "U", "US,,CA", "U1", "US;CA"] {
            let err = with_plaid(&[
                ("COFFER_PLAID_CLIENT_ID", "id"),
                ("COFFER_PLAID_SECRET_FILE", &path),
                ("COFFER_PLAID_ENV", "sandbox"),
                ("COFFER_PLAID_COUNTRY_CODES", bad),
            ])
            .unwrap_err();
            assert!(
                matches!(
                    err,
                    ConfigError::Invalid {
                        var: "COFFER_PLAID_COUNTRY_CODES",
                        ..
                    }
                ),
                "a malformed code list is refused"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn plaid_config_debug_redacted() {
        let dir = tempfile::tempdir().unwrap();
        let path = secret_file(&dir, 0o600, b"SECRET-MARKER\n");
        let config = plaid_with_secret(&path, &[]);
        let plaid = config.plaid.clone().unwrap();
        assert!(
            plaid.client_id == "client-id-MARKER",
            "the client id is read"
        );
        for debug in [format!("{config:?}"), format!("{plaid:?}")] {
            assert!(
                !debug.contains("SECRET-MARKER"),
                "the Debug output must not show the secret"
            );
            assert!(
                !debug.contains("client-id-MARKER"),
                "the Debug output must not show the client id"
            );
            assert!(
                debug.contains("Sandbox"),
                "the Debug output must show the environment"
            );
        }
    }

    #[test]
    fn simplefin_hosts_default_to_the_public_bridges() {
        let config = with_public_url("https://coffer.example").unwrap();
        assert_eq!(
            config.simplefin_hosts,
            HostAllowList::parse("bridge.simplefin.org,beta-bridge.simplefin.org").unwrap()
        );
        let config = with_var("COFFER_SIMPLEFIN_HOSTS", "Bridge.Example.").unwrap();
        assert_eq!(
            config.simplefin_hosts,
            HostAllowList::parse("bridge.example").unwrap()
        );
        for bad in [
            "",
            "10.0.0.1",
            "*.example.org",
            "https://bridge.example",
            "a,,b",
        ] {
            let err = with_var("COFFER_SIMPLEFIN_HOSTS", bad).unwrap_err();
            assert!(
                matches!(
                    err,
                    ConfigError::Invalid {
                        var: "COFFER_SIMPLEFIN_HOSTS",
                        ..
                    }
                ),
                "{bad}: {err:?}"
            );
        }
    }
}
