//! `authorize_create_account`: what a page asks before it reads a bind form, directly, because a
//! page's session can't be a read token.

use super::{reported, world};
use coffer_domain::identity::{Actor, Role, Scope, Via};
use coffer_domain::ids::{AgentTokenId, ProviderAccountId};
use coffer_server::sync::SyncError;

#[tokio::test]
async fn a_read_token_is_refused() {
    let world = world().await;
    world
        .sync_once(vec![reported("acct-1", "USD", None, Vec::new())])
        .await;
    let held = world.provider_account("acct-1").await;
    let reader = Actor {
        member: world.alice.member,
        role: Role::Admin,
        via: Via::Agent {
            token: AgentTokenId::new(),
            scope: Scope::Read,
        },
    };
    let answer = world
        .server
        .sync
        .authorize_create_account(&reader, held.id)
        .await;
    assert!(matches!(answer, Err(SyncError::Denied(_))));
}

#[tokio::test]
async fn a_member_who_may_bind_is_let_through() {
    let world = world().await;
    world
        .sync_once(vec![reported("acct-1", "USD", None, Vec::new())])
        .await;
    let held = world.provider_account("acct-1").await;
    let answer = world
        .server
        .sync
        .authorize_create_account(&world.alice, held.id)
        .await;
    assert!(answer.is_ok());
}

#[tokio::test]
async fn an_unknown_row_is_missing() {
    let world = world().await;
    let answer = world
        .server
        .sync
        .authorize_create_account(&world.alice, ProviderAccountId::new())
        .await;
    assert!(matches!(answer, Err(SyncError::NotFound)));
}
