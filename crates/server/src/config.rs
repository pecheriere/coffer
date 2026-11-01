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
const MAX_PLAID_SECRET_BYTES: u16 = 4096;

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
    let limit = usize::from(MAX_PLAID_SECRET_BYTES);
    let mut text = Zeroizing::new(String::with_capacity(limit + 1));
    file.take(u64::from(MAX_PLAID_SECRET_BYTES) + 1)
        .read_to_string(&mut text)
        .map_err(|_| invalid("the file could not be read as UTF-8 text"))?;
    if text.len() > limit {
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
mod tests;
