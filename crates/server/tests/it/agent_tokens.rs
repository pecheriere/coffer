//! Agent tokens and request authentication, through the real router.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use axum::Router;
use axum::body::Body;
use axum::http::{Request, Response, StatusCode, header};
use axum::routing::get;
use coffer_domain::audit::AuditChanges;
use coffer_domain::identity::{Actor, Role, Scope, TokenName, Via};
use coffer_domain::ids::{AgentTokenId, MemberId};
use coffer_domain::policy::DenyReason;
use coffer_server::auth::{AuthError, COOKIE_NAME};
use coffer_server::http::extract::PageActor;
use coffer_server::http::router;
use coffer_server::secrets::{SecretToken, TokenKind};
use coffer_server::testkit::{TestServer, event_signal};
use coffer_store::AgentTokenRecord;
use http_body_util::BodyExt as _;
use jiff::SignedDuration;
use secrecy::ExposeSecret as _;
use serde_json::Value;
use tower::ServiceExt as _;

struct World {
    server: TestServer,
    alice: Actor,
    bob: MemberId,
    bob_cookie: SecretToken,
}

/// Admin "alice" and member "bob", both signed in.
async fn world() -> World {
    let server = TestServer::new().await;
    let link = server.identity.bootstrap_link().await.unwrap().unwrap();
    let mut alice_auth = server.authenticator();
    let alice_id = alice_auth.enroll(&link, "alice", "Alice").await;
    let alice = Actor {
        member: alice_id,
        role: Role::Admin,
        via: Via::Session,
    };
    let invite = server
        .identity
        .create_invite(&alice, Role::Member, None)
        .await
        .unwrap();
    let mut bob_auth = server.authenticator();
    bob_auth.enroll(&invite, "bob", "Bob").await;
    let (bob, bob_cookie) = bob_auth.login_with_session("bob").await;
    World {
        server,
        alice,
        bob,
        bob_cookie,
    }
}

fn name(label: &str) -> TokenName {
    TokenName::parse(label).unwrap()
}

async fn mint(
    world: &World,
    actor: &Actor,
    scope: Scope,
    expires_in: Option<SignedDuration>,
) -> (AgentTokenRecord, SecretToken) {
    let expires_at = expires_in.map(|by| {
        use coffer_domain::time::Clock as _;
        world.server.clock.now().checked_add(by).unwrap()
    });
    world
        .server
        .agent_tokens
        .create(actor, name("claude"), scope, expires_at, Vec::new())
        .await
        .unwrap()
}

fn bob_actor(world: &World) -> Actor {
    Actor {
        member: world.bob,
        role: Role::Member,
        via: Via::Session,
    }
}

async fn get_me(server: &TestServer, authorization: Option<&str>) -> Response<Body> {
    let mut request = Request::get("/api/v1/me");
    if let Some(value) = authorization {
        request = request.header(header::AUTHORIZATION, value);
    }
    router(server.app_state())
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn body_text(response: Response<Body>) -> String {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn bearer(token: &SecretToken) -> String {
    format!("Bearer {}", token.expose_secret())
}

#[tokio::test]
async fn token_shown_once_and_stored_hashed() {
    let world = world().await;
    let (record, token) = mint(&world, &world.alice, Scope::Read, None).await;

    let listed = world
        .server
        .agent_tokens
        .list_own(&world.alice)
        .await
        .unwrap();

    assert_eq!(listed, vec![record.clone()]);
    assert!(
        record.prefix == token.expose_secret()[..12],
        "the stored prefix must be the first twelve characters"
    );
    assert!(!format!("{listed:?}").contains(token.expose_secret()));
    // The store finds the token by its keyed hash, never by its bytes.
    let hash = world
        .server
        .key
        .hash(TokenKind::Agent, token.expose_secret());
    assert!(
        hash.as_slice() != token.expose_secret().as_bytes(),
        "the stored hash must not be the token"
    );
    let now = coffer_domain::time::Clock::now(&*world.server.clock);
    assert_eq!(
        world
            .server
            .store
            .find_live_agent_token(&hash, now)
            .await
            .unwrap()
            .id,
        record.id
    );
}

#[tokio::test]
async fn me_describes_the_member_and_token() {
    let world = world().await;
    let (record, token) = mint(&world, &world.alice, Scope::Write, None).await;

    let response = get_me(&world.server, Some(&bearer(&token))).await;

    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = serde_json::from_str(&body_text(response).await).unwrap();
    assert_eq!(body["member"]["username"], "alice");
    assert_eq!(body["member"]["display_name"], "Alice");
    assert_eq!(body["member"]["role"], "admin");
    assert_eq!(body["member"]["id"], world.alice.member.to_string());
    assert_eq!(body["agent"]["id"], record.id.to_string());
    assert_eq!(body["agent"]["name"], "claude");
    assert_eq!(body["agent"]["scope"], "write");
}

#[tokio::test]
async fn missing_or_malformed_authorization_is_401() {
    let world = world().await;
    let (_, token) = mint(&world, &world.alice, Scope::Read, None).await;

    for header_value in [
        None,
        Some(String::new()),
        Some("Bearer".to_owned()),
        Some("Bearer ".to_owned()),
        Some(format!("Basic {}", token.expose_secret())),
        Some(token.expose_secret().to_owned()),
    ] {
        let response = get_me(&world.server, header_value.as_deref()).await;
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{header_value:?}"
        );
        assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer");
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/problem+json"
        );
        let body: Value = serde_json::from_str(&body_text(response).await).unwrap();
        assert_eq!(body["code"], "unauthenticated");
    }
}

#[tokio::test]
async fn revoked_token_is_401() {
    let world = world().await;
    let (record, token) = mint(&world, &world.alice, Scope::Read, None).await;
    assert_eq!(
        get_me(&world.server, Some(&bearer(&token))).await.status(),
        StatusCode::OK
    );

    world
        .server
        .agent_tokens
        .revoke(&world.alice, record.id)
        .await
        .unwrap();

    assert_eq!(
        get_me(&world.server, Some(&bearer(&token))).await.status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn expired_token_is_401() {
    let world = world().await;
    let (_, token) = mint(
        &world,
        &world.alice,
        Scope::Read,
        Some(SignedDuration::from_hours(1)),
    )
    .await;
    world.server.clock.advance(SignedDuration::from_mins(59));
    assert_eq!(
        get_me(&world.server, Some(&bearer(&token))).await.status(),
        StatusCode::OK
    );

    world.server.clock.advance(SignedDuration::from_mins(1));

    assert_eq!(
        get_me(&world.server, Some(&bearer(&token))).await.status(),
        StatusCode::UNAUTHORIZED
    );
}

/// A router with one cookie-authenticated route, standing in for a page.
fn page_router(server: &TestServer) -> Router {
    Router::new()
        .route(
            "/page",
            get(|PageActor(actor): PageActor| async move { actor.member.to_string() }),
        )
        .with_state(server.app_state())
}

async fn get_page(server: &TestServer, headers: &[(&str, String)]) -> StatusCode {
    let mut request = Request::get("/page");
    for (key, value) in headers {
        request = request.header(*key, value);
    }
    page_router(server)
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn disabled_member_loses_session_and_tokens() {
    let world = world().await;
    let (_, bob_token) = mint(&world, &bob_actor(&world), Scope::Read, None).await;
    let cookie = format!("{COOKIE_NAME}={}", world.bob_cookie.expose_secret());
    assert_eq!(
        get_page(&world.server, &[("cookie", cookie.clone())]).await,
        StatusCode::OK
    );
    assert_eq!(
        get_me(&world.server, Some(&bearer(&bob_token)))
            .await
            .status(),
        StatusCode::OK
    );

    world
        .server
        .identity
        .disable_member(&world.alice, world.bob)
        .await
        .unwrap();

    assert_eq!(
        get_page(&world.server, &[("cookie", cookie)]).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        get_me(&world.server, Some(&bearer(&bob_token)))
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn page_route_rejects_bearer_and_accepts_cookie_among_others() {
    let world = world().await;
    let (_, token) = mint(&world, &world.alice, Scope::Admin, None).await;

    let with_bearer = get_page(&world.server, &[("authorization", bearer(&token))]).await;
    let with_cookie = get_page(
        &world.server,
        &[(
            "cookie",
            format!(
                "theme=dark; {COOKIE_NAME}={}; x=y",
                world.bob_cookie.expose_secret()
            ),
        )],
    )
    .await;

    assert_eq!(with_bearer, StatusCode::UNAUTHORIZED);
    assert_eq!(with_cookie, StatusCode::OK);
}

#[tokio::test]
async fn agent_cannot_create_tokens() {
    let world = world().await;
    let (record, token) = mint(&world, &world.alice, Scope::Admin, None).await;
    let agent = world
        .server
        .agent_tokens
        .resolve(token.expose_secret())
        .await
        .unwrap();
    assert_eq!(
        agent.via,
        Via::Agent {
            token: record.id,
            scope: Scope::Admin
        }
    );

    let response = router(world.server.app_state())
        .oneshot(
            Request::post("/api/v1/tokens")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let err = world
        .server
        .agent_tokens
        .create(&agent, name("child"), Scope::Read, None, Vec::new())
        .await
        .unwrap_err();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(matches!(
        err,
        AuthError::Denied(denied) if denied.reason == DenyReason::SessionRequired
    ));
}

#[tokio::test]
async fn member_cannot_mint_admin_token() {
    let world = world().await;

    let err = world
        .server
        .agent_tokens
        .create(
            &bob_actor(&world),
            name("too much"),
            Scope::Admin,
            None,
            Vec::new(),
        )
        .await
        .unwrap_err();

    assert!(matches!(
        err,
        AuthError::Denied(denied) if denied.reason == DenyReason::ScopeAboveRole
    ));
    assert_eq!(err.to_string(), "requested scope exceeds your role");
    let own = world
        .server
        .agent_tokens
        .list_own(&bob_actor(&world))
        .await
        .unwrap();
    assert_eq!(own.len(), 0);
}

#[tokio::test]
async fn bad_bearer_not_echoed() {
    let world = world().await;

    let response = get_me(
        &world.server,
        Some("Bearer coffer_SECRETVALUEabcdefghijklmnopqrstuvwxyz0123456"),
    )
    .await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let headers = format!("{:?}", response.headers());
    let body = body_text(response).await;
    assert!(!body.contains("SECRETVALUE"));
    assert!(!headers.contains("SECRETVALUE"));
}

#[tokio::test]
async fn role_change_applies_to_existing_token_at_once() {
    let world = world().await;
    world
        .server
        .identity
        .set_role(&world.alice, world.bob, Role::Admin)
        .await
        .unwrap();
    let promoted = Actor {
        role: Role::Admin,
        ..bob_actor(&world)
    };
    let (_, token) = mint(&world, &promoted, Scope::Admin, None).await;
    let before = world
        .server
        .agent_tokens
        .resolve(token.expose_secret())
        .await
        .unwrap();

    world
        .server
        .identity
        .set_role(&world.alice, world.bob, Role::Member)
        .await
        .unwrap();
    let after = world
        .server
        .agent_tokens
        .resolve(token.expose_secret())
        .await
        .unwrap();

    assert_eq!(before.effective_scope(), Scope::Admin);
    assert_eq!(after.role, Role::Member);
    assert_eq!(after.effective_scope(), Scope::Write);
}

#[tokio::test]
async fn revoking_others_tokens_needs_admin() {
    let world = world().await;
    let (alice_record, _) = mint(&world, &world.alice, Scope::Read, None).await;
    let (bob_record, _) = mint(&world, &bob_actor(&world), Scope::Read, None).await;

    let denied = world
        .server
        .agent_tokens
        .revoke(&bob_actor(&world), alice_record.id)
        .await;
    world
        .server
        .agent_tokens
        .revoke(&world.alice, bob_record.id)
        .await
        .unwrap();

    assert!(matches!(denied, Err(AuthError::NotFound)));
    let unknown = world
        .server
        .agent_tokens
        .revoke(&bob_actor(&world), AgentTokenId::new())
        .await;
    assert!(matches!(unknown, Err(AuthError::NotFound)));
    let all = world
        .server
        .agent_tokens
        .list_all(&world.alice)
        .await
        .unwrap();
    assert_eq!(all.len(), 2);
    assert!(
        all.iter()
            .find(|t| t.id == bob_record.id)
            .unwrap()
            .revoked_at
            .is_some()
    );
    assert!(
        all.iter()
            .find(|t| t.id == alice_record.id)
            .unwrap()
            .revoked_at
            .is_none()
    );
    assert!(matches!(
        world.server.agent_tokens.list_all(&bob_actor(&world)).await,
        Err(AuthError::Denied(_))
    ));
}

#[tokio::test]
async fn last_used_is_recorded_at_most_once_a_minute() {
    let world = world().await;
    let (_, token) = mint(&world, &world.alice, Scope::Read, None).await;
    let used = |world: &World| {
        let tokens = world.server.agent_tokens.clone();
        let alice = world.alice;
        async move { tokens.list_own(&alice).await.unwrap()[0].last_used_at }
    };
    assert_eq!(used(&world).await, None);

    world
        .server
        .agent_tokens
        .resolve(token.expose_secret())
        .await
        .unwrap();
    let first = used(&world).await.unwrap();
    world.server.clock.advance(SignedDuration::from_secs(30));
    world
        .server
        .agent_tokens
        .resolve(token.expose_secret())
        .await
        .unwrap();
    let within_minute = used(&world).await.unwrap();
    world.server.clock.advance(SignedDuration::from_secs(30));
    world
        .server
        .agent_tokens
        .resolve(token.expose_secret())
        .await
        .unwrap();
    let after_minute = used(&world).await.unwrap();

    assert_eq!(within_minute, first);
    assert_eq!(
        after_minute,
        first.checked_add(SignedDuration::from_mins(1)).unwrap()
    );
}

#[tokio::test]
async fn token_lifecycle_is_audited_without_secrets() {
    use coffer_store::{AuditFilter, PageRequest};
    let world = world().await;
    let (record, token) = mint(
        &world,
        &world.alice,
        Scope::Write,
        Some(SignedDuration::from_hours(1)),
    )
    .await;
    world
        .server
        .agent_tokens
        .revoke(&world.alice, record.id)
        .await
        .unwrap();

    let events = world
        .server
        .store
        .list_audit(&AuditFilter::default(), &PageRequest::default())
        .await
        .unwrap()
        .items;

    let actions: Vec<_> = events.iter().map(|e| e.action.as_str()).collect();
    assert!(actions.contains(&"token.create"));
    assert!(actions.contains(&"token.revoke"));
    let create = events.iter().find(|e| e.action == "token.create").unwrap();
    assert!(record.expires_at.is_some());
    let recorded = AuditChanges::new()
        .set("member", record.member)
        .set("name", "claude")
        .set("scope", "write")
        .set("expires_at", record.expires_at);
    assert_eq!(create.changes, recorded);
    let revoke = events.iter().find(|e| e.action == "token.revoke").unwrap();
    assert_eq!(revoke.changes, recorded);
    let dump = format!("{events:?}");
    assert!(!dump.contains(token.expose_secret()));
}

#[tokio::test]
async fn agents_cannot_list_or_revoke_tokens() {
    let world = world().await;
    let (own, token) = mint(&world, &world.alice, Scope::Admin, None).await;
    let (other, _) = mint(&world, &bob_actor(&world), Scope::Read, None).await;
    let agent = world
        .server
        .agent_tokens
        .resolve(token.expose_secret())
        .await
        .unwrap();
    let tokens = &world.server.agent_tokens;

    let results = [
        tokens.list_own(&agent).await.map(drop),
        tokens.list_all(&agent).await.map(drop),
        tokens.revoke(&agent, own.id).await,
        tokens.revoke(&agent, other.id).await,
    ];

    for result in results {
        assert!(matches!(
            result,
            Err(AuthError::Denied(denied)) if denied.reason == DenyReason::SessionRequired
        ));
    }
    assert!(
        tokens
            .list_all(&world.alice)
            .await
            .unwrap()
            .iter()
            .all(|t| t.revoked_at.is_none())
    );
}

#[tokio::test]
async fn failed_last_used_update_does_not_fail_the_request() {
    let world = world().await;
    let (_, token) = mint(&world, &world.alice, Scope::Read, None).await;
    // Hold the write lock so recording the use fails once the store gives up.
    let blocker = world.server.store.begin().await.unwrap();

    let actor = world
        .server
        .agent_tokens
        .resolve(token.expose_secret())
        .await
        .unwrap();
    drop(blocker);

    assert_eq!(actor.member, world.alice.member);
    assert_eq!(
        world
            .server
            .agent_tokens
            .list_own(&world.alice)
            .await
            .unwrap()[0]
            .last_used_at,
        None
    );
}

#[tokio::test]
async fn token_revoked_while_its_use_is_recorded_is_refused() {
    let world = world().await;
    let (record, secret) = mint(&world, &bob_actor(&world), Scope::Read, None).await;
    // Hold the write lock so the request, once it has found the token live,
    // waits right where it records use; revoke the token then.
    let mut blocker = world.server.store.begin().await.unwrap();
    let tokens = std::sync::Arc::clone(&world.server.agent_tokens);
    let bearer = secret.expose_secret().to_owned();
    let recording = event_signal("recording token use", "token", record.id);
    let request = tokio::spawn(async move { tokens.resolve(&bearer).await });
    tokio::time::timeout(std::time::Duration::from_secs(5), recording.notified())
        .await
        .unwrap();

    blocker
        .revoke_agent_token(
            record.id,
            coffer_domain::time::Clock::now(&*world.server.clock),
        )
        .await
        .unwrap();
    blocker.commit().await.unwrap();

    let err = request.await.unwrap().unwrap_err();
    assert!(matches!(err, AuthError::Unauthenticated), "{err:?}");
}
