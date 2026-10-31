//! Server configuration read from `COFFER_*` environment variables.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use jiff::SignedDuration;
use thiserror::Error;
use url::{Host, Url};

const DEFAULT_LISTEN: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8080);
const DEFAULT_INVITE_TTL_MINUTES: i64 = 30;
const DEFAULT_SESSION_IDLE_HOURS: i64 = 168;
const DEFAULT_SESSION_ABSOLUTE_DAYS: i64 = 30;

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
#[derive(Debug, Clone)]
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

        Ok(Self {
            listen,
            public_url,
            data_dir,
            secret_key_file,
            invite_ttl: SignedDuration::from_mins(invite_minutes),
            session_idle: SignedDuration::from_hours(idle_hours),
            session_absolute: SignedDuration::from_hours(absolute_days * 24),
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
}
