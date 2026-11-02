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
    let list = HostAllowList::parse(" Bridge.Example , beta.bridge.example.,localhost").unwrap();
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
        assert_eq!(addrs, [SocketAddr::new(relay, 443)]);
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
