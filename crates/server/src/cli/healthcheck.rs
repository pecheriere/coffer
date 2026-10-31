//! `coffer healthcheck`: a minimal HTTP/1.1 client for `GET /healthz`, so a
//! container image needs no curl or shell to check the server.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

/// How long the whole probe (connect, request, status line) may take.
pub(super) const TIMEOUT: Duration = Duration::from_secs(5);

/// The longest status line accepted, well above any real one.
const MAX_STATUS_LINE: usize = 1024;

/// Why the server is not healthy.
#[derive(Debug, thiserror::Error)]
pub(super) enum ProbeError {
    #[error("cannot reach {addr}")]
    Io {
        addr: SocketAddr,
        #[source]
        source: std::io::Error,
    },
    #[error("no answer from {addr} within {} ms", .timeout.as_millis())]
    TimedOut { addr: SocketAddr, timeout: Duration },
    #[error("{addr} sent a malformed HTTP response")]
    Malformed { addr: SocketAddr },
    #[error("{addr} answered /healthz with status {status}")]
    Unhealthy { addr: SocketAddr, status: u16 },
}

/// The address to probe for a server bound to `listen`: loopback when it
/// binds every interface (`0.0.0.0` or `[::]`), else `listen` itself.
fn target(listen: SocketAddr) -> SocketAddr {
    match listen.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => {
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), listen.port())
        }
        IpAddr::V6(ip) if ip.is_unspecified() => {
            SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), listen.port())
        }
        _ => listen,
    }
}

/// Sends `GET /healthz` to the server bound to `listen` and succeeds only on
/// a `200` status, all within `timeout`.
pub(super) async fn probe(listen: SocketAddr, timeout: Duration) -> Result<(), ProbeError> {
    let addr = target(listen);
    let status = tokio::time::timeout(timeout, status(addr))
        .await
        .map_err(|_| ProbeError::TimedOut { addr, timeout })??;
    if status == 200 {
        Ok(())
    } else {
        Err(ProbeError::Unhealthy { addr, status })
    }
}

/// The status code of `GET /healthz` at `addr`.
async fn status(addr: SocketAddr) -> Result<u16, ProbeError> {
    let io = |source| ProbeError::Io { addr, source };
    let mut stream = TcpStream::connect(addr).await.map_err(io)?;
    let request = format!("GET /healthz HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.map_err(io)?;

    let mut head = Vec::new();
    let mut buf = [0u8; 256];
    let line_end = loop {
        if let Some(end) = head.windows(2).position(|pair| pair == b"\r\n") {
            break end;
        }
        if head.len() > MAX_STATUS_LINE {
            return Err(ProbeError::Malformed { addr });
        }
        let read = stream.read(&mut buf).await.map_err(io)?;
        if read == 0 {
            return Err(ProbeError::Malformed { addr });
        }
        head.extend_from_slice(&buf[..read]);
    };
    parse_status_line(&head[..line_end]).ok_or(ProbeError::Malformed { addr })
}

/// The code in an HTTP/1.x status line such as `HTTP/1.1 200 OK`.
fn parse_status_line(line: &[u8]) -> Option<u16> {
    let line = std::str::from_utf8(line).ok()?;
    let mut parts = line.splitn(3, ' ');
    let version = parts.next()?;
    let code = parts.next()?;
    if !version.starts_with("HTTP/1.")
        || code.len() != 3
        || !code.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    code.parse().ok()
}

#[cfg(test)]
mod tests {
    use tokio::net::TcpListener;

    use super::{ProbeError, parse_status_line, probe, target};

    #[test]
    fn unspecified_addresses_are_probed_on_loopback() {
        for (listen, probed) in [
            ("0.0.0.0:8080", "127.0.0.1:8080"),
            ("[::]:8080", "[::1]:8080"),
            ("192.0.2.7:8080", "192.0.2.7:8080"),
            ("[2001:db8::7]:9000", "[2001:db8::7]:9000"),
        ] {
            assert_eq!(
                target(listen.parse().unwrap()),
                probed.parse().unwrap(),
                "{listen}"
            );
        }
    }

    #[test]
    fn status_lines() {
        assert_eq!(parse_status_line(b"HTTP/1.1 200 OK"), Some(200));
        assert_eq!(
            parse_status_line(b"HTTP/1.0 503 Service Unavailable"),
            Some(503)
        );
        assert_eq!(parse_status_line(b"HTTP/1.1 200"), Some(200));
        assert_eq!(parse_status_line(b"HTTP/2 200 OK"), None);
        assert_eq!(parse_status_line(b"SSH-2.0-OpenSSH_9.9"), None);
        assert_eq!(parse_status_line(b"HTTP/1.1 2000 OK"), None);
        assert_eq!(parse_status_line(b"HTTP/1.1 +20 OK"), None);
        assert_eq!(parse_status_line(b""), None);
    }

    #[tokio::test]
    async fn a_server_that_accepts_but_never_answers_times_out() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        // Held open, never read or written.
        let silent = tokio::spawn(async move { listener.accept().await.unwrap() });

        let timeout = std::time::Duration::from_millis(200);
        let result = probe(addr, timeout).await;

        assert!(
            matches!(result, Err(ProbeError::TimedOut { addr: a, .. }) if a == addr),
            "{result:?}"
        );
        drop(silent.await.unwrap());
    }
}
