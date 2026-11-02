//! A connection is there for a token only through an account it serves that the token sees, or
//! an `all` grant. Its label is a member's free text, which could name who an account is for.

use super::{World, grant, on, s, world};
use axum::http::{Method, StatusCode};
use coffer_domain::identity::Scope;
use coffer_domain::ids::ConnectionId;
use coffer_domain::policy::{AccountLevel, GrantTarget, TokenGrant};
use coffer_domain::sync::SyncTrigger;
use coffer_domain::time::Clock as _;
use coffer_store::{ConnectionStatus, ConnectionStatusChange, NewSyncRun, SyncRunRecord};
use jiff::SignedDuration;
use serde_json::{Value, json};

impl World {
    /// Marks the connection removed.
    async fn removed(&self, connection: ConnectionId) {
        let now = self.server.clock.now();
        let mut tx = self.server.store.begin().await.unwrap();
        tx.update_connection_status(
            connection,
            ConnectionStatusChange {
                status: ConnectionStatus::Removed,
                last_error: None,
                attempted_at: None,
                succeeded_at: None,
                at: now,
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }

    /// Makes the connection look as a clock that ran ahead leaves it: last attempted, and with a
    /// run started, 30 days from now, which a listing repairs.
    async fn run_started_ahead(&self, connection: ConnectionId) -> SyncRunRecord {
        let now = self.server.clock.now();
        let ahead = now
            .checked_add(SignedDuration::from_hours(24 * 30))
            .unwrap();
        let mut tx = self.server.store.begin().await.unwrap();
        tx.update_connection_status(
            connection,
            ConnectionStatusChange {
                status: ConnectionStatus::Active,
                last_error: None,
                attempted_at: Some(ahead),
                succeeded_at: Some(ahead),
                at: ahead,
            },
        )
        .await
        .unwrap();
        let run = tx
            .insert_sync_run(NewSyncRun {
                connection,
                trigger: SyncTrigger::Schedule,
                requested_by: None,
                queued_at: now,
            })
            .await
            .unwrap();
        let run = tx.start_sync_run(run.id, ahead).await.unwrap();
        tx.commit().await.unwrap();
        run
    }
}

fn listed_ids(body: &Value) -> Vec<String> {
    body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|connection| connection["id"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn a_token_lists_only_the_connections_serving_an_account_it_sees() {
    let world = world().await;
    let (a, b, c) = (
        world.account("Checking").await,
        world.account("Hidden").await,
        world.account("Unserved").await,
    );
    let serving_a = world.connection(&[("Everyday", Some(a))]).await;
    let serving_b = world.connection(&[("Secret", Some(b))]).await;
    let serving_both = world
        .connection(&[("Shared", Some(a)), ("Shared secret", Some(b))])
        .await;
    let unbound_only = world.connection(&[("Unbound card", None)]).await;
    let reports_nothing = world.connection(&[]).await;
    let removed_serving_a = world.connection(&[("Old everyday", Some(a))]).await;
    world.removed(removed_serving_a).await;
    let removed_serving_b = world.connection(&[("Old secret", Some(b))]).await;
    world.removed(removed_serving_b).await;
    let every = [
        serving_a,
        serving_b,
        serving_both,
        unbound_only,
        reports_nothing,
        removed_serving_a,
        removed_serving_b,
    ];
    let of_a = [serving_a, serving_both, removed_serving_a];

    let cases: [(Scope, Vec<TokenGrant>, &[ConnectionId]); 7] = [
        (Scope::Read, vec![on(a, AccountLevel::Balances)], &of_a),
        (Scope::Read, vec![on(a, AccountLevel::Transactions)], &of_a),
        (Scope::Admin, vec![on(a, AccountLevel::Write)], &of_a),
        (Scope::Read, vec![on(c, AccountLevel::Transactions)], &[]),
        (
            Scope::Read,
            vec![grant(GrantTarget::All, AccountLevel::Balances)],
            &every,
        ),
        (
            Scope::Read,
            vec![grant(GrantTarget::All, AccountLevel::Transactions)],
            &every,
        ),
        (Scope::Read, Vec::new(), &every),
    ];
    for (scope, grants, expected) in cases {
        let (_, token) = world.token(scope, grants).await;
        let (status, body) = world.get("/api/v1/connections", &token).await;
        assert_eq!(status, StatusCode::OK);
        let expected: Vec<String> = expected.iter().map(|id| s(*id)).collect();
        assert_eq!(listed_ids(&body), expected);
    }

    // A member signed in sees all of them, as a token without grants does.
    let session = world
        .server
        .sync
        .list_connections(&world.alice)
        .await
        .unwrap();
    assert_eq!(session.len(), every.len());
}

/// Every route naming a connection answers a restricted token the same for one it can't see as
/// for one that doesn't exist, and changes nothing.
#[tokio::test]
async fn a_hidden_connection_answers_like_a_missing_one() {
    let world = world().await;
    let (a, b) = (
        world.account("Checking").await,
        world.account("Hidden").await,
    );
    let hidden = world.connection(&[("Secret", Some(b))]).await;
    let missing = ConnectionId::new();
    let setup = json!({"setup_token": "aHR0cHM6Ly9icmlkZ2U"});
    let routes = |id: ConnectionId| {
        [
            (
                Method::GET,
                format!("/api/v1/connections/{id}/sync-runs"),
                None,
            ),
            (Method::POST, format!("/api/v1/connections/{id}/sync"), None),
            (Method::DELETE, format!("/api/v1/connections/{id}"), None),
            (
                Method::POST,
                format!("/api/v1/connections/{id}/simplefin/reconnect"),
                Some(&setup),
            ),
        ]
    };
    for (scope, level) in [
        (Scope::Read, AccountLevel::Balances),
        (Scope::Read, AccountLevel::Transactions),
        (Scope::Admin, AccountLevel::Write),
    ] {
        let (_, token) = world.token(scope, vec![on(a, level)]).await;
        let audited = world.audit_count().await;
        for (of_hidden, of_missing) in routes(hidden).into_iter().zip(routes(missing)) {
            let (method, uri, body) = of_hidden;
            let answer = world.send(method, &uri, &token, body, None).await;
            let (method, uri, body) = of_missing;
            let absent = world.send(method, &uri, &token, body, None).await;
            assert_eq!(answer.0, StatusCode::FORBIDDEN);
            assert!(
                answer == absent,
                "a hidden connection reads as a missing one"
            );
        }
        assert_eq!(world.audit_count().await, audited, "nothing was written");
    }
}

/// A token never holds the id of a connection it can't list: a provider account it reads, and the
/// source of a transaction it reads, both name one that serves what it sees.
#[tokio::test]
async fn a_connection_a_token_is_told_of_is_one_it_can_list() {
    let world = world().await;
    let (a, b) = (
        world.account("Checking").await,
        world.account("Hidden").await,
    );
    let serving_a = world.connection(&[("Everyday", Some(a))]).await;
    let serving_b = world.connection(&[("Secret", Some(b))]).await;
    let (ours, _) = world.imported(serving_a, a, "Groceries").await;
    world.imported(serving_b, b, "Tuition").await;
    let (_, token) = world
        .token(Scope::Read, vec![on(a, AccountLevel::Transactions)])
        .await;

    let (_, listed) = world.get("/api/v1/connections", &token).await;
    assert_eq!(listed_ids(&listed), [s(serving_a)]);
    let (_, accounts) = world.get("/api/v1/provider-accounts", &token).await;
    assert_eq!(accounts["items"][0]["connection"], s(serving_a));
    assert_eq!(accounts["items"].as_array().unwrap().len(), 1);
    let (_, read) = world
        .get(&format!("/api/v1/transactions/{ours}"), &token)
        .await;
    assert_eq!(read["sources"][0]["connection"], s(serving_a));
    let (_, page) = world.get("/api/v1/transactions", &token).await;
    assert!(!page.to_string().contains(&s(serving_b)));
    assert!(!page.to_string().contains(&s(b)));
}

/// A merge moves a source onto the target, so it adds a connection to what a transaction names:
/// still one serving the account, as a merge across accounts is refused, whoever asks.
#[tokio::test]
async fn a_merged_source_names_a_connection_the_token_can_list() {
    let world = world().await;
    let (a, b) = (
        world.account("Checking").await,
        world.account("Hidden").await,
    );
    let target_feed = world.connection(&[("Target", Some(a))]).await;
    let duplicate_feed = world.connection(&[("Duplicate", Some(a))]).await;
    let hidden_feed = world.connection(&[("Secret", Some(b))]).await;
    let (target, _) = world.imported(target_feed, a, "Groceries").await;
    let (duplicate, _) = world.imported(duplicate_feed, a, "Groceries").await;
    let (hidden, _) = world.imported(hidden_feed, b, "Groceries").await;
    world.flag_duplicate(target, duplicate).await;
    let (_, everything) = world
        .token(
            Scope::Write,
            vec![grant(GrantTarget::All, AccountLevel::Write)],
        )
        .await;
    let uri = format!("/api/v1/transactions/{target}/merge");

    let (status, _) = world
        .call(
            Method::POST,
            &uri,
            &everything,
            Some(json!({"duplicate": hidden})),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a merge stays in one account"
    );
    let (status, _) = world
        .call(
            Method::POST,
            &uri,
            &everything,
            Some(json!({"duplicate": duplicate})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let (_, token) = world
        .token(Scope::Read, vec![on(a, AccountLevel::Transactions)])
        .await;
    let (_, read) = world
        .get(&format!("/api/v1/transactions/{target}"), &token)
        .await;
    let mut named: Vec<String> = read["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|source| source["connection"].as_str().unwrap().to_owned())
        .collect();
    named.sort();
    let mut expected = vec![s(target_feed), s(duplicate_feed)];
    expected.sort();
    assert_eq!(
        named, expected,
        "the merged source came with its connection"
    );
    let (_, listed) = world.get("/api/v1/connections", &token).await;
    let mut listed = listed_ids(&listed);
    listed.sort();
    assert_eq!(listed, expected);
}

/// Listing repairs run starts a clock left ahead, but only for a connection it shows: a read token
/// writes nothing to the runs of one it can't see. A token that does see it shows the repair is
/// there to skip.
#[tokio::test]
async fn a_scoped_listing_leaves_a_hidden_connections_runs_alone() {
    let world = world().await;
    let (a, b) = (
        world.account("Checking").await,
        world.account("Hidden").await,
    );
    world.connection(&[("Everyday", Some(a))]).await;
    let hidden = world.connection(&[("Secret", Some(b))]).await;
    let run = world.run_started_ahead(hidden).await;
    let ahead = run.started_at.unwrap();
    let (_, scoped) = world
        .token(Scope::Read, vec![on(a, AccountLevel::Transactions)])
        .await;

    let (status, _) = world.get("/api/v1/connections", &scoped).await;
    assert_eq!(status, StatusCode::OK);
    let after = world.server.store.get_sync_run(run.id).await.unwrap();
    assert_eq!(after.started_at, Some(ahead), "the run was not touched");

    let (_, seeing) = world.token(Scope::Read, Vec::new()).await;
    let (status, _) = world.get("/api/v1/connections", &seeing).await;
    assert_eq!(status, StatusCode::OK);
    let after = world.server.store.get_sync_run(run.id).await.unwrap();
    assert_eq!(after.started_at, Some(world.server.clock.now()));
}
