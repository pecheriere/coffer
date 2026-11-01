//! Claiming a setup token: SSRF checks, error mapping and credentials that are
//! never echoed.

use super::{
    BOGUS_TOKEN, PASSWORD, PASSWORD_ENCODED, PASSWORD_MARKER, Reply, USER, contains, setup_token,
    world,
};
use axum::http::{Method, StatusCode};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use coffer_domain::ids::ConnectionId;
use coffer_domain::sync::SyncRunStatus;
use coffer_server::testkit::{captured_log_text, log_marker};
use serde_json::json;

#[tokio::test]
async fn claim_stores_sealed_access_url() {
    let world = world().await;
    world.script.claim(Reply::ok(&format!(
        "http://{USER}:{PASSWORD_ENCODED}@{{host}}/simplefin\n"
    )));

    let (status, raw, created) = world.connect(&setup_token(&world.claim_url())).await;

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["provider"], json!("simplefin"));
    assert_eq!(created["status"], json!("active"));
    let id: ConnectionId = created["id"].as_str().unwrap().parse().unwrap();
    let requests = world.mock.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, Method::POST);
    assert_eq!(requests[0].path, "/claim/x");
    assert!(!requests[0].authorization);

    let sealed = world
        .server
        .store
        .get_connection(id)
        .await
        .unwrap()
        .credentials
        .unwrap();
    assert!(!contains(&sealed, USER));
    assert!(!contains(&sealed, PASSWORD_MARKER));
    assert!(world.opens_to(id, sealed, &world.access_url()));
    assert!(!contains(&raw, PASSWORD_MARKER));
    assert!(!world.audit_text().await.contains(PASSWORD_MARKER));
    let log = captured_log_text();
    assert!(!log.contains(PASSWORD_MARKER));
    assert!(log.contains(&id.to_string()));
}

#[tokio::test]
async fn setup_token_for_unlisted_host_refused() {
    let world = world().await;
    for claim in [
        "https://internal.example/claim/x",
        "https://10.0.0.1/claim/x",
        "https://bridge.simplefin.org.evil.test/claim/x",
        "https://bridge.simplefin.org/claim/x",
        "http://127.0.0.1:1/claim/x",
        // User information that looks like the allowed host.
        "https://bridge.simplefin.org@evil.test/claim/x",
        // IPv6 literals.
        "https://[::1]/claim/x",
        "https://[2001:db8::1]:443/claim/x",
    ] {
        let (status, _, problem) = world.connect(&setup_token(claim)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(problem["code"], json!("setup_token_invalid"));
    }
    assert_eq!(world.mock.requests().len(), 0);
    assert_eq!(world.connection_count().await, 0);
}

#[tokio::test]
async fn access_url_for_unlisted_host_refused() {
    let world = world().await;
    for access in [
        format!("https://{USER}:{PASSWORD_ENCODED}@internal.example/simplefin"),
        format!("http://{USER}:{PASSWORD_ENCODED}@127.0.0.1:1/simplefin"),
        // No password, no user information, or a query.
        format!("http://{USER}@{{host}}/simplefin"),
        "http://{host}/simplefin".to_owned(),
        format!("http://{USER}:{PASSWORD_ENCODED}@{{host}}/simplefin?x=1"),
        "not a url".to_owned(),
    ] {
        world.script.claim(Reply::ok(&access));
        let (status, raw, problem) = world.connect(&setup_token(&world.claim_url())).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(problem["code"], json!("setup_token_invalid"));
        assert!(!contains(&raw, PASSWORD_MARKER));
    }
    // An oversized answer is refused too.
    world.script.claim(Reply::ok(&format!(
        "http://{USER}:{PASSWORD_ENCODED}@{{host}}/simplefin/{}",
        "a".repeat(2100)
    )));
    let (status, _, _) = world.connect(&setup_token(&world.claim_url())).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(world.connection_count().await, 0);
    assert!(
        world
            .mock
            .requests()
            .iter()
            .all(|request| request.path == "/claim/x")
    );
}

#[tokio::test]
async fn claim_redirect_not_followed() {
    let world = world().await;
    world.script.claim(Reply {
        status: StatusCode::FOUND,
        body: String::new(),
        location: Some("http://{host}/target".to_owned()),
    });

    let (status, _, problem) = world.connect(&setup_token(&world.claim_url())).await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(problem["code"], json!("setup_token_invalid"));
    let paths: Vec<String> = world
        .mock
        .requests()
        .into_iter()
        .map(|request| request.path)
        .collect();
    assert_eq!(paths, vec!["/claim/x".to_owned()]);
    assert_eq!(world.connection_count().await, 0);
}

#[tokio::test]
async fn claim_errors_map_to_their_problems() {
    let world = world().await;
    for (reply, status, code) in [
        (
            Reply::status(StatusCode::FORBIDDEN, "token already claimed"),
            StatusCode::UNPROCESSABLE_ENTITY,
            "setup_token_invalid",
        ),
        (
            Reply::status(StatusCode::NOT_FOUND, ""),
            StatusCode::UNPROCESSABLE_ENTITY,
            "setup_token_invalid",
        ),
        (
            Reply::status(StatusCode::SERVICE_UNAVAILABLE, "maintenance"),
            StatusCode::BAD_GATEWAY,
            "provider_error",
        ),
        // An error page over the claim answer's limit still reads as its
        // status.
        (
            Reply::status(StatusCode::SERVICE_UNAVAILABLE, &"<p>down</p>".repeat(400)),
            StatusCode::BAD_GATEWAY,
            "provider_error",
        ),
    ] {
        world.script.claim(reply);
        let (got, _, problem) = world.connect(&setup_token(&world.claim_url())).await;
        assert_eq!(got, status);
        assert_eq!(problem["code"], json!(code));
    }
    assert_eq!(world.connection_count().await, 0);
}

#[tokio::test]
async fn setup_token_never_echoed() {
    let world = world().await;
    let marker = log_marker();
    let oversized = format!("{BOGUS_TOKEN}{}", "A".repeat(2100));
    let not_a_url = STANDARD.encode(format!("{BOGUS_TOKEN} is no url"));
    for token in [BOGUS_TOKEN, oversized.as_str(), not_a_url.as_str(), ""] {
        let (status, raw, problem) = world.connect(token).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(problem["code"], json!("setup_token_invalid"));
        assert!(!contains(&raw, "SECRETTOKEN"));
    }
    assert!(!world.audit_text().await.contains("SECRETTOKEN"));
    let log = captured_log_text();
    assert!(
        log.contains(&marker),
        "the capture still holds this test's lines"
    );
    assert!(!log.contains("SECRETTOKEN"));
    assert_eq!(world.mock.requests().len(), 0);
}

/// Error answers that echo the setup token, the claim URL, the access URL
/// or its password reach no response, audit entry, run detail,
/// connection error or log line.
#[tokio::test]
async fn upstream_errors_never_echo_credentials() {
    let world = world().await;
    let marker = log_marker();
    let claim_marker = "/claim/x";
    let token = setup_token(&world.claim_url());
    let echo = format!(
        "token {token} claim {} access {} password {PASSWORD}",
        world.claim_url(),
        world.access_url()
    );
    for status in [StatusCode::FORBIDDEN, StatusCode::INTERNAL_SERVER_ERROR] {
        world.script.claim(Reply::status(status, &echo));
        let (_, raw, _) = world.connect(&token).await;
        assert!(!contains(&raw, &token));
        assert!(!contains(&raw, claim_marker));
        assert!(!contains(&raw, PASSWORD_MARKER));
    }
    // A claim answered with garbage that holds the password.
    world
        .script
        .claim(Reply::ok(&format!("{PASSWORD} {}", world.access_url())));
    let (_, raw, _) = world.connect(&token).await;
    assert!(!contains(&raw, PASSWORD_MARKER));

    let connection = world.seeded().await;
    let mut details = Vec::new();
    for status in [
        StatusCode::INTERNAL_SERVER_ERROR,
        StatusCode::BAD_REQUEST,
        StatusCode::PAYMENT_REQUIRED,
        StatusCode::FORBIDDEN,
    ] {
        world.script.bridge_reply(Reply::status(status, &echo));
        let run = world.sync(connection).await;
        assert_eq!(run.status, SyncRunStatus::Failed);
        details.push(run.detail.unwrap_or_default());
    }
    // A 200 answer that cannot be read, and one whose errors echo the
    // password.
    world.script.bridge_reply(Reply::ok(&echo));
    details.push(world.sync(connection).await.detail.unwrap_or_default());
    world.script.bridge_reply(Reply::ok(
        &json!({ "errors": [echo], "accounts": [] }).to_string(),
    ));
    details.push(world.sync(connection).await.detail.unwrap_or_default());

    let record = world.server.store.get_connection(connection).await.unwrap();
    let mut stored = details.join("\n");
    stored.push_str(&record.last_error.unwrap_or_default());
    let audit = world.audit_text().await;
    let log = captured_log_text();
    assert!(
        log.contains(&marker),
        "the capture still holds this test's lines"
    );
    for text in [&stored, &audit, &log] {
        assert!(!text.contains(&token));
        assert!(!text.contains(PASSWORD_MARKER));
        assert!(!text.contains(USER));
    }
    assert!(!stored.contains(claim_marker));
}
