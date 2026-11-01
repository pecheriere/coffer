//! Outbound HTTPS, the only way coffer reaches the network (spec §8.5).
//!
//! Every provider request goes through [`Egress::send`], which refuses any
//! destination that is not `https` on port 443 to a host in the caller's
//! [`HostAllowList`]. Redirects are never followed, the request and the body
//! are bounded in time and size, and without a proxy a destination must
//! resolve to public addresses only ([`is_public_ip`]), checked by the
//! resolver the connection itself uses, so a name cannot be re-pointed
//! between the check and the connect. With `HTTPS_PROXY` set, coffer resolves
//! only the proxy (unfiltered: an egress proxy usually sits on a private
//! address) and the proxy resolves targets and enforces its own policy. With
//! `COFFER_EGRESS_RELAY` set, nothing is resolved: every connection goes to
//! that one address, which only the operator's configuration can name, and the
//! URL, so the SNI and the certificate check, stay the real host's.

use std::error::Error as StdError;
use std::fmt;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::header::{CONTENT_TYPE, HeaderName, HeaderValue};
use reqwest::{Method, StatusCode, redirect};
use rustls_platform_verifier::BuilderVerifierExt as _;
use secrecy::{ExposeSecret as _, SecretString};
use thiserror::Error;
use url::{Host, Url};

use crate::config::Config;

/// Largest response body accepted, in bytes (16 MiB).
pub const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const USER_AGENT: &str = concat!("coffer/", env!("CARGO_PKG_VERSION"));

/// A refused or failed outbound request. Messages never carry a URL, query,
/// header or body, and no variant holds a value that could.
#[derive(Debug, Error)]
pub enum EgressError {
    /// The destination is not an allowed one: its host is not in the
    /// allow-list or is an IP address, or the URL names a port other than
    /// 443, carries user information or has a fragment.
    #[error("destination is not an allowed provider host")]
    HostNotAllowed,
    /// The destination does not use https.
    #[error("destination must use https")]
    NotHttps,
    /// The destination host did not resolve, or resolved to no public
    /// address.
    #[error("destination did not resolve to a public address")]
    Resolution,
    /// Connecting took longer than 10 s, or the whole exchange longer than
    /// 60 s.
    #[error("request timed out")]
    Timeout,
    /// The response body is larger than [`MAX_BODY_BYTES`], or than the
    /// caller's limit (see [`Egress::send_limited`]). The status lets the
    /// caller tell an oversized answer from an oversized error page.
    #[error("response body exceeds the size limit")]
    TooLarge {
        /// The response's status.
        status: StatusCode,
    },
    /// The connection, the TLS handshake, the proxy or the exchange failed.
    #[error("connection failed")]
    Transport {
        /// What failed, reduced to fixed values.
        #[source]
        cause: TransportCause,
    },
    /// The client or a request could not be set up.
    #[error("egress configuration is invalid: {0}")]
    Config(&'static str),
}

/// The redacted cause of an [`EgressError::Transport`]: the stage that failed
/// and the I/O error kind, if any. The underlying `reqwest` error is reduced
/// to these fixed values because its text may name the proxy, whose URL can
/// carry credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransportCause {
    stage: TransportStage,
    io: Option<io::ErrorKind>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransportStage {
    Connect,
    Tls,
    Request,
    Body,
}

impl fmt::Display for TransportCause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let stage = match self.stage {
            TransportStage::Connect => "connecting",
            TransportStage::Tls => "the TLS handshake",
            TransportStage::Request => "the request",
            TransportStage::Body => "reading the response body",
        };
        match self.io {
            Some(kind) => write!(f, "{stage} failed: {kind}"),
            None => write!(f, "{stage} failed"),
        }
    }
}

impl StdError for TransportCause {}

/// The DNS names a provider may reach. Matching is exact: no wildcards and no
/// subdomains.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostAllowList(Vec<String>);

impl HostAllowList {
    /// Parses a comma-separated list of DNS names (whitespace around a name
    /// and one trailing dot are ignored, case is folded).
    ///
    /// # Errors
    ///
    /// [`EgressError::Config`] when the list is empty or an entry is empty,
    /// an IP address, a wildcard, carries a port, a scheme or a path, or is
    /// otherwise not a DNS name.
    pub fn parse(list: &str) -> Result<HostAllowList, EgressError> {
        list.split(',')
            .map(parse_host)
            .collect::<Result<Vec<_>, _>>()
            .map(HostAllowList)
    }

    fn contains(&self, host: &str) -> bool {
        self.0.iter().any(|allowed| allowed == host)
    }
}

fn parse_host(raw: &str) -> Result<String, EgressError> {
    const INVALID: EgressError = EgressError::Config("expected comma-separated DNS host names");
    let lowered = raw.trim().to_ascii_lowercase();
    let host = lowered.strip_suffix('.').unwrap_or(&lowered);
    let label_ok = |label: &str| {
        (1..=63).contains(&label.len())
            && label
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !label.starts_with('-')
            && !label.ends_with('-')
    };
    if host.is_empty() || host.len() > 253 || !host.split('.').all(label_ok) {
        return Err(INVALID);
    }
    // A URL whose last label is a number (decimal, or hex with 0x) is read as
    // an IPv4 address, so such a name could never match a DNS host.
    let last = host.rsplit('.').next().unwrap_or(host);
    let numeric = last.bytes().all(|b| b.is_ascii_digit())
        || last
            .strip_prefix("0x")
            .is_some_and(|hex| hex.bytes().all(|b| b.is_ascii_hexdigit()));
    if numeric {
        return Err(INVALID);
    }
    Ok(host.to_owned())
}

#[cfg(feature = "testkit")]
impl HostAllowList {
    /// An allow-list for a loopback mock: with [`Egress::for_tests`], it
    /// accepts `http://127.0.0.1:<port>` and nothing else.
    #[must_use]
    pub fn loopback(port: u16) -> HostAllowList {
        HostAllowList(vec![format!("127.0.0.1:{port}")])
    }

    /// An allow-list that allows nothing.
    #[must_use]
    pub fn none() -> HostAllowList {
        HostAllowList(Vec::new())
    }

    fn allows_loopback(&self, url: &Url) -> bool {
        url.scheme() == "http"
            && url.host() == Some(Host::Ipv4(Ipv4Addr::LOCALHOST))
            && url.username().is_empty()
            && url.password().is_none()
            && url
                .port()
                .is_some_and(|port| self.contains(&format!("127.0.0.1:{port}")))
    }
}

/// The outbound HTTP client. Cheap to clone: clones share one connection
/// pool.
#[derive(Clone)]
pub struct Egress {
    client: reqwest::Client,
    #[cfg(feature = "testkit")]
    loopback_http: bool,
}

impl fmt::Debug for Egress {
    /// Shows nothing of the client: its proxy settings may hold credentials.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Egress").finish_non_exhaustive()
    }
}

/// An outbound request.
pub struct OutboundRequest {
    /// Method.
    pub method: Method,
    /// Destination, checked by [`Egress::check_url`] before anything is sent.
    pub url: Url,
    /// User name and password for HTTP basic authentication.
    pub basic_auth: Option<(String, SecretString)>,
    /// Extra request headers.
    pub headers: Vec<(HeaderName, HeaderValue)>,
    /// Content type and body.
    pub body: Option<(&'static str, Vec<u8>)>,
}

/// A response: its status (a 3xx is returned as is, never followed) and its
/// whole body.
pub struct OutboundResponse {
    /// Status.
    pub status: StatusCode,
    /// Body, at most [`MAX_BODY_BYTES`].
    pub body: Vec<u8>,
}

impl fmt::Debug for OutboundResponse {
    /// Shows the body's length only: a provider response can hold
    /// credentials.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OutboundResponse")
            .field("status", &self.status)
            .field("body_len", &self.body.len())
            .finish()
    }
}

impl Egress {
    /// Builds the client: rustls with the `ring` provider and the platform
    /// verifier (the system CA bundle; `SSL_CERT_FILE` is honoured on Linux),
    /// TLS 1.2 or 1.3, no redirects, a 10 s connect and 60 s total timeout,
    /// no cookies, no proxy read from the environment, and `proxy` (from
    /// `HTTPS_PROXY`) for every https request when given.
    ///
    /// # Errors
    ///
    /// [`EgressError::Config`] when no CA certificate can be loaded, the
    /// proxy URL is not `http` or `https` with a host, or the client cannot
    /// be built.
    pub fn new(proxy: Option<&Url>) -> Result<Egress, EgressError> {
        let tls = platform_tls()?;

        let resolver = match proxy {
            None => PublicResolver::Public,
            Some(proxy) => {
                if !matches!(proxy.scheme(), "http" | "https") {
                    return Err(EgressError::Config("the proxy must use http or https"));
                }
                match proxy.host() {
                    Some(Host::Domain(domain)) => {
                        PublicResolver::ProxyOnly(Some(normalize_name(domain).into()))
                    }
                    Some(Host::Ipv4(_) | Host::Ipv6(_)) => PublicResolver::ProxyOnly(None),
                    None => return Err(EgressError::Config("the proxy URL has no host")),
                }
            }
        };

        let mut builder = client_builder(tls, resolver);
        if let Some(proxy) = proxy {
            // Credentials in the proxy URL become its Proxy-Authorization
            // header; no NO_PROXY list is attached.
            let proxy = reqwest::Proxy::https(proxy.as_str())
                .map_err(|_| EgressError::Config("the proxy URL is invalid"))?;
            builder = builder.proxy(proxy);
        }
        Egress::build(builder)
    }

    /// The production client for `config`: through its relay when it has one
    /// (see [`Egress::new`] for all that does not change), else as
    /// [`Egress::new`] with its proxy.
    ///
    /// # Errors
    ///
    /// As [`Egress::new`].
    pub fn from_config(config: &Config) -> Result<Egress, EgressError> {
        match config.egress_relay {
            Some(relay) => {
                let resolver = PublicResolver::relay(relay);
                Egress::build(client_builder(platform_tls()?, resolver))
            }
            None => Egress::new(config.https_proxy.as_ref()),
        }
    }

    fn build(builder: reqwest::ClientBuilder) -> Result<Egress, EgressError> {
        let client = builder
            .build()
            .map_err(|_| EgressError::Config("the HTTP client could not be built"))?;
        Ok(Egress {
            client,
            #[cfg(feature = "testkit")]
            loopback_http: false,
        })
    }

    /// A production client that also accepts `http://127.0.0.1:<port>` for
    /// an allow-list from [`HostAllowList::loopback`], for tests against a
    /// loopback mock.
    ///
    /// # Panics
    ///
    /// When the client cannot be built (no CA certificate on the host).
    #[cfg(feature = "testkit")]
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "test helper: a client that cannot be built must abort the test using it"
    )]
    pub fn for_tests() -> Egress {
        Egress {
            loopback_http: true,
            ..Egress::new(None).expect("the test client builds")
        }
    }

    /// A production client whose connections go to `relay`, port included
    /// (a test server cannot listen on 443), and which trusts `extra_root` on
    /// top of the platform's roots. Its destination checks are the production
    /// ones: it has no [`Egress::for_tests`] exception.
    ///
    /// # Panics
    ///
    /// When the client cannot be built.
    #[cfg(feature = "testkit")]
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "test helper: a client that cannot be built must abort the test using it"
    )]
    pub fn relay_for_tests(
        relay: SocketAddr,
        extra_root: rustls::pki_types::CertificateDer<'static>,
    ) -> Egress {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let verifier = rustls_platform_verifier::Verifier::new_with_extra_roots(
            [extra_root],
            Arc::clone(&provider),
        )
        .expect("the test root is valid");
        let tls = tls_versions(provider)
            .expect("the test client builds")
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(verifier))
            .with_no_client_auth();
        Egress::build(client_builder(tls, PublicResolver::Relay(relay)))
            .expect("the test client builds")
    }

    /// Checks a destination: scheme `https`; host a DNS name in `allowed`
    /// (case-insensitive, a trailing dot ignored); port absent or 443; no
    /// user information; no fragment.
    ///
    /// # Errors
    ///
    /// [`EgressError::NotHttps`] or [`EgressError::HostNotAllowed`].
    pub fn check_url(&self, url: &Url, allowed: &HostAllowList) -> Result<(), EgressError> {
        #[cfg(feature = "testkit")]
        if self.loopback_http && allowed.allows_loopback(url) {
            return Ok(());
        }
        if url.scheme() != "https" {
            return Err(EgressError::NotHttps);
        }
        // `port()` is `None` for an explicit default port (443).
        let authority_ok = url.port().is_none()
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none();
        match url.host() {
            Some(Host::Domain(domain))
                if authority_ok && allowed.contains(&normalize_name(domain)) =>
            {
                Ok(())
            }
            _ => Err(EgressError::HostNotAllowed),
        }
    }

    /// Sends `request` after [`Egress::check_url`] and reads the whole
    /// response body, up to [`MAX_BODY_BYTES`].
    ///
    /// # Errors
    ///
    /// [`EgressError`]: a refused destination, a resolution failure, a
    /// timeout, an oversized body or a transport failure.
    pub async fn send(
        &self,
        request: OutboundRequest,
        allowed: &HostAllowList,
    ) -> Result<OutboundResponse, EgressError> {
        self.send_limited(request, allowed, MAX_BODY_BYTES).await
    }

    /// As [`Egress::send`], with the body limited to `max_body` bytes (at
    /// most [`MAX_BODY_BYTES`]): a declared length over it is refused before
    /// reading, and reading stops as soon as the body would exceed it.
    ///
    /// # Errors
    ///
    /// As [`Egress::send`].
    pub async fn send_limited(
        &self,
        request: OutboundRequest,
        allowed: &HostAllowList,
        max_body: usize,
    ) -> Result<OutboundResponse, EgressError> {
        let max_body = max_body.min(MAX_BODY_BYTES);
        self.check_url(&request.url, allowed)?;
        let mut builder = self.client.request(request.method, request.url);
        for (name, value) in request.headers {
            builder = builder.header(name, value);
        }
        if let Some((user, password)) = &request.basic_auth {
            builder = builder.basic_auth(user, Some(password.expose_secret()));
        }
        if let Some((content_type, body)) = request.body {
            let content_type = HeaderValue::from_str(content_type)
                .map_err(|_| EgressError::Config("invalid content type"))?;
            builder = builder.header(CONTENT_TYPE, content_type).body(body);
        }

        let mut response = builder.send().await.map_err(classify)?;
        let status = response.status();
        if response
            .content_length()
            .is_some_and(|len| usize::try_from(len).map_or(true, |len| len > max_body))
        {
            return Err(EgressError::TooLarge { status });
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(classify)? {
            if chunk.len() > max_body - body.len() {
                return Err(EgressError::TooLarge { status });
            }
            body.extend_from_slice(&chunk);
        }
        Ok(OutboundResponse { status, body })
    }
}

/// The TLS settings every client shares, up to the choice of verifier.
fn tls_versions(
    provider: Arc<rustls::crypto::CryptoProvider>,
) -> Result<rustls::ConfigBuilder<rustls::ClientConfig, rustls::WantsVerifier>, EgressError> {
    rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
        .map_err(|_| EgressError::Config("TLS 1.2 and 1.3 are unavailable"))
}

fn platform_tls() -> Result<rustls::ClientConfig, EgressError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    Ok(tls_versions(provider)?
        .with_platform_verifier()
        .map_err(|_| EgressError::Config("no CA certificate could be loaded"))?
        .with_no_client_auth())
}

/// The settings every client shares: no proxy from the environment, no
/// redirects, no referer, and the time limits.
fn client_builder(tls: rustls::ClientConfig, resolver: PublicResolver) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .tls_backend_preconfigured(tls)
        .no_proxy()
        .redirect(redirect::Policy::none())
        .referer(false)
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .user_agent(USER_AGENT)
        .dns_resolver(Arc::new(resolver))
}

/// Lowercases a DNS name and drops one trailing dot.
fn normalize_name(name: &str) -> String {
    let lowered = name.to_ascii_lowercase();
    match lowered.strip_suffix('.') {
        Some(stripped) => stripped.to_owned(),
        None => lowered,
    }
}

/// Reduces a `reqwest` error to an [`EgressError`] holding fixed values only.
fn classify(error: reqwest::Error) -> EgressError {
    let error = error.without_url();
    if error.is_timeout() {
        return EgressError::Timeout;
    }
    let mut stage = if error.is_connect() {
        TransportStage::Connect
    } else if error.is_body() || error.is_decode() {
        TransportStage::Body
    } else {
        TransportStage::Request
    };
    let mut io_kind = None;
    let mut source = error.source();
    while let Some(cause) = source {
        if cause.is::<ResolveRefused>() {
            return EgressError::Resolution;
        }
        if cause.is::<rustls::Error>() {
            stage = TransportStage::Tls;
        }
        if let Some(io_error) = cause.downcast_ref::<io::Error>() {
            io_kind.get_or_insert(io_error.kind());
            // A failed handshake arrives as an `io::Error` holding an
            // `io::Error` holding the rustls error, and `source()` skips an
            // `io::Error`'s payload, so look through the payloads here.
            let mut payload = io_error.get_ref();
            while let Some(inner) = payload {
                if inner.is::<rustls::Error>() {
                    stage = TransportStage::Tls;
                }
                payload = inner
                    .downcast_ref::<io::Error>()
                    .and_then(io::Error::get_ref);
            }
        }
        source = cause.source();
    }
    EgressError::Transport {
        cause: TransportCause { stage, io: io_kind },
    }
}

/// The resolver every connection uses.
enum PublicResolver {
    /// No proxy: a name resolves to its public addresses only.
    Public,
    /// A proxy is configured: only its host name (if it has one) resolves,
    /// unfiltered; any other name is refused, since targets are the proxy's
    /// to resolve.
    ProxyOnly(Option<Arc<str>>),
    /// A relay is configured: every name answers with this address, with no
    /// lookup and no filter. Port 0 stands for the port of the URL.
    Relay(SocketAddr),
}

impl PublicResolver {
    fn relay(relay: IpAddr) -> PublicResolver {
        PublicResolver::Relay(SocketAddr::new(relay, 0))
    }
}

/// A name that did not resolve or resolved to no acceptable address.
#[derive(Debug, Error)]
#[error("no acceptable address")]
struct ResolveRefused;

impl Resolve for PublicResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = normalize_name(name.as_str());
        let filtered = match self {
            PublicResolver::Relay(relay) => {
                let addrs: Addrs = Box::new(std::iter::once(*relay));
                return Box::pin(std::future::ready(Ok(addrs)));
            }
            PublicResolver::Public => Some(true),
            PublicResolver::ProxyOnly(Some(proxy)) if **proxy == *host => Some(false),
            PublicResolver::ProxyOnly(_) => None,
        };
        Box::pin(async move {
            let refused = || -> Box<dyn StdError + Send + Sync> { Box::new(ResolveRefused) };
            let filtered = filtered.ok_or_else(refused)?;
            let found = tokio::net::lookup_host((host.as_str(), 0))
                .await
                .map_err(|_| refused())?;
            let addrs: Vec<SocketAddr> = found
                .filter(|addr| !filtered || is_public_ip(addr.ip()))
                .collect();
            if addrs.is_empty() {
                return Err(refused());
            }
            let addrs: Addrs = Box::new(addrs.into_iter());
            Ok(addrs)
        })
    }
}

/// Whether `ip` is globally reachable: false for every range of the IANA
/// IPv4 and IPv6 Special-Purpose Address Registries that is not, and for
/// multicast. IPv4-mapped, NAT64 (`64:ff9b::/96`) and 6to4 addresses are
/// judged by the IPv4 address they embed. Some blocks are refused whole
/// although a few addresses in them are global (`192.0.0.0/24`,
/// `2001::/23`); `64:ff9b:1::/48` (local-use NAT64, where the embedded
/// address's position depends on the local prefix length) and every IPv6
/// address outside `2000::/3` are refused.
#[must_use]
pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_public_v4(ip),
        IpAddr::V6(ip) => is_public_v6(ip),
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    const BLOCKED: [(Ipv4Addr, u32); 15] = [
        (Ipv4Addr::UNSPECIFIED, 8),           // "this network"
        (Ipv4Addr::new(10, 0, 0, 0), 8),      // private use
        (Ipv4Addr::new(100, 64, 0, 0), 10),   // shared address space (CGNAT)
        (Ipv4Addr::new(127, 0, 0, 0), 8),     // loopback
        (Ipv4Addr::new(169, 254, 0, 0), 16),  // link local
        (Ipv4Addr::new(172, 16, 0, 0), 12),   // private use
        (Ipv4Addr::new(192, 0, 0, 0), 24),    // IETF protocol assignments
        (Ipv4Addr::new(192, 0, 2, 0), 24),    // documentation (TEST-NET-1)
        (Ipv4Addr::new(192, 88, 99, 0), 24),  // deprecated 6to4 relay anycast
        (Ipv4Addr::new(192, 168, 0, 0), 16),  // private use
        (Ipv4Addr::new(198, 18, 0, 0), 15),   // benchmarking
        (Ipv4Addr::new(198, 51, 100, 0), 24), // documentation (TEST-NET-2)
        (Ipv4Addr::new(203, 0, 113, 0), 24),  // documentation (TEST-NET-3)
        (Ipv4Addr::new(224, 0, 0, 0), 4),     // multicast
        (Ipv4Addr::new(240, 0, 0, 0), 4),     // reserved, and limited broadcast
    ];
    let bits = u32::from(ip);
    !BLOCKED
        .iter()
        .any(|&(net, len)| bits & (u32::MAX << (32 - len)) == u32::from(net))
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    let bits = u128::from(ip);
    let within = |net: u128, len: u32| bits & (u128::MAX << (128 - len)) == net;
    let o = ip.octets();
    if within(0xffff_u128 << 32, 96) {
        // IPv4-mapped.
        return is_public_v4(Ipv4Addr::new(o[12], o[13], o[14], o[15]));
    }
    if within(0x0064_ff9b_u128 << 96, 96) {
        // Well-known NAT64 prefix.
        return is_public_v4(Ipv4Addr::new(o[12], o[13], o[14], o[15]));
    }
    if !within(0x2000_u128 << 112, 3) {
        // Outside global unicast: unspecified, loopback, IPv4-compatible,
        // local-use NAT64, discard-only, SRv6 SIDs, unique local, link
        // local, site local and multicast all live here.
        return false;
    }
    if within(0x2001_u128 << 112, 23)
        || within(0x2001_0db8_u128 << 96, 32)
        || within(0x3fff_u128 << 112, 20)
    {
        // IETF protocol assignments (Teredo, benchmarking, ORCHID),
        // documentation.
        return false;
    }
    if within(0x2002_u128 << 112, 16) {
        // 6to4.
        return is_public_v4(Ipv4Addr::new(o[2], o[3], o[4], o[5]));
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::net::{Ipv4Addr, Ipv6Addr};

    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpListener;

    fn url(raw: &str) -> Url {
        Url::parse(raw).unwrap()
    }

    fn get(raw: &str) -> OutboundRequest {
        OutboundRequest {
            method: Method::GET,
            url: url(raw),
            basic_auth: None,
            headers: Vec::new(),
            body: None,
        }
    }

    #[test]
    fn host_allow_list_parsing() {
        let list =
            HostAllowList::parse(" Bridge.Example , beta.bridge.example.,localhost").unwrap();
        assert_eq!(
            list,
            HostAllowList(vec![
                "bridge.example".to_owned(),
                "beta.bridge.example".to_owned(),
                "localhost".to_owned(),
            ])
        );
        for bad in [
            "",
            " , ",
            "bridge.example,,other.example",
            "127.0.0.1",
            "127.1",
            "[::1]",
            "::1",
            "*.bridge.example",
            "bridge.example:443",
            "https://bridge.example",
            "bridge.example/x",
            "user@bridge.example",
            "bri dge.example",
            "-bridge.example",
            "bridge-.example",
            "bridge..example",
        ] {
            assert!(
                matches!(HostAllowList::parse(bad), Err(EgressError::Config(_))),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn check_url_rules() {
        let egress = Egress::new(None).unwrap();
        let allowed = HostAllowList::parse("bridge.example").unwrap();
        for ok in [
            "https://bridge.example/x",
            "https://BRIDGE.example/x?q=1",
            "https://bridge.example./x",
            "https://bridge.example:443/x",
        ] {
            assert!(egress.check_url(&url(ok), &allowed).is_ok(), "{ok}");
        }
        assert!(matches!(
            egress.check_url(&url("http://bridge.example/x"), &allowed),
            Err(EgressError::NotHttps)
        ));
        for refused in [
            "https://93.184.215.14/x",
            "https://[2606:4700::1111]/x",
            "https://bridge.example:8443/x",
            "https://user:pw@bridge.example/x",
            "https://user@bridge.example/x",
            "https://bridge.example/x#frag",
            "https://other.example/x",
            "https://bridge.example.evil.test/x",
            "https://evilbridge.example/x",
            "https://sub.bridge.example/x",
        ] {
            assert!(
                matches!(
                    egress.check_url(&url(refused), &allowed),
                    Err(EgressError::HostNotAllowed)
                ),
                "{refused}"
            );
        }
    }

    #[test]
    fn is_public_ip_table() {
        let v4 = |s: &str| IpAddr::V4(s.parse::<Ipv4Addr>().unwrap());
        let v6 = |s: &str| IpAddr::V6(s.parse::<Ipv6Addr>().unwrap());
        let private = [
            // IPv4 special-purpose ranges, first and last address where useful.
            v4("0.0.0.0"),
            v4("0.255.255.255"),
            v4("10.0.0.1"),
            v4("10.255.255.255"),
            v4("100.64.0.1"),
            v4("100.127.255.255"),
            v4("127.0.0.1"),
            v4("127.255.255.254"),
            v4("169.254.169.254"),
            v4("172.16.0.1"),
            v4("172.31.255.255"),
            v4("192.0.0.1"),
            v4("192.0.0.255"),
            v4("192.0.2.1"),
            v4("192.88.99.1"),
            v4("192.168.1.1"),
            v4("198.18.0.1"),
            v4("198.19.255.255"),
            v4("198.51.100.1"),
            v4("203.0.113.1"),
            v4("224.0.0.1"),
            v4("239.255.255.255"),
            v4("240.0.0.1"),
            v4("255.255.255.254"),
            v4("255.255.255.255"),
            // IPv6 special-purpose ranges.
            v6("::"),
            v6("::1"),
            v6("::7f00:1"),
            v6("::808:808"),
            v6("::ffff:127.0.0.1"),
            v6("::ffff:10.0.0.1"),
            v6("::ffff:169.254.169.254"),
            v6("::ffff:0:808:808"),
            v6("64:ff9b::7f00:1"),
            v6("64:ff9b::a00:1"),
            v6("64:ff9b:1::7f00:1"),
            v6("64:ff9b:1::808:808"),
            v6("100::1"),
            v6("2001::1"),
            v6("2001:0:4136:e378:8000:63bf:3fff:fdd2"),
            v6("2001:2::1"),
            v6("2001:db8::1"),
            v6("2002:7f00:1::"),
            v6("2002:a00:1::1"),
            v6("2002:c0a8:101::1"),
            v6("3fff::1"),
            v6("5f00::1"),
            v6("fc00::1"),
            v6("fd12:3456::1"),
            v6("fe80::1"),
            v6("fec0::1"),
            v6("ff02::1"),
        ];
        for ip in private {
            assert!(!is_public_ip(ip), "{ip} must not be public");
        }
        let public = [
            v4("8.8.8.8"),
            v4("1.1.1.1"),
            v4("93.184.215.14"),
            v4("100.63.255.255"),
            v4("100.128.0.1"),
            v4("172.15.255.255"),
            v4("172.32.0.1"),
            v4("223.255.255.255"),
            v6("2606:4700::1111"),
            v6("2a00:1450:4001::200e"),
            v6("::ffff:8.8.8.8"),
            v6("64:ff9b::808:808"),
            v6("2002:808:808::1"),
        ];
        for ip in public {
            assert!(is_public_ip(ip), "{ip} must be public");
        }
    }

    #[tokio::test]
    async fn resolver_refuses_private_targets() {
        let egress = Egress::new(None).unwrap();
        let allowed = HostAllowList::parse("localhost").unwrap();
        let err = egress
            .send(get("https://localhost/"), &allowed)
            .await
            .unwrap_err();
        assert!(matches!(err, EgressError::Resolution), "{err:?}");
    }

    #[tokio::test]
    async fn relay_resolver_answers_only_the_configured_address() {
        let relay = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 7));
        for name in ["bridge.example", "localhost", "metadata.internal"] {
            let name: Name = name.parse().unwrap();
            let addrs: Vec<SocketAddr> = PublicResolver::relay(relay)
                .resolve(name)
                .await
                .unwrap()
                .collect();
            // Port 0 makes the client use the URL's port, which `check_url`
            // holds to 443.
            assert_eq!(addrs, [SocketAddr::new(relay, 0)]);
        }
    }

    /// Accepts connections, records the first request line of each and
    /// answers 403.
    async fn recording_proxy() -> (u16, tokio::sync::mpsc::UnboundedReceiver<String>) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let mut stream = BufReader::new(stream);
                let mut line = String::new();
                if stream.read_line(&mut line).await.is_ok() {
                    let _ = tx.send(line.trim_end().to_owned());
                }
                let _ = stream
                    .get_mut()
                    .write_all(b"HTTP/1.1 403 Forbidden\r\ncontent-length: 0\r\n\r\n")
                    .await;
            }
        });
        (port, rx)
    }

    #[tokio::test]
    async fn proxy_on_private_address_is_reachable() {
        let (port, mut seen) = recording_proxy().await;
        let proxy = url(&format!("http://127.0.0.1:{port}"));
        let egress = Egress::new(Some(&proxy)).unwrap();
        let allowed = HostAllowList::parse("bridge.example").unwrap();

        let err = egress
            .send(get("https://bridge.example/x"), &allowed)
            .await
            .unwrap_err();
        assert!(matches!(err, EgressError::Transport { .. }), "{err:?}");
        assert_eq!(
            seen.recv().await.as_deref(),
            Some("CONNECT bridge.example:443 HTTP/1.1")
        );

        let err = egress
            .send(get("https://unlisted.example/x"), &allowed)
            .await
            .unwrap_err();
        assert!(matches!(err, EgressError::HostNotAllowed), "{err:?}");
        assert!(
            seen.try_recv().is_err(),
            "an unlisted host reached the proxy"
        );
    }

    #[tokio::test]
    async fn proxy_host_name_resolves_unfiltered() {
        // `localhost` resolves to loopback addresses, which the filter refuses
        // for any target but must accept for the configured proxy.
        let (port, mut seen) = recording_proxy().await;
        let proxy = url(&format!("http://localhost:{port}"));
        let egress = Egress::new(Some(&proxy)).unwrap();
        let allowed = HostAllowList::parse("bridge.example").unwrap();

        let err = egress
            .send(get("https://bridge.example/x"), &allowed)
            .await
            .unwrap_err();
        assert!(matches!(err, EgressError::Transport { .. }), "{err:?}");
        assert_eq!(
            seen.recv().await.as_deref(),
            Some("CONNECT bridge.example:443 HTTP/1.1")
        );
    }
}
