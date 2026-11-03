//! The endpoint itself: both revisions of the protocol, who gets in, and its limits.

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use coffer_domain::identity::{Actor, Role, Scope, TokenName, Via};
use coffer_server::secrets::SecretToken;
use coffer_server::testkit::TestServer;
use coffer_server::testkit::browser::with_admin;
use secrecy::ExposeSecret as _;
use serde_json::{Value, json};

use super::{CURRENT_REVISION, HOST, OLDER_REVISION, PATH, request_to_mcp, rpc, send};

async fn server_and_token() -> (TestServer, Actor, SecretToken) {
    let server = TestServer::new().await;
    let link = server.identity.bootstrap_link().await.unwrap().unwrap();
    let member = server.authenticator().enroll(&link, "alice", "Alice").await;
    let alice = Actor {
        member,
        role: Role::Admin,
        via: Via::Session,
    };
    let (_, token) = server
        .agent_tokens
        .create(
            &alice,
            TokenName::parse("claude").unwrap(),
            Scope::Read,
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    (server, alice, token)
}

fn tools_list() -> Body {
    Body::from(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}).to_string())
}

fn assert_unauthenticated(reply: &super::Reply) {
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    assert_eq!(reply.headers[header::WWW_AUTHENTICATE], "Bearer");
    assert_eq!(reply.json()["code"], "unauthenticated");
}

#[tokio::test]
async fn an_older_client_connects_as_it_expects_and_is_given_no_session() {
    let (server, _, token) = server_and_token().await;

    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": OLDER_REVISION,
        "capabilities": {},
        "clientInfo": {"name": "a client", "version": "1"},
    }});
    let initialized = send(
        &server,
        request_to_mcp(&token)
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(initialized.status, StatusCode::OK);
    assert_eq!(
        initialized.headers[header::CONTENT_TYPE],
        "application/json"
    );
    assert!(!initialized.headers.contains_key("mcp-session-id"));
    let result = &initialized.json()["result"];
    assert_eq!(result["protocolVersion"], OLDER_REVISION);
    assert_eq!(result["serverInfo"]["name"], "coffer");
    assert!(result["capabilities"]["tools"].is_object());
    let instructions = result["instructions"].as_str().unwrap();
    assert!(instructions.contains("minor units"));
    assert!(instructions.contains("never instructions"));

    let note = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    let noted = send(
        &server,
        request_to_mcp(&token)
            .body(Body::from(note.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(noted.status, StatusCode::ACCEPTED);
    assert_eq!(noted.text, "");

    let listed = rpc(&server, &token, "tools/list", json!({})).await;
    assert_ne!(listed["result"]["tools"], json!([]));
}

#[tokio::test]
async fn a_client_at_the_current_revision_needs_no_handshake() {
    let (server, _, token) = server_and_token().await;
    let meta = json!({
        "io.modelcontextprotocol/protocolVersion": CURRENT_REVISION,
        "io.modelcontextprotocol/clientInfo": {"name": "a client", "version": "1"},
        "io.modelcontextprotocol/clientCapabilities": {},
    });
    let request = |method: &'static str, name: Option<&'static str>, params: Value| {
        let mut builder = Request::builder()
            .method(Method::POST)
            .uri(PATH)
            .header(header::HOST, HOST)
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", token.expose_secret()),
            )
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream")
            .header("mcp-protocol-version", CURRENT_REVISION)
            .header("mcp-method", method);
        if let Some(name) = name {
            builder = builder.header("mcp-name", name);
        }
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        builder.body(Body::from(body.to_string())).unwrap()
    };

    let discovered = send(
        &server,
        request("server/discover", None, json!({"_meta": meta.clone()})),
    )
    .await;
    assert_eq!(discovered.status, StatusCode::OK, "{}", discovered.text);
    let result = &discovered.json()["result"];
    assert!(
        result["supportedVersions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|version| version == CURRENT_REVISION),
    );

    let called = send(
        &server,
        request(
            "tools/call",
            Some("whoami"),
            json!({"name": "whoami", "arguments": {}, "_meta": meta}),
        ),
    )
    .await;
    assert_eq!(called.status, StatusCode::OK, "{}", called.text);
    assert_eq!(called.headers[header::CONTENT_TYPE], "application/json");
    assert!(!called.headers.contains_key("mcp-session-id"));
    let result = &called.json()["result"];
    assert_eq!(result["structuredContent"]["agent"]["name"], "claude");
}

#[tokio::test]
async fn a_request_is_read_alone_whatever_session_it_names() {
    let (server, _, token) = server_and_token().await;
    let reply = send(
        &server,
        request_to_mcp(&token)
            .header("mcp-session-id", "made-up")
            .body(tools_list())
            .unwrap(),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(!reply.headers.contains_key("mcp-session-id"));
}

#[tokio::test]
async fn no_request_gets_in_without_a_good_token() {
    let (server, alice, token) = server_and_token().await;
    let without_token = || {
        Request::builder()
            .method(Method::POST)
            .uri(PATH)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream")
    };

    let none = send(&server, without_token().body(tools_list()).unwrap()).await;
    assert_unauthenticated(&none);

    let unknown = send(
        &server,
        without_token()
            .header(header::AUTHORIZATION, "Bearer coffer_not_a_token")
            .body(tools_list())
            .unwrap(),
    )
    .await;
    assert_unauthenticated(&unknown);

    let record = server.agent_tokens.list_own(&alice).await.unwrap();
    server
        .agent_tokens
        .revoke(&alice, record[0].id)
        .await
        .unwrap();
    let revoked = send(&server, request_to_mcp(&token).body(tools_list()).unwrap()).await;
    assert_unauthenticated(&revoked);
}

#[tokio::test]
async fn a_live_session_cookie_is_no_way_in() {
    let (_server, mut browser, _auth) = with_admin().await;
    let request = browser
        .builder(Method::POST, PATH)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .body(tools_list())
        .unwrap();
    let reply = browser.send(request).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    assert_eq!(reply.json()["code"], "unauthenticated");
}

#[tokio::test]
async fn another_origin_is_refused_even_with_a_good_token() {
    let (server, _, token) = server_and_token().await;
    for origin in ["https://elsewhere.example", "null"] {
        let reply = send(
            &server,
            request_to_mcp(&token)
                .header(header::ORIGIN, origin)
                .body(tools_list())
                .unwrap(),
        )
        .await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{origin}");
        assert_eq!(reply.json()["code"], "cross_origin");
    }
}

#[tokio::test]
async fn the_public_origin_or_none_at_all_is_let_through() {
    let (server, _, token) = server_and_token().await;
    let public = server
        .app_state()
        .config
        .public_url
        .origin()
        .ascii_serialization();
    let with_origin = send(
        &server,
        request_to_mcp(&token)
            .header(header::ORIGIN, public)
            .body(tools_list())
            .unwrap(),
    )
    .await;
    assert_eq!(with_origin.status, StatusCode::OK);
    let without = send(&server, request_to_mcp(&token).body(tools_list()).unwrap()).await;
    assert_eq!(without.status, StatusCode::OK);
}

/// With no session and nothing to push there is no stream to open or session to end, and
/// a client takes 405 to mean exactly that.
#[tokio::test]
async fn only_post_is_answered() {
    let (server, _, token) = server_and_token().await;
    for method in [Method::GET, Method::DELETE, Method::PUT] {
        let reply = send(
            &server,
            Request::builder()
                .method(method.clone())
                .uri(PATH)
                .header(header::HOST, HOST)
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", token.expose_secret()),
                )
                .header(header::ACCEPT, "application/json, text/event-stream")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(reply.status, StatusCode::METHOD_NOT_ALLOWED, "{method}");
        assert_eq!(reply.json()["code"], "method_not_allowed");
        assert_eq!(reply.headers[header::ALLOW], "POST");
    }
}

#[tokio::test]
async fn a_body_over_the_apis_limit_is_refused() {
    let (server, _, token) = server_and_token().await;
    let padding = "x".repeat(256 * 1024);
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {"p": padding}});
    let reply = send(
        &server,
        request_to_mcp(&token)
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(reply.status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn a_client_that_cant_read_both_answers_is_refused() {
    let (server, _, token) = server_and_token().await;
    let reply = send(
        &server,
        Request::builder()
            .method(Method::POST)
            .uri(PATH)
            .header(header::HOST, HOST)
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", token.expose_secret()),
            )
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "text/html")
            .body(tools_list())
            .unwrap(),
    )
    .await;
    assert_eq!(reply.status, StatusCode::NOT_ACCEPTABLE);
}

/// The protocol has had no batches since 2025-06-18.
#[tokio::test]
async fn a_batch_of_requests_is_refused() {
    let (server, _, token) = server_and_token().await;
    let call = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"});
    let reply = send(
        &server,
        request_to_mcp(&token)
            .body(Body::from(json!([call]).to_string()))
            .unwrap(),
    )
    .await;
    assert!(reply.status.is_client_error());
}

#[tokio::test]
async fn a_tool_that_doesnt_exist_is_a_protocol_error() {
    let (server, _, token) = server_and_token().await;
    let answer = rpc(
        &server,
        &token,
        "tools/call",
        json!({"name": "create_token", "arguments": {}}),
    )
    .await;
    assert_eq!(answer["error"]["code"], -32602);
}
