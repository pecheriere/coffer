//! `COFFER_EGRESS_RELAY`: its configuration, and the outbound client against a
//! loopback TLS server standing in for the relay. The server uses a certificate
//! made at run time by a throwaway CA, which the client trusts on top of the
//! platform's roots; everything else about the client is the production path.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use std::collections::HashMap;
use std::io::{Read as _, Write as _};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use coffer_server::config::{Config, ConfigError};
use coffer_server::egress::{Egress, EgressError, HostAllowList, OutboundRequest};
use openssl::asn1::Asn1Time;
use openssl::bn::BigNum;
use openssl::ec::{EcGroup, EcKey};
use openssl::hash::MessageDigest;
use openssl::nid::Nid;
use openssl::pkey::{PKey, Private};
use openssl::x509::extension::{
    BasicConstraints, ExtendedKeyUsage, KeyUsage, SubjectAlternativeName,
};
use openssl::x509::{X509, X509Builder, X509NameBuilder, X509NameRef};
use reqwest::{Method, StatusCode};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::sync::mpsc;
use url::Url;

const HOST: &str = "bridge.example";
const OK: &[u8] = b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok";
const REDIRECT: &[u8] = b"HTTP/1.1 302 Found\r\nlocation: https://bridge.example/next\r\n\
    content-length: 0\r\nconnection: close\r\n\r\n";

fn get_request(url: &str) -> OutboundRequest {
    OutboundRequest {
        method: Method::GET,
        url: Url::parse(url).unwrap(),
        basic_auth: None,
        headers: Vec::new(),
        body: None,
    }
}

fn allowed() -> HostAllowList {
    HostAllowList::parse(HOST).unwrap()
}

/// A throwaway certificate authority.
struct TestCa {
    cert: X509,
    key: PKey<Private>,
}

fn ec_key() -> PKey<Private> {
    let group = EcGroup::from_curve_name(Nid::X9_62_PRIME256V1).unwrap();
    PKey::from_ec_key(EcKey::generate(&group).unwrap()).unwrap()
}

/// A certificate builder valid for one day, for `key`, named `common_name`,
/// issued by `issuer` or else by itself.
fn certificate(
    common_name: &str,
    serial: u32,
    key: &PKey<Private>,
    issuer: Option<&X509NameRef>,
) -> X509Builder {
    let mut name = X509NameBuilder::new().unwrap();
    name.append_entry_by_nid(Nid::COMMONNAME, common_name)
        .unwrap();
    let name = name.build();
    let mut builder = X509Builder::new().unwrap();
    builder.set_version(2).unwrap();
    let serial = BigNum::from_u32(serial).unwrap().to_asn1_integer().unwrap();
    builder.set_serial_number(&serial).unwrap();
    builder.set_subject_name(&name).unwrap();
    builder.set_issuer_name(issuer.unwrap_or(&name)).unwrap();
    builder.set_pubkey(key).unwrap();
    builder
        .set_not_before(&Asn1Time::days_from_now(0).unwrap())
        .unwrap();
    builder
        .set_not_after(&Asn1Time::days_from_now(1).unwrap())
        .unwrap();
    builder
}

impl TestCa {
    fn new() -> TestCa {
        let key = ec_key();
        let mut builder = certificate("coffer test CA", 1, &key, None);
        builder
            .append_extension(BasicConstraints::new().critical().ca().build().unwrap())
            .unwrap();
        builder
            .append_extension(KeyUsage::new().critical().key_cert_sign().build().unwrap())
            .unwrap();
        builder.sign(&key, MessageDigest::sha256()).unwrap();
        TestCa {
            cert: builder.build(),
            key,
        }
    }

    fn root(&self) -> CertificateDer<'static> {
        CertificateDer::from(self.cert.to_der().unwrap())
    }

    /// A server certificate and key for the DNS name `name`.
    fn server_identity(&self, name: &str) -> ServerIdentity {
        let key = ec_key();
        let mut builder = certificate(name, 2, &key, Some(self.cert.subject_name()));
        let san = SubjectAlternativeName::new()
            .dns(name)
            .build(&builder.x509v3_context(Some(&self.cert), None))
            .unwrap();
        builder.append_extension(san).unwrap();
        builder
            .append_extension(ExtendedKeyUsage::new().server_auth().build().unwrap())
            .unwrap();
        builder
            .append_extension(
                KeyUsage::new()
                    .critical()
                    .digital_signature()
                    .build()
                    .unwrap(),
            )
            .unwrap();
        builder.sign(&self.key, MessageDigest::sha256()).unwrap();
        ServerIdentity {
            cert: CertificateDer::from(builder.build().to_der().unwrap()),
            key: PrivateKeyDer::try_from(key.private_key_to_pkcs8().unwrap()).unwrap(),
        }
    }
}

struct ServerIdentity {
    cert: CertificateDer<'static>,
    key: PrivateKeyDer<'static>,
}

/// What the loopback relay saw on one connection.
#[derive(Debug)]
struct Seen {
    /// The name in the client's TLS hello.
    sni: Option<String>,
    /// The request head, when the handshake completed.
    head: Option<String>,
}

/// The stand-in for the relay: terminates TLS on a loopback port, records each
/// connection and answers each request with `response`.
struct LoopbackRelay {
    addr: SocketAddr,
    accepted: Arc<AtomicUsize>,
    seen: mpsc::UnboundedReceiver<Seen>,
}

fn start_relay(identity: ServerIdentity, response: &'static [u8]) -> LoopbackRelay {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![identity.cert], identity.key)
        .unwrap();
    let config = Arc::new(config);
    let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&accepted);
    let (tx, seen) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        while let Ok((mut tcp, _)) = listener.accept() {
            counter.fetch_add(1, Ordering::SeqCst);
            let seen = serve_connection(&mut tcp, Arc::clone(&config), response);
            if tx.send(seen).is_err() {
                return;
            }
        }
    });
    LoopbackRelay {
        addr,
        accepted,
        seen,
    }
}

fn serve_connection(
    tcp: &mut TcpStream,
    config: Arc<rustls::ServerConfig>,
    response: &[u8],
) -> Seen {
    tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut tls = rustls::ServerConnection::new(config).unwrap();
    let mut head = Vec::new();
    let mut outcome = Ok(());
    while tls.is_handshaking() && outcome.is_ok() {
        outcome = tls.complete_io(tcp).map(|_| ());
    }
    let sni = tls.server_name().map(str::to_owned);
    if outcome.is_err() {
        // The client refused the certificate, or the handshake failed.
        return Seen { sni, head: None };
    }
    let mut stream = rustls::Stream::new(&mut tls, tcp);
    let mut chunk = [0; 1024];
    while !head.windows(4).any(|end| end == b"\r\n\r\n") {
        match stream.read(&mut chunk) {
            Ok(read) if read > 0 => head.extend_from_slice(&chunk[..read]),
            _ => return Seen { sni, head: None },
        }
    }
    stream.write_all(response).unwrap();
    stream.flush().unwrap();
    tls.send_close_notify();
    let _ = tls.complete_io(tcp);
    Seen {
        sni,
        head: Some(String::from_utf8(head).unwrap()),
    }
}

fn assert_tls_failure(err: &EgressError) {
    let EgressError::Transport { cause } = err else {
        panic!("expected a transport failure");
    };
    assert!(
        cause.to_string().contains("TLS handshake"),
        "expected the TLS handshake to fail"
    );
}

/// The whole point: the URL names the real host, which does not resolve (the
/// `.example` domain is reserved), and the connection reaches the relay with
/// that name in SNI and `Host`, and verified against it.
#[tokio::test]
async fn relay_carries_an_allowed_host_under_its_real_name() {
    let ca = TestCa::new();
    let mut relay = start_relay(ca.server_identity(HOST), OK);
    let egress = Egress::relay_for_tests(relay.addr, ca.root());

    let response = egress
        .send(get_request("https://bridge.example/claim"), &allowed())
        .await
        .unwrap();
    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.body, b"ok");

    let seen = relay.seen.recv().await.unwrap();
    assert_eq!(seen.sni.as_deref(), Some(HOST));
    let head = seen.head.unwrap().to_ascii_lowercase();
    assert!(
        head.starts_with("get /claim http/1.1\r\n"),
        "the request line must be the URL's path"
    );
    assert!(
        head.contains("\r\nhost: bridge.example\r\n"),
        "the Host header must be the real host"
    );
}

/// A certificate valid for some other name is refused, although the same CA
/// issued it: the name verified is the URL's, whatever the relay presents.
#[tokio::test]
async fn relay_presenting_a_certificate_for_another_name_is_refused() {
    let ca = TestCa::new();
    let mut relay = start_relay(ca.server_identity("other.example"), OK);
    let egress = Egress::relay_for_tests(relay.addr, ca.root());

    let err = egress
        .send(get_request("https://bridge.example/claim"), &allowed())
        .await
        .unwrap_err();
    assert_tls_failure(&err);

    let seen = relay.seen.recv().await.unwrap();
    assert_eq!(seen.sni.as_deref(), Some(HOST));
    assert!(seen.head.is_none(), "a request crossed a refused handshake");
}

/// A certificate for the right name from a CA nobody trusts is refused: the
/// relay is trusted to forward, not to terminate TLS.
#[tokio::test]
async fn relay_presenting_an_untrusted_certificate_is_refused() {
    let trusted = TestCa::new();
    let intruder = TestCa::new();
    let mut relay = start_relay(intruder.server_identity(HOST), OK);
    let egress = Egress::relay_for_tests(relay.addr, trusted.root());

    let err = egress
        .send(get_request("https://bridge.example/claim"), &allowed())
        .await
        .unwrap_err();
    assert_tls_failure(&err);

    let seen = relay.seen.recv().await.unwrap();
    assert!(seen.head.is_none(), "a request crossed a refused handshake");
}

/// Everything `check_url` refuses stays refused and opens no connection. The
/// allowed request afterwards shows the relay was there to be reached.
#[tokio::test]
async fn relay_reaches_nothing_for_a_refused_destination() {
    let ca = TestCa::new();
    let mut relay = start_relay(ca.server_identity(HOST), OK);
    let egress = Egress::relay_for_tests(relay.addr, ca.root());

    for url in [
        "https://unlisted.example/x",
        "https://bridge.example.unlisted.example/x",
        "https://127.0.0.1/x",
        "https://[::1]/x",
        "https://bridge.example:8443/x",
        "https://user:password@bridge.example/x",
        "https://bridge.example/x#fragment",
    ] {
        let err = egress.send(get_request(url), &allowed()).await.unwrap_err();
        assert!(
            matches!(err, EgressError::HostNotAllowed),
            "a refused destination must be refused as not allowed"
        );
    }
    let err = egress
        .send(get_request("http://bridge.example/x"), &allowed())
        .await
        .unwrap_err();
    assert!(
        matches!(err, EgressError::NotHttps),
        "plain http must be refused"
    );

    egress
        .send(get_request("https://bridge.example/x"), &allowed())
        .await
        .unwrap();
    relay.seen.recv().await.unwrap();
    assert_eq!(
        relay.accepted.load(Ordering::SeqCst),
        1,
        "a refused destination reached the relay"
    );
}

#[tokio::test]
async fn relay_does_not_follow_redirects() {
    let ca = TestCa::new();
    let mut relay = start_relay(ca.server_identity(HOST), REDIRECT);
    let egress = Egress::relay_for_tests(relay.addr, ca.root());

    let response = egress
        .send(get_request("https://bridge.example/start"), &allowed())
        .await
        .unwrap();
    assert_eq!(response.status, StatusCode::FOUND);
    relay.seen.recv().await.unwrap();
    assert!(
        relay.seen.try_recv().is_err(),
        "the redirect was followed through the relay"
    );
    assert_eq!(relay.accepted.load(Ordering::SeqCst), 1);
}

fn config_with(extra: &[(&str, &str)]) -> Result<Config, ConfigError> {
    let map: HashMap<String, String> = [
        ("COFFER_PUBLIC_URL", "https://coffer.example"),
        ("COFFER_DATA_DIR", "/data"),
    ]
    .iter()
    .chain(extra)
    .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
    .collect();
    Config::from_env(move |key| map.get(key).cloned())
}

/// The production client, built from configuration, connects to the relay
/// without resolving: nothing listens on the relay's port 443 here, so the
/// request fails connecting, or at TLS if something does. The name does not
/// resolve at all, so the same request without the relay is a resolution
/// failure.
#[tokio::test]
async fn configured_relay_replaces_name_resolution() {
    for relay in ["127.0.0.1", "::1"] {
        let config = config_with(&[("COFFER_EGRESS_RELAY", relay)]).unwrap();
        let err = Egress::from_config(&config)
            .unwrap()
            .send(get_request("https://bridge.example/x"), &allowed())
            .await
            .unwrap_err();
        assert!(
            matches!(err, EgressError::Transport { .. }),
            "the relay must be connected to, with no resolution"
        );
    }

    let config = config_with(&[]).unwrap();
    let err = Egress::from_config(&config)
        .unwrap()
        .send(get_request("https://bridge.example/x"), &allowed())
        .await
        .unwrap_err();
    assert!(
        matches!(err, EgressError::Resolution),
        "without the relay the name must be resolved"
    );
}

#[test]
fn relay_setting_accepts_one_ip_address() {
    for (raw, expected) in [
        ("10.0.0.7", IpAddr::from([10, 0, 0, 7])),
        ("127.0.0.1", IpAddr::from(Ipv4Addr::LOCALHOST)),
        ("198.51.100.9", IpAddr::from([198, 51, 100, 9])),
        ("fd00::7", IpAddr::from([0xfd00, 0, 0, 0, 0, 0, 0, 7_u16])),
        ("::1", IpAddr::from(std::net::Ipv6Addr::LOCALHOST)),
        // An IPv4-mapped address means the IPv4 address it carries.
        ("::ffff:10.0.0.7", IpAddr::from([10, 0, 0, 7])),
    ] {
        let config = config_with(&[("COFFER_EGRESS_RELAY", raw)]).unwrap();
        assert_eq!(
            config.egress_relay,
            Some(expected),
            "a valid address must be read as itself"
        );
    }
}

#[test]
fn relay_setting_refuses_anything_but_one_usable_address() {
    for raw in [
        "",
        " ",
        "relay.internal.example",
        "localhost",
        "10.0.0.7:443",
        "[fd00::7]:443",
        "[fd00::7]",
        "fe80::1%eth0",
        "https://10.0.0.7",
        "10.0.0.7/32",
        "10.0.0.7,10.0.0.8",
        " 10.0.0.7",
        "10.0.0",
        "not an address",
        // Addresses nothing can be reached at.
        "0.0.0.0",
        "::",
        "::ffff:0.0.0.0",
        "224.0.0.1",
        "ff02::1",
        "255.255.255.255",
    ] {
        let err = config_with(&[("COFFER_EGRESS_RELAY", raw)]).unwrap_err();
        assert!(
            matches!(
                err,
                ConfigError::Invalid {
                    var: "COFFER_EGRESS_RELAY",
                    ..
                }
            ),
            "the error must blame the relay variable"
        );
        assert!(
            raw.trim().is_empty() || !err.to_string().contains(raw),
            "the error must not echo the value"
        );
    }
}

#[test]
fn relay_setting_conflicts_with_a_proxy() {
    for var in ["HTTPS_PROXY", "https_proxy"] {
        let err = config_with(&[
            ("COFFER_EGRESS_RELAY", "10.0.0.7"),
            (var, "http://proxy.example:3128"),
        ])
        .unwrap_err();
        assert!(
            matches!(
                err,
                ConfigError::Invalid {
                    var: "COFFER_EGRESS_RELAY",
                    ..
                }
            ),
            "the error must blame the relay variable"
        );
        let message = err.to_string();
        assert!(
            message.contains("HTTPS_PROXY"),
            "the error must name the conflicting setting"
        );
        assert!(
            !message.contains("10.0.0.7") && !message.contains("proxy.example"),
            "the error must not echo a value"
        );
    }

    // An empty proxy variable counts as unset, as it does everywhere.
    let config = config_with(&[("COFFER_EGRESS_RELAY", "10.0.0.7"), ("HTTPS_PROXY", "")]).unwrap();
    assert!(config.egress_relay.is_some());
    assert!(config.https_proxy.is_none());
}

#[test]
fn unset_relay_setting_changes_nothing() {
    let config = config_with(&[]).unwrap();
    assert_eq!(config.egress_relay, None);

    let config = config_with(&[("HTTPS_PROXY", "http://proxy.example:3128")]).unwrap();
    assert_eq!(config.egress_relay, None);
    assert!(config.https_proxy.is_some());
}

#[test]
fn relay_setting_is_shown_in_config_debug() {
    let config = config_with(&[("COFFER_EGRESS_RELAY", "10.0.0.7")]).unwrap();
    let shown = format!("{config:?}");
    assert!(
        shown.contains("egress_relay: Some(10.0.0.7)"),
        "the Debug output must show the relay"
    );
}
