//! Identity service: bootstrap, invites, enrollment, re-enroll, login and
//! member management, driven with software passkeys.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

mod invites;
mod login;
mod members;

use coffer_domain::identity::{Actor, DisplayName, Role, TokenName, Username, Via};
use coffer_domain::ids::MemberId;
use coffer_server::testkit::{SoftAuthenticator, TestServer};
use coffer_store::{AuditEvent, AuditFilter, PageRequest};

fn session(member: MemberId, role: Role) -> Actor {
    Actor {
        member,
        role,
        via: Via::Session,
    }
}

fn username(name: &str) -> Username {
    Username::parse(name).unwrap()
}

fn display(name: &str) -> DisplayName {
    DisplayName::parse(name).unwrap()
}

fn label(name: &str) -> TokenName {
    TokenName::parse(name).unwrap()
}

/// A server whose first admin, "alice", has enrolled with the returned
/// authenticator.
async fn with_admin() -> (TestServer, SoftAuthenticator, MemberId) {
    let server = TestServer::new().await;
    let link = server.identity.bootstrap_link().await.unwrap().unwrap();
    let mut auth = server.authenticator();
    let alice = auth.enroll(&link, "alice", "Alice").await;
    (server, auth, alice)
}

/// Alice invites "bob" as a plain member; bob enrolls with the returned
/// authenticator.
async fn with_bob(server: &TestServer, alice: MemberId) -> (SoftAuthenticator, MemberId) {
    let link = server
        .identity
        .create_invite(&session(alice, Role::Admin), Role::Member, None)
        .await
        .unwrap();
    let mut auth = server.authenticator();
    let bob = auth.enroll(&link, "bob", "Bob").await;
    (auth, bob)
}

async fn audit(server: &TestServer) -> Vec<AuditEvent> {
    let mut events = server
        .store
        .list_audit(
            &AuditFilter::default(),
            &PageRequest {
                after: None,
                limit: 200,
            },
        )
        .await
        .unwrap()
        .items;
    events.reverse();
    events
}
