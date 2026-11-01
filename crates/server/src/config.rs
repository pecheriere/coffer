//! Server configuration read from `COFFER_*` environment variables.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use jiff::SignedDuration;
use jiff::tz::{TimeZone, TimeZoneDatabase};
use thiserror::Error;
use url::{Host, Url};

use crate::egress::HostAllowList;

const DEFAULT_LISTEN: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8080);
const DEFAULT_INVITE_TTL_MINUTES: i64 = 30;
const DEFAULT_SESSION_IDLE_HOURS: i64 = 168;
const DEFAULT_SESSION_ABSOLUTE_DAYS: i64 = 30;
const DEFAULT_SYNC_INTERVAL_HOURS: i64 = 6;
const DEFAULT_SIMPLEFIN_HOSTS: &str = "bridge.simplefin.org,beta-bridge.simplefin.org";

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
        assert!(!debug.contains("SECRETPW"), "{debug}");
        assert!(!debug.contains("user:"), "{debug}");
        assert!(debug.contains("proxy:3128"), "{debug}");

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
            assert!(!err.to_string().contains("proxy:"), "{err}");
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
