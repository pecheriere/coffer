//! Possible duplicates: merging either way, discarding what the one deleted holds, saying one
//! isn't a duplicate, and moving a merged record back out. Imported transactions come from the
//! sync's apply step, on a connection bound with the store's primitives.

use axum::http::StatusCode;
use coffer_domain::accounts::ConnectionLabel;
use coffer_domain::ids::{ConnectionId, TransactionId};
use coffer_domain::time::Clock as _;
use coffer_domain::transactions::TransactionFlag;
use coffer_server::sync::apply::{
    AccountBatch, Coverage, NormalizedTransaction, apply_account_batch,
};
use coffer_store::{AccountRecord, NewConnection, ProviderAccountRecord, ProviderAccountUpsert};
use jiff::civil::date;
use serde_json::json;

use super::{World, account, assert_redirect, assert_refused, detail, member, signed_in, world};

/// A connection whose provider account feeds "Checking".
struct Feed {
    connection: ConnectionId,
    provider_account: ProviderAccountRecord,
    account: AccountRecord,
}

async fn feed(world: &World) -> Feed {
    let id = account(&world.server, &world.alice, "Checking", "USD").await;
    let account = world.server.store.get_account(id).await.unwrap();
    let now = world.server.clock.now();
    let mut tx = world.server.store.begin().await.unwrap();
    let connection = tx
        .insert_connection(NewConnection {
            provider: "testbank",
            label: ConnectionLabel::parse("Bank").unwrap(),
            owner: world.alice.member,
            credentials: vec![1, 2, 3],
            created_at: now,
        })
        .await
        .unwrap();
    let provider_account = tx
        .upsert_provider_account(ProviderAccountUpsert {
            connection: connection.id,
            provider_account_id: "acct-1".into(),
            name: "Everyday".into(),
            institution: None,
            currency_raw: "USD".into(),
            kind_hint: None,
            balance: None,
            seen_at: now,
        })
        .await
        .unwrap();
    let provider_account = tx
        .bind_provider_account(provider_account.id, account.id)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    Feed {
        connection: connection.id,
        provider_account,
        account,
    }
}

fn record(id: &str, day: i8, pending: bool) -> NormalizedTransaction {
    NormalizedTransaction {
        provider_id: id.into(),
        posted_on: date(2026, 10, day),
        date_reported: true,
        transacted_on: None,
        amount_minor: -1_250,
        amount_raw: "-12.50".into(),
        description: "CORNER SHOP".into(),
        payee: None,
        memo: None,
        pending,
        replaces_provider_id: None,
        dating: None,
    }
}

async fn apply(world: &World, feed: &Feed, transactions: Vec<NormalizedTransaction>) {
    let now = world.server.clock.now();
    let mut tx = world.server.store.begin().await.unwrap();
    apply_account_batch(
        &mut tx,
        now,
        feed.connection,
        AccountBatch {
            provider_account: feed.provider_account.clone(),
            account: Some(feed.account.clone()),
            transactions,
            removed: Vec::new(),
            coverage: Coverage::Delta,
            unusable: Vec::new(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

/// A pending record, then a posted one of the same amount beside it, which the sync flags as
/// its possible duplicate. Returns (pending, posted).
async fn flagged_pair(world: &mut World) -> (TransactionId, TransactionId) {
    let feed = feed(world).await;
    apply(world, &feed, vec![record("pending-1", 10, true)]).await;
    apply(
        world,
        &feed,
        vec![record("pending-1", 10, true), record("posted-1", 11, false)],
    )
    .await;
    let listed = world
        .api(
            &format!("/api/v1/transactions?account={}", feed.account.id),
            StatusCode::OK,
        )
        .await;
    let id = |pending: bool| -> TransactionId {
        listed["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["pending"] == json!(pending))
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap()
    };
    let (pending, posted) = (id(true), id(false));
    let read = world.read(posted).await;
    assert_eq!(
        read["flags"],
        json!([TransactionFlag::PossibleDuplicate.as_str()])
    );
    (pending, posted)
}

#[tokio::test]
async fn a_possible_duplicate_is_merged_and_its_record_moved_back_out() {
    let mut world = world().await;
    let (pending, posted) = flagged_pair(&mut world).await;
    let page = world.page(&detail(posted)).await;
    assert!(page.body.contains("This may be the same purchase as:"));
    assert!(
        page.body
            .contains(&format!("href=\"/transactions/{pending}\""))
    );

    let reply = world
        .post(
            &format!("{}/merge", detail(posted)),
            &[("duplicate", &pending.to_string())],
        )
        .await;
    assert_redirect(&reply, &detail(posted));
    world
        .api(
            &format!("/api/v1/transactions/{pending}"),
            StatusCode::NOT_FOUND,
        )
        .await;
    let read = world.read(posted).await;
    assert_eq!(read["flags"], json!([]));
    assert_eq!(read["sources"].as_array().unwrap().len(), 2);
    let merged = world.audit(posted).await;
    assert!(
        merged
            .iter()
            .any(|event| event.action == "transaction.merge"
                && event.actor == signed_in(&world.alice))
    );

    let page = world.page(&detail(posted)).await;
    assert!(page.body.contains("Bank records merged into it"));
    let source = read["sources"][0]["id"].as_str().unwrap().to_owned();
    let reply = world
        .post(
            &format!("{}/unmerge", detail(posted)),
            &[("source", &source)],
        )
        .await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    let created: TransactionId = reply
        .header(axum::http::header::LOCATION)
        .strip_prefix("/transactions/")
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(world.read(created).await["sources"][0]["id"], source);
    assert_eq!(
        world.read(posted).await["sources"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn merging_one_that_holds_something_needs_the_box_ticked() {
    let mut world = world().await;
    let (pending, posted) = flagged_pair(&mut world).await;
    world
        .post(
            &format!("{}/details", detail(pending)),
            &[
                ("payee", ""),
                ("notes", "Keep the receipt"),
                ("assignee", ""),
            ],
        )
        .await;
    let path = format!("{}/merge", detail(posted));

    let reply = world
        .post(&path, &[("duplicate", &pending.to_string())])
        .await;
    assert_refused(
        &reply,
        StatusCode::CONFLICT,
        "tick the box to discard them",
        "discard_duplicate_state",
    );
    world.read(pending).await;

    let reply = world
        .post(
            &path,
            &[("duplicate", &pending.to_string()), ("discard", "yes")],
        )
        .await;
    assert_redirect(&reply, &detail(posted));
    world
        .api(
            &format!("/api/v1/transactions/{pending}"),
            StatusCode::NOT_FOUND,
        )
        .await;
}

#[tokio::test]
async fn keeping_that_one_merges_this_one_into_it() {
    let mut world = world().await;
    let (pending, posted) = flagged_pair(&mut world).await;
    let reply = world
        .post(
            &format!("{}/merge", detail(pending)),
            &[("duplicate", &posted.to_string())],
        )
        .await;
    assert_redirect(&reply, &detail(pending));
    world
        .api(
            &format!("/api/v1/transactions/{posted}"),
            StatusCode::NOT_FOUND,
        )
        .await;
}

#[tokio::test]
async fn saying_it_isnt_a_duplicate_clears_the_mark() {
    let mut world = world().await;
    let (pending, posted) = flagged_pair(&mut world).await;
    let reply = world
        .post(&format!("{}/dismiss-duplicate", detail(posted)), &[])
        .await;
    assert_redirect(&reply, &detail(posted));
    let read = world.read(posted).await;
    assert_eq!(read["flags"], json!([]));
    assert_eq!(read["duplicate_candidates"], json!([]));
    world.read(pending).await;
    let event = &world.audit(posted).await[0];
    assert_eq!(event.action, "transaction.dismiss_duplicate");
    assert!(
        !world
            .page(&detail(posted))
            .await
            .body
            .contains("id=\"duplicates\"")
    );
}

/// A member merges, moves a record out and dismisses on this page as the API lets a member do:
/// on every account, as themselves.
#[tokio::test]
async fn a_member_settles_duplicates_as_themselves() {
    let mut world = world().await;
    let (pending, posted) = flagged_pair(&mut world).await;
    let (mut bob, bob_actor) = member(&world).await;
    let page = bob.get(&detail(posted)).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("Keep this one, delete that one"));
    let csrf = page.csrf();

    let reply = bob
        .post_form(
            &format!("{}/merge", detail(posted)),
            &[("csrf", &csrf), ("duplicate", &pending.to_string())],
        )
        .await;
    assert_redirect(&reply, &detail(posted));
    let merged = world.audit(posted).await;
    assert!(
        merged.iter().any(
            |event| event.action == "transaction.merge" && event.actor == signed_in(&bob_actor)
        )
    );

    let source = world.read(posted).await["sources"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let reply = bob
        .post_form(
            &format!("{}/unmerge", detail(posted)),
            &[("csrf", &csrf), ("source", &source)],
        )
        .await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    let unmerged = world.audit(posted).await;
    assert!(
        unmerged
            .iter()
            .any(|event| event.action == "transaction.unmerge"
                && event.actor == signed_in(&bob_actor))
    );
}

#[tokio::test]
async fn a_merge_form_it_cannot_read_is_refused_without_echo() {
    let mut world = world().await;
    let (pending, posted) = flagged_pair(&mut world).await;
    let path = format!("{}/merge", detail(posted));
    let pending = pending.to_string();
    for fields in [
        [("duplicate", "xyzzy"), ("discard", "")],
        [("duplicate", pending.as_str()), ("discard", "xyzzy")],
    ] {
        let reply = world.post(&path, &fields).await;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(!reply.body.contains("xyzzy"));
    }
    let reply = world
        .post(
            &format!("{}/unmerge", detail(posted)),
            &[("source", "xyzzy")],
        )
        .await;
    assert_refused(
        &reply,
        StatusCode::UNPROCESSABLE_ENTITY,
        "the form could not be read",
        "xyzzy",
    );
    let reply = world
        .post(
            &format!("{}/merge", detail(posted)),
            &[("duplicate", &posted.to_string())],
        )
        .await;
    assert_refused(
        &reply,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a transaction cannot be merged with itself",
        "xyzzy",
    );
    assert_eq!(
        world.read(posted).await["flags"],
        json!(["possible_duplicate"])
    );
}

#[tokio::test]
async fn the_duplicate_forms_need_the_sessions_token_and_refuse_unknown_fields() {
    let mut world = world().await;
    let (pending, posted) = flagged_pair(&mut world).await;
    let base = detail(posted);
    let pending = pending.to_string();
    let source = world.read(posted).await["sources"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let forms = [
        (
            format!("{base}/merge"),
            vec![("duplicate", pending.as_str())],
        ),
        (format!("{base}/unmerge"), vec![("source", source.as_str())]),
        (format!("{base}/dismiss-duplicate"), vec![]),
    ];
    for (path, fields) in &forms {
        let mut forged = vec![("csrf", "forged")];
        forged.extend_from_slice(fields);
        let reply = world.browser.post_form(path, &forged).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN);
        let mut extra = fields.clone();
        extra.push(("admin", "yes"));
        let reply = world.post(path, &extra).await;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    }
    assert_eq!(
        world.read(posted).await["flags"],
        json!(["possible_duplicate"])
    );
    world.read(pending.parse().unwrap()).await;
}
