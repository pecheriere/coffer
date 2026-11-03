//! Making, reading and removing a link, and what a link can't join.

use axum::body::Body;
use axum::http::{Method, StatusCode};
use axum::http::{Request, header};
use coffer_domain::audit::AuditActor;
use coffer_domain::identity::{Scope, TokenName};
use coffer_domain::policy::AccountLevel::Write;
use coffer_server::http::router;
use coffer_store::{AuditFilter, PageRequest};
use http_body_util::BodyExt as _;
use secrecy::ExposeSecret as _;
use serde_json::{Value, json};
use tower::ServiceExt as _;

use super::{LINKS, TRANSACTIONS, World, linking, on, usd, world};

impl World {
    /// Asks for a link that must be refused with `status`; returns the problem's detail.
    async fn refused(&self, outflow: &str, inflow: &str, status: StatusCode) -> String {
        let (answer, body) = self
            .send(
                Method::POST,
                LINKS,
                &self.admin,
                Some(&linking(outflow, inflow)),
            )
            .await;
        assert_eq!(answer, status);
        body["detail"].as_str().unwrap().to_owned()
    }

    /// Links two transactions under one idempotency key; returns the status and the answer.
    async fn keyed(&self, outflow: &str, inflow: &str) -> (StatusCode, Vec<u8>) {
        let request = Request::builder()
            .method(Method::POST)
            .uri(LINKS)
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", self.admin.expose_secret()),
            )
            .header("idempotency-key", "the-one-link")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(linking(outflow, inflow).to_string()))
            .unwrap();
        let response = router(self.server.app_state())
            .oneshot(request)
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, bytes.to_vec())
    }
}

/// A link says what moved and between what, and each transaction then names the other.
#[tokio::test]
async fn a_link_joins_two_transactions() {
    let world = world().await;
    let (outflow, inflow) = world.pair().await;

    let link = world.link(&outflow, &inflow).await;

    assert_eq!(link["outflow"]["transaction"], json!(outflow));
    assert_eq!(link["outflow"]["account"], json!(world.checking));
    assert_eq!(link["outflow"]["amount"], usd(-50_000));
    assert_eq!(link["inflow"]["transaction"], json!(inflow));
    assert_eq!(link["inflow"]["account"], json!(world.savings));
    assert_eq!(link["inflow"]["amount"], usd(50_000));
    assert_eq!(link["difference"], usd(0));
    assert_eq!(link["one_movement"], true);

    let (status, read) = world
        .send(
            Method::GET,
            &format!("{LINKS}/{}", link["id"].as_str().unwrap()),
            &world.admin,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(read, link);

    let out = world.transaction(&outflow).await;
    assert_eq!(out["transfer_link"]["id"], link["id"]);
    assert_eq!(out["transfer_link"]["other_transaction"], json!(inflow));
    let into = world.transaction(&inflow).await;
    assert_eq!(into["transfer_link"]["other_transaction"], json!(outflow));
}

/// A link leaves each leg's category alone: filing it is the member's, and removing the link
/// has to bring back what was there.
#[tokio::test]
async fn a_link_keeps_each_legs_category() {
    let world = world().await;
    let (outflow, inflow) = world.pair().await;
    let food = world.category("Food", "expense").await;
    world
        .splits(
            &outflow,
            &json!([{"category": food, "minor_units": -50_000}]),
        )
        .await;

    world.link(&outflow, &inflow).await;

    let read = world.transaction(&outflow).await;
    assert_eq!(read["splits"][0]["category"], json!(food));
    assert_eq!(read["splits"][0]["amount"], usd(-50_000));
    assert_eq!(world.transaction(&inflow).await["splits"], json!([]));
}

/// A fee or a rate between the legs is reported, not hidden: less arrived than left.
#[tokio::test]
async fn amounts_that_differ_may_be_linked_and_say_so() {
    let world = world().await;
    let outflow = world.manual(world.checking, "2026-10-05", -50_000).await;
    let inflow = world.manual(world.savings, "2026-10-05", 49_900).await;

    let link = world.link(&outflow, &inflow).await;

    assert_eq!(link["difference"], usd(-100));
}

/// Two currencies have no difference to state without a rate, and each leg says its own.
#[tokio::test]
async fn currencies_that_differ_may_be_linked() {
    let world = world().await;
    let euros = world.account("Euros", "checking", "EUR").await;
    let outflow = world.manual(world.checking, "2026-10-05", -50_000).await;
    let inflow = world.manual(euros, "2026-10-05", 46_000).await;

    let link = world.link(&outflow, &inflow).await;

    assert_eq!(link["difference"], Value::Null);
    assert_eq!(link["inflow"]["amount"]["currency"], "EUR");
}

/// Crossing out of counted cash is recorded as such, and is allowed: spec §8.11 B9.
#[tokio::test]
async fn a_link_out_of_counted_cash_says_so() {
    let world = world().await;
    let fund = world.account("College fund", "education", "USD").await;
    let outflow = world.manual(world.checking, "2026-10-05", -50_000).await;
    let inflow = world.manual(fund, "2026-10-05", 50_000).await;

    let link = world.link(&outflow, &inflow).await;

    assert_eq!(link["one_movement"], false);
}

/// Two legs, on two accounts, one out and one in, and neither already in a link.
#[tokio::test]
async fn what_a_link_cant_join() {
    let world = world().await;
    let (outflow, inflow) = world.pair().await;
    let also_checking = world.manual(world.checking, "2026-10-06", 50_000).await;
    let also_out = world.manual(world.savings, "2026-10-06", -50_000).await;

    for (from, into) in [
        (&outflow, &outflow),
        (&outflow, &also_checking),
        (&inflow, &outflow),
        (&outflow, &also_out),
    ] {
        world
            .refused(from, into, StatusCode::UNPROCESSABLE_ENTITY)
            .await;
    }

    world.link(&outflow, &inflow).await;
    let other_in = world.manual(world.savings, "2026-10-07", 50_000).await;
    let other_out = world.manual(world.checking, "2026-10-07", -50_000).await;
    world
        .refused(&outflow, &other_in, StatusCode::CONFLICT)
        .await;
    world
        .refused(&other_out, &inflow, StatusCode::CONFLICT)
        .await;
}

/// Removing a link puts things back, and the two transactions can be linked again.
#[tokio::test]
async fn a_link_can_be_removed_and_made_again() {
    let world = world().await;
    let (outflow, inflow) = world.pair().await;
    let link = world.link(&outflow, &inflow).await;
    let uri = format!("{LINKS}/{}", link["id"].as_str().unwrap());

    let (status, _) = world.send(Method::DELETE, &uri, &world.admin, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    assert_eq!(
        world.transaction(&outflow).await["transfer_link"],
        Value::Null
    );
    let (status, _) = world.send(Method::GET, &uri, &world.admin, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = world.send(Method::DELETE, &uri, &world.admin, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    world.link(&outflow, &inflow).await;
}

/// Who linked and who removed is on the record, down to the agent's token.
#[tokio::test]
async fn links_are_audited_with_the_agent() {
    let world = world().await;
    let (outflow, inflow) = world.pair().await;
    let (token_record, token) = world
        .server
        .agent_tokens
        .create(
            &world.alice,
            TokenName::parse("agent").unwrap(),
            Scope::Write,
            None,
            vec![on(world.checking, Write), on(world.savings, Write)],
        )
        .await
        .unwrap();

    let (status, link) = world
        .send(
            Method::POST,
            LINKS,
            &token,
            Some(&linking(&outflow, &inflow)),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(link["created_by"]["agent"], json!(token_record.id));
    let id = link["id"].as_str().unwrap();
    let (status, _) = world
        .send(Method::DELETE, &format!("{LINKS}/{id}"), &token, None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let events = world
        .server
        .store
        .list_audit(&AuditFilter::default(), &PageRequest::default())
        .await
        .unwrap()
        .items;
    for action in ["transfer_link.create", "transfer_link.remove"] {
        let event = events.iter().find(|event| event.action == action).unwrap();
        assert_eq!(event.entity_type, "transfer_link");
        assert_eq!(event.entity_id, id);
        assert!(matches!(
            event.actor,
            AuditActor::Member { agent: Some(token), .. } if token == token_record.id
        ));
    }
}

/// A retry with the same key answers the first link, not a conflict with it.
#[tokio::test]
async fn a_retry_answers_the_first_link() {
    let world = world().await;
    let (outflow, inflow) = world.pair().await;
    let first = world.keyed(&outflow, &inflow).await;
    let second = world.keyed(&outflow, &inflow).await;
    assert_eq!(first.0, StatusCode::CREATED);
    assert_eq!(first, second);
}

/// A closed account's history can still be explained: the last sweep out of an account is
/// often what closed it.
#[tokio::test]
async fn a_closed_account_can_hold_a_leg() {
    let world = world().await;
    let (outflow, inflow) = world.pair().await;
    world
        .admin_send(
            Method::PATCH,
            &format!("/api/v1/accounts/{}", world.savings),
            &json!({"closed": true}),
        )
        .await;

    let link = world.link(&outflow, &inflow).await;

    assert_eq!(link["one_movement"], false, "closed isn't counted");
    let (status, _) = world
        .send(
            Method::DELETE,
            &format!("{LINKS}/{}", link["id"].as_str().unwrap()),
            &world.admin,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

/// A manual leg isn't deleted out from under its link: a person removes the link first.
#[tokio::test]
async fn a_linked_manual_transaction_cant_be_deleted() {
    let world = world().await;
    let (outflow, inflow) = world.pair().await;
    let link = world.link(&outflow, &inflow).await;
    let uri = format!("{TRANSACTIONS}/{outflow}");

    let (status, _) = world.send(Method::DELETE, &uri, &world.admin, None).await;
    assert_eq!(status, StatusCode::CONFLICT);

    world
        .send(
            Method::DELETE,
            &format!("{LINKS}/{}", link["id"].as_str().unwrap()),
            &world.admin,
            None,
        )
        .await;
    let (status, _) = world.send(Method::DELETE, &uri, &world.admin, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}
