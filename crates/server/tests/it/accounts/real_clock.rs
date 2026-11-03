//! On the real clock, which keeps finer than the store does, a new account answers with the
//! time a later read shows.

use axum::http::{Method, StatusCode};
use coffer_domain::identity::Scope;
use coffer_server::testkit::TestServer;
use serde_json::json;

use super::{call, create, token, world_on};

#[tokio::test]
async fn a_new_account_answers_with_the_time_a_later_read_shows() {
    let world = world_on(TestServer::with_system_clock().await).await;
    let (_, write) = token(&world, Scope::Write).await;

    let created = create(
        &world.server,
        &write,
        json!({"name": "Visa", "kind": "credit_card", "currency": "USD"}),
    )
    .await;

    let id = created["id"].as_str().unwrap();
    let (status, read_back) = call(
        &world.server,
        Method::GET,
        &format!("/api/v1/accounts/{id}"),
        &write,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(read_back["created_at"], created["created_at"]);
}
