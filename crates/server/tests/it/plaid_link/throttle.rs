//! How often a member's pages may call Plaid: 20 requests per member per ten
//! minutes, shared by the two Link pages and the public token exchange.

use super::{
    CREDENTIALS, DEFAULT_CSP, EXCHANGE_URL, LINK_MARKER, LINK_SCRIPT, LINK_TOKEN_CREATE, NEW_PAGE,
    PUBLIC_MARKER, PUBLIC_TOKEN, World, csp, reconnect_path, world,
};
use axum::http::{StatusCode, header};
use coffer_domain::accounts::ConnectionLabel;
use coffer_domain::identity::{Actor, Role, Scope, TokenName, Via};
use coffer_domain::ids::ConnectionId;
use coffer_domain::time::Clock as _;
use coffer_server::sync::SyncError;
use coffer_server::sync::connections::ConnectionView;
use coffer_server::sync::provider::{ConnectInput, ProviderKind};
use coffer_server::testkit::TEST_PUBLIC_URL;
use coffer_server::testkit::browser::{Browser, Reply};
use coffer_store::{ConnectionStatus, ConnectionStatusChange};
use jiff::SignedDuration;
use secrecy::SecretString;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::task::JoinSet;

/// Repeated here so that changing the limit also takes changing this test.
const LIMIT: usize = 20;
const WINDOW: SignedDuration = SignedDuration::from_mins(10);
const WINDOW_SECONDS: &str = "600";
const REFUSED_DETAIL: &str = "Too many requests to Plaid. Wait a few minutes.";
const LABEL_MARKER: &str = "LABEL-MARKER-75";

fn exchange_body() -> Value {
    json!({ "label": LABEL_MARKER, "public_token": PUBLIC_TOKEN })
}

/// What `reply` must look like for a Link page refused by the limit.
fn assert_refused_page(reply: &Reply, retry_after: &str) {
    assert_eq!(reply.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(reply.header(header::RETRY_AFTER), retry_after);
    assert_eq!(
        csp(reply),
        DEFAULT_CSP,
        "a refused page has the default policy"
    );
    assert!(
        reply.header(header::CONTENT_TYPE).starts_with("text/html"),
        "a refused page is a page"
    );
    assert!(reply.body.contains(REFUSED_DETAIL), "it says what happened");
    assert!(!reply.body.contains(LINK_SCRIPT), "no Plaid script");
    assert!(!reply.body.contains("data-link-token"), "no link token");
    assert!(!reply.body.contains("nonce"), "no nonce");
    assert!(!reply.body.contains("src/"), "no source paths");
    assert!(!reply.body.contains("panicked"), "no stack trace");
}

/// What `reply` must look like for a token exchange refused by the limit.
fn assert_refused_exchange(reply: &Reply, retry_after: &str) {
    assert_eq!(reply.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(reply.header(header::RETRY_AFTER), retry_after);
    assert_eq!(csp(reply), DEFAULT_CSP);
    let body = reply.json();
    assert_eq!(body["code"], "busy");
    assert_eq!(body["detail"], REFUSED_DETAIL);
    assert!(
        !reply.body.contains(PUBLIC_MARKER),
        "the token is not echoed"
    );
    assert!(
        !reply.body.contains(LABEL_MARKER),
        "the label is not echoed"
    );
}

impl World {
    fn plaid_calls(&self) -> usize {
        self.mock.requests().len()
    }

    /// Spends the admin's whole budget on the new Link page, every call
    /// answered.
    async fn spend_budget(&mut self) {
        for _ in 0..LIMIT {
            self.script.link_token();
            let page = self.admin.get(NEW_PAGE).await;
            assert_eq!(page.status, StatusCode::OK, "within the limit");
        }
    }

    async fn removed_connection(&self) -> ConnectionId {
        let id = self.seeded().await;
        let mut tx = self.server.store.begin().await.unwrap();
        tx.set_connection_credentials(id, None).await.unwrap();
        tx.update_connection_status(
            id,
            ConnectionStatusChange {
                status: ConnectionStatus::Removed,
                last_error: None,
                attempted_at: None,
                succeeded_at: None,
                at: self.server.clock.now(),
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        id
    }

    /// Asks the service to connect `kind` with a `SimpleFin` setup token.
    async fn setup_token_attempt(&self, kind: ProviderKind) -> Result<ConnectionView, SyncError> {
        self.server
            .sync
            .create_connection(
                &self.alice,
                kind,
                ConnectionLabel::parse("Bank").unwrap(),
                ConnectInput::SimpleFinSetupToken(SecretString::from("not-a-token")),
                None,
            )
            .await
    }

    /// The admin, through an agent token below write on every account.
    async fn narrow_actor(&self) -> Actor {
        let (record, _) = self
            .server
            .agent_tokens
            .create(
                &self.alice,
                TokenName::parse("narrow").unwrap(),
                Scope::Read,
                None,
                Vec::new(),
            )
            .await
            .unwrap();
        Actor {
            member: self.alice.member,
            role: Role::Admin,
            via: Via::Agent {
                token: record.id,
                scope: Scope::Read,
            },
        }
    }
}

#[tokio::test]
async fn requests_within_the_limit_reach_plaid() {
    let mut world = world().await;

    for _ in 0..LIMIT {
        world.script.link_token();
        let page = world.admin.get(NEW_PAGE).await;
        assert_eq!(page.status, StatusCode::OK);
        assert!(page.body.contains(LINK_MARKER), "the page has its token");
        assert!(!page.headers.contains_key(header::RETRY_AFTER));
    }

    assert_eq!(world.script.seen(LINK_TOKEN_CREATE).len(), LIMIT);
}

#[tokio::test]
async fn past_the_limit_no_route_calls_plaid() {
    let mut world = world().await;
    let id = world.needing_login().await;
    world.spend_budget().await;
    let calls = world.plaid_calls();
    assert_eq!(calls, LIMIT, "one call to Plaid for each page");

    for _ in 0..3 {
        let new = world.admin.get(NEW_PAGE).await;
        assert_refused_page(&new, WINDOW_SECONDS);
        let reconnect = world.admin.get(&reconnect_path(id)).await;
        assert_refused_page(&reconnect, WINDOW_SECONDS);
        let exchange = world.exchange(&exchange_body()).await;
        assert_refused_exchange(&exchange, WINDOW_SECONDS);
    }

    assert_eq!(world.plaid_calls(), calls, "nothing more reached Plaid");
    assert_eq!(
        world.server.store.list_connections().await.unwrap().len(),
        1,
        "a refused exchange stores nothing"
    );
}

#[tokio::test]
async fn the_three_routes_share_one_budget() {
    let mut world = world().await;
    let id = world.needing_login().await;
    for used in 0..LIMIT {
        world.script.link_token();
        let reply = match used % 3 {
            0 => world.admin.get(NEW_PAGE).await,
            1 => world.admin.get(&reconnect_path(id)).await,
            _ => world.exchange(&exchange_body()).await,
        };
        assert_ne!(
            reply.status,
            StatusCode::TOO_MANY_REQUESTS,
            "within the limit"
        );
    }
    let calls = world.plaid_calls();
    assert_eq!(calls, LIMIT);

    assert_refused_page(&world.admin.get(NEW_PAGE).await, WINDOW_SECONDS);
    assert_refused_page(&world.admin.get(&reconnect_path(id)).await, WINDOW_SECONDS);
    assert_refused_exchange(&world.exchange(&exchange_body()).await, WINDOW_SECONDS);
    assert_eq!(world.plaid_calls(), calls);
}

#[tokio::test]
async fn failed_plaid_calls_count() {
    let mut world = world().await;

    // Nothing is scripted: Plaid answers every call with an error.
    for _ in 0..LIMIT {
        let page = world.admin.get(NEW_PAGE).await;
        assert_eq!(page.status, StatusCode::BAD_GATEWAY);
        assert!(!page.headers.contains_key(header::RETRY_AFTER));
    }
    let calls = world.plaid_calls();
    assert_eq!(calls, LIMIT);

    assert_refused_page(&world.admin.get(NEW_PAGE).await, WINDOW_SECONDS);
    assert_eq!(world.plaid_calls(), calls);
}

#[tokio::test]
async fn the_window_expires() {
    let mut world = world().await;
    world.spend_budget().await;

    world
        .server
        .clock
        .advance(WINDOW - SignedDuration::from_secs(1));
    assert_refused_page(&world.admin.get(NEW_PAGE).await, "1");
    assert_eq!(world.plaid_calls(), LIMIT);

    world.server.clock.advance(SignedDuration::from_secs(1));
    world.script.link_token();
    let page = world.admin.get(NEW_PAGE).await;
    assert_eq!(page.status, StatusCode::OK);
    assert_eq!(world.plaid_calls(), LIMIT + 1);
}

#[tokio::test]
async fn two_members_do_not_share_a_budget() {
    let mut world = world().await;
    let mut bob = world.bob().await;
    world.spend_budget().await;
    assert_refused_page(&world.admin.get(NEW_PAGE).await, WINDOW_SECONDS);

    for _ in 0..LIMIT {
        world.script.link_token();
        assert_eq!(bob.get(NEW_PAGE).await.status, StatusCode::OK);
    }
    assert_eq!(world.plaid_calls(), LIMIT * 2);
    assert_refused_page(&bob.get(NEW_PAGE).await, WINDOW_SECONDS);
    assert_refused_page(&world.admin.get(NEW_PAGE).await, WINDOW_SECONDS);
    assert_eq!(world.plaid_calls(), LIMIT * 2);
}

/// Bob may not reconnect Alice's connection, however often he asks, and asking
/// costs him nothing.
#[tokio::test]
async fn a_refused_member_gets_the_same_answer_and_spends_nothing() {
    let world = world().await;
    let id = world.needing_login().await;
    let mut bob = world.bob().await;

    let first = bob.get(&reconnect_path(id)).await;
    assert_eq!(first.status, StatusCode::FORBIDDEN);
    assert_eq!(csp(&first), DEFAULT_CSP);
    for _ in 0..LIMIT * 2 {
        let again = bob.get(&reconnect_path(id)).await;
        assert_eq!(again.status, StatusCode::FORBIDDEN);
        assert_eq!(again.body, first.body, "the same answer every time");
        assert!(!again.headers.contains_key(header::RETRY_AFTER));
    }
    assert_eq!(world.plaid_calls(), 0, "no refusal reaches Plaid");

    for _ in 0..LIMIT {
        world.script.link_token();
        assert_eq!(bob.get(NEW_PAGE).await.status, StatusCode::OK);
    }

    // With his budget spent, the refusal is still the same one.
    let spent = bob.get(&reconnect_path(id)).await;
    assert_eq!(spent.status, StatusCode::FORBIDDEN);
    assert_eq!(spent.body, first.body, "authorization comes first");
    assert!(!spent.headers.contains_key(header::RETRY_AFTER));
}

#[tokio::test]
async fn denied_service_calls_are_denied_before_the_budget_and_spend_nothing() {
    let mut world = world().await;
    let id = world.needing_login().await;
    let narrow = world.narrow_actor().await;
    let sync = Arc::clone(&world.server.sync);

    for _ in 0..LIMIT * 2 {
        assert!(matches!(
            sync.new_link_token(&narrow).await,
            Err(SyncError::Denied(_))
        ));
        assert!(matches!(
            sync.update_link_token(&narrow, id).await,
            Err(SyncError::Denied(_))
        ));
    }
    assert_eq!(world.plaid_calls(), 0);

    world.spend_budget().await;
    assert!(
        matches!(
            sync.new_link_token(&narrow).await,
            Err(SyncError::Denied(_))
        ),
        "a denial stays a denial with the budget spent"
    );
}

/// Every answer that needs no call to Plaid stays what it was once the budget
/// is spent, and costs nothing before it is.
#[tokio::test]
async fn requests_that_never_call_plaid_are_not_limited_and_spend_nothing() {
    let mut world = world().await;
    let id = world.needing_login().await;
    let removed = world.removed_connection().await;
    let simplefin = world
        .server
        .seed_connection(
            ProviderKind::SimpleFin,
            "Other bank",
            world.alice.member,
            CREDENTIALS,
        )
        .await;
    let unknown = ConnectionId::new();

    for round in 0..=LIMIT {
        if round == LIMIT {
            world.spend_budget().await;
        }
        for (path, status) in [
            (reconnect_path(unknown), StatusCode::NOT_FOUND),
            (reconnect_path(simplefin), StatusCode::NOT_FOUND),
            (
                "/connections/not-an-id/plaid/reconnect".to_owned(),
                StatusCode::NOT_FOUND,
            ),
            (reconnect_path(removed), StatusCode::CONFLICT),
        ] {
            assert_eq!(world.admin.get(&path).await.status, status);
        }
        for body in [
            json!({ "label": "", "public_token": PUBLIC_TOKEN }),
            json!({ "label": LABEL_MARKER, "public_token": " " }),
            json!({ "label": LABEL_MARKER }),
        ] {
            let reply = world.exchange(&body).await;
            assert_ne!(reply.status, StatusCode::TOO_MANY_REQUESTS);
            assert!(reply.status.is_client_error());
        }
        let forged = world
            .admin
            .post_json_with(
                EXCHANGE_URL,
                Some(TEST_PUBLIC_URL),
                Some("wrong"),
                &exchange_body(),
            )
            .await;
        assert_eq!(forged.status, StatusCode::FORBIDDEN);
    }
    assert_eq!(world.plaid_calls(), LIMIT, "only the budget reached Plaid");

    // Finishing update mode asks nothing of Plaid either.
    let confirmed = world.confirm(id).await;
    assert_eq!(confirmed.status, StatusCode::OK);
    let mut anonymous = Browser::new(&world.server);
    let login = anonymous.get(NEW_PAGE).await;
    assert_eq!(
        login.status,
        StatusCode::SEE_OTHER,
        "signed out goes to login"
    );
}

/// `SimpleFin` shares `create_connection` with the exchange, and agents reach it.
#[tokio::test]
async fn other_inputs_to_create_connection_are_not_limited() {
    let mut world = world().await;

    for _ in 0..LIMIT * 2 {
        let setup = world.setup_token_attempt(ProviderKind::SimpleFin).await;
        assert!(!matches!(setup, Err(SyncError::PlaidBudgetSpent { .. })));
        let wrong = world.setup_token_attempt(ProviderKind::Plaid).await;
        assert!(wrong.is_err(), "Plaid refuses a setup token by itself");
    }
    assert_eq!(world.plaid_calls(), 0);

    world.spend_budget().await;
    for kind in [ProviderKind::SimpleFin, ProviderKind::Plaid] {
        let result = world.setup_token_attempt(kind).await;
        assert!(!matches!(result, Err(SyncError::PlaidBudgetSpent { .. })));
    }
}

/// Spending happens before the call, so requests in flight together cannot
/// overrun the limit.
#[tokio::test]
async fn concurrent_requests_cannot_overrun_the_limit() {
    let world = world().await;
    let extra = 10;
    let mut requests = JoinSet::new();
    for _ in 0..LIMIT + extra {
        let sync = Arc::clone(&world.server.sync);
        let actor = world.alice;
        requests.spawn(async move { sync.new_link_token(&actor).await });
    }

    let mut refused = 0;
    while let Some(done) = requests.join_next().await {
        if matches!(done.unwrap(), Err(SyncError::PlaidBudgetSpent { .. })) {
            refused += 1;
        }
    }

    assert_eq!(refused, extra);
    assert_eq!(world.plaid_calls(), LIMIT);
}
