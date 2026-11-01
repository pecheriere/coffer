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

/// Permissions are those of the file a symlink reaches, not the link's.
#[cfg(unix)]
#[test]
fn plaid_secret_file_symlink_follows_target_permissions() {
    let dir = tempfile::tempdir().unwrap();
    let link_to = |mode: u32, name: &str| {
        let target = secret_file(&dir, mode, b"SECRET-MARKER\n");
        let link = dir.path().join(name);
        std::os::unix::fs::symlink(&target, &link).unwrap();
        link.to_str().unwrap().to_owned()
    };

    let private = link_to(0o600, "link-private");
    let plaid = plaid_with_secret(&private, &[]).plaid.unwrap();
    assert!(
        plaid.secret.expose_secret() == "SECRET-MARKER",
        "a link to a private file is read"
    );

    let shared = link_to(0o644, "link-shared");
    let err = with_plaid(&[
        ("COFFER_PLAID_CLIENT_ID", "id"),
        ("COFFER_PLAID_SECRET_FILE", &shared),
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
        "a link to a file others can access is refused"
    );
    assert!(!err.to_string().contains("SECRET-MARKER"));
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
