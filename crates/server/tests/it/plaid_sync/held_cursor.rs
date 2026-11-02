//! A cursor held for good: when the connection says so, who sees it, and how
//! a member lets the cursor move past the failing records.

use super::{ITEM_REMOVE, REMOVE, SYNC, World, account_json, accounts_answer, page, record, world};
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use coffer_domain::audit::AuditActor;
use coffer_domain::identity::{Actor, Role, Scope, TokenName, Via};
use coffer_domain::ids::ProviderAccountId;
use coffer_domain::policy::{AccountLevel, GrantTarget, TokenGrant};
use coffer_domain::sync::SyncRunStatus;
use coffer_server::http::router;
use coffer_server::sync::SyncError;
use coffer_server::sync::connections::{CURSOR_HELD_TEXT, HELD_AFTER_SYNCS, HeldCursorView};
use coffer_server::testkit::INJECTED_FAILURE_RECORD_ID;
use http_body_util::BodyExt as _;
use secrecy::ExposeSecret as _;
use serde_json::{Value, json};
use tower::ServiceExt as _;

const STRAY: &str = "acct-stray";

impl World {
    /// Plaid keeps sending a record of an account it does not list: the
    /// account fails, and each sync fetches the record again.
    fn stray_record_every_sync(&self) {
        self.script.always(
            SYNC,
            StatusCode::OK,
            page(&[record("stray", STRAY, "1")], &[], &[], "cursor-1", false),
        );
    }

    async fn syncs(&self, count: u32) {
        for _ in 0..count {
            self.sync().await;
        }
    }

    async fn held_cursor_as(&self, actor: &Actor) -> Option<HeldCursorView> {
        self.server
            .sync
            .list_connections(actor)
            .await
            .unwrap()
            .into_iter()
            .find(|view| view.id == self.connection)
            .unwrap()
            .held_cursor
    }

    /// Releases what the member sees holding the cursor now.
    async fn release(&self) -> Result<Vec<ProviderAccountId>, SyncError> {
        let shown = self
            .held_cursor()
            .await
            .map(|held| held.provider_accounts)
            .unwrap_or_default();
        self.server
            .sync
            .release_held_cursor(&self.alice, self.connection, &shown)
            .await
    }

    async fn audits(&self, action: &str) -> Vec<coffer_store::AuditEvent> {
        self.audit()
            .await
            .into_iter()
            .filter(|event| event.action == action)
            .collect()
    }

    async fn held_cursor(&self) -> Option<HeldCursorView> {
        self.held_cursor_as(&self.alice).await
    }

    async fn token_actor(&self, scope: Scope, grants: Vec<TokenGrant>) -> (Actor, String) {
        let (view, secret) = self
            .server
            .agent_tokens
            .create(
                &self.alice,
                TokenName::parse("claude").unwrap(),
                scope,
                None,
                grants,
            )
            .await
            .unwrap();
        let actor = Actor {
            member: self.alice.member,
            role: Role::Admin,
            via: Via::Agent {
                token: view.id,
                scope,
            },
        };
        (actor, secret.expose_secret().to_owned())
    }

    async fn release_audits(&self) -> Vec<coffer_store::AuditEvent> {
        self.audit()
            .await
            .into_iter()
            .filter(|event| event.action == "connection.release_cursor")
            .collect()
    }

    async fn api_connections(&self, secret: &str) -> Value {
        let request = Request::builder()
            .method(Method::GET)
            .uri("/api/v1/connections")
            .header(header::AUTHORIZATION, format!("Bearer {secret}"))
            .body(Body::empty())
            .unwrap();
        let response = router(self.server.app_state())
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }
}

#[tokio::test]
async fn the_connection_says_so_once_an_account_has_held_the_cursor_for_good() {
    let world = world().await;
    world.stray_record_every_sync();

    world.syncs(HELD_AFTER_SYNCS - 1).await;
    assert_eq!(world.held_cursor().await, None, "a few syncs are no hold");

    world.sync().await;
    let stray = world.provider_account(STRAY).await;
    let held = world.held_cursor().await.expect("the cursor is held");
    assert_eq!(held.text, CURSOR_HELD_TEXT);
    assert_eq!(held.provider_accounts, [stray.id]);
    assert!(!held.release_pending);
    assert_eq!(
        held.records, 1,
        "the records the last fetch could not store"
    );
    assert_eq!(world.connection().await.provider_state, None, "still held");

    let (_, secret) = world.token_actor(Scope::Read, Vec::new()).await;
    let listed = world.api_connections(&secret).await;
    let view = &listed["items"][0]["held_cursor"];
    assert_eq!(view["text"], json!(CURSOR_HELD_TEXT));
    assert_eq!(view["provider_accounts"], json!([stray.id.to_string()]));
    assert_eq!(view["release_pending"], json!(false));
    assert_eq!(view["records"], json!(1));
}

#[tokio::test]
async fn an_actor_not_seeing_every_account_gets_no_signal() {
    let world = world().await;
    world.stray_record_every_sync();
    world.syncs(HELD_AFTER_SYNCS).await;
    assert!(world.held_cursor().await.is_some());

    let account = world.account().await;
    for grants in [
        vec![TokenGrant {
            target: GrantTarget::Account(account),
            level: AccountLevel::Transactions,
        }],
        vec![TokenGrant {
            target: GrantTarget::All,
            level: AccountLevel::Balances,
        }],
    ] {
        let (narrow, secret) = world.token_actor(Scope::Read, grants).await;
        assert_eq!(
            world.held_cursor_as(&narrow).await,
            None,
            "the signal follows the rule of the last error"
        );
        let listed = world.api_connections(&secret).await;
        assert_eq!(listed["items"][0]["held_cursor"], Value::Null);
        assert_eq!(listed["items"][0]["last_error"], Value::Null);
    }
    let (wide, _) = world
        .token_actor(
            Scope::Read,
            vec![TokenGrant {
                target: GrantTarget::All,
                level: AccountLevel::Transactions,
            }],
        )
        .await;
    assert!(world.held_cursor_as(&wide).await.is_some());
}

#[tokio::test]
async fn failures_fetching_again_cannot_fix_are_no_hold() {
    let world = world().await;
    world.script.always(
        SYNC,
        StatusCode::OK,
        page(
            &[record("coin", "acct-crypto", "1")],
            &[],
            &[],
            "cursor-1",
            false,
        ),
    );

    world.syncs(HELD_AFTER_SYNCS + 1).await;

    assert_eq!(world.provider_account("acct-crypto").await.held_syncs, 0);
    assert_eq!(world.held_cursor().await, None);
}

#[tokio::test]
async fn an_account_that_applies_ends_the_hold() {
    let world = world().await;
    world.stray_record_every_sync();
    world.syncs(HELD_AFTER_SYNCS).await;
    assert!(world.held_cursor().await.is_some());

    world.script.always(
        super::GET_ACCOUNTS,
        StatusCode::OK,
        accounts_answer(&[account_json(STRAY, "depository", &json!(1))]),
    );
    let run = world.sync().await;

    assert_eq!(run.status, SyncRunStatus::Succeeded);
    assert_eq!(world.held_cursor().await, None);
    assert_eq!(world.provider_account(STRAY).await.held_syncs, 0);
    assert_eq!(
        world.connection().await.provider_state.as_deref(),
        Some("cursor-1")
    );
}

#[tokio::test]
async fn releasing_lets_the_next_sync_pass_the_records_for_good() {
    let world = world().await;
    world.stray_record_every_sync();
    world.syncs(HELD_AFTER_SYNCS).await;
    let stray = world.provider_account(STRAY).await;

    let released = world.release().await.unwrap();

    assert_eq!(released, [stray.id]);
    assert!(world.held_cursor().await.unwrap().release_pending);
    assert_eq!(
        world.connection().await.provider_state,
        None,
        "the cursor moves with the next sync, not before"
    );
    let audits = world.release_audits().await;
    assert_eq!(audits.len(), 1);
    assert_eq!(audits[0].entity_type, "connection");
    assert_eq!(audits[0].entity_id, world.connection.to_string());
    assert_eq!(
        audits[0].actor,
        AuditActor::Member {
            member: world.alice.member,
            agent: None
        }
    );
    let changes = serde_json::to_string(&audits[0].changes).unwrap();
    assert!(changes.contains(&stray.id.to_string()));
    assert!(!changes.contains("stray"), "no provider record is named");

    let run = world.sync().await;

    assert_eq!(run.status, SyncRunStatus::Partial);
    assert!(
        run.detail
            .as_deref()
            .is_some_and(|detail| detail.contains("1 skipped")),
        "the passed records are counted"
    );
    assert_eq!(
        world.connection().await.provider_state.as_deref(),
        Some("cursor-1")
    );
    assert_eq!(world.held_cursor().await, None);
    let passed = world.audits("connection.cursor_passed").await;
    assert_eq!(passed.len(), 1, "the pass itself is audited");
    assert_eq!(
        passed[0].actor,
        AuditActor::System {
            reason: "system:sync".to_owned()
        }
    );
    let changes = serde_json::to_string(&passed[0].changes).unwrap();
    assert!(changes.contains(&stray.id.to_string()) && changes.contains('1'));
    let stray = world.provider_account(STRAY).await;
    assert_eq!((stray.held_syncs, stray.hold_released), (0, false));
    assert!(
        stray.error.is_some(),
        "the account still says what is wrong"
    );
}

#[tokio::test]
async fn a_release_is_used_up_by_the_sync_that_follows() {
    let world = world().await;
    world.stray_record_every_sync();
    world.syncs(HELD_AFTER_SYNCS).await;
    world.release().await.unwrap();
    world.sync().await;

    world.syncs(HELD_AFTER_SYNCS - 1).await;
    assert_eq!(world.held_cursor().await, None, "holding counts anew");
    world.sync().await;
    assert!(world.held_cursor().await.is_some());
    assert_eq!(
        world.release_audits().await.len(),
        1,
        "only the member's request is audited"
    );
}

#[tokio::test]
async fn a_release_loses_nothing_when_the_records_apply_after_all() {
    let world = world().await;
    let stray = world.bound(STRAY).await;
    world.stray_record_every_sync();
    world.syncs(HELD_AFTER_SYNCS).await;
    world.script.always(
        super::GET_ACCOUNTS,
        StatusCode::OK,
        accounts_answer(&[account_json(STRAY, "depository", &json!(1))]),
    );
    world.release().await.unwrap();

    let run = world.sync().await;

    assert_eq!(run.status, SyncRunStatus::Succeeded);
    assert_eq!(world.transactions(stray).await.len(), 1, "not passed");
    assert!(!world.provider_account(STRAY).await.hold_released);
}

#[tokio::test]
async fn a_release_passes_records_that_cannot_be_stored() {
    let world = world().await;
    let checking = world.bound("acct-checking").await;
    world.server.inject_source_insert_failure().await;
    world.script.always(
        SYNC,
        StatusCode::OK,
        page(
            &[record(INJECTED_FAILURE_RECORD_ID, "acct-checking", "1")],
            &[],
            &[],
            "cursor-1",
            false,
        ),
    );
    world.syncs(HELD_AFTER_SYNCS).await;
    assert!(world.held_cursor().await.is_some());
    world.release().await.unwrap();

    let run = world.sync().await;

    assert!(
        run.detail
            .as_deref()
            .is_some_and(|detail| detail.contains("1 skipped"))
    );
    assert_eq!(world.transactions(checking).await.len(), 0);
    assert_eq!(
        world.connection().await.provider_state.as_deref(),
        Some("cursor-1")
    );
    assert_eq!(world.held_cursor().await, None);
}

#[tokio::test]
async fn nothing_to_release_is_a_conflict() {
    let world = world().await;
    world.stray_record_every_sync();
    for syncs in [0, HELD_AFTER_SYNCS - 1] {
        world.syncs(syncs).await;
        let refused = world.release().await;
        assert!(
            matches!(refused, Err(SyncError::Conflict(_))),
            "a cursor that is not held cannot be released"
        );
    }
    assert_eq!(world.release_audits().await.len(), 0);
    assert!(!world.provider_account(STRAY).await.hold_released);
}

#[tokio::test]
async fn only_a_member_who_may_manage_the_connection_releases_it() {
    let world = world().await;
    world.stray_record_every_sync();
    world.syncs(HELD_AFTER_SYNCS).await;
    let (token, _) = world.token_actor(Scope::Admin, Vec::new()).await;
    let outsider = Actor {
        member: coffer_domain::ids::MemberId::new(),
        role: Role::Member,
        via: Via::Session,
    };

    for (actor, why) in [
        (&token, "an agent token, whatever its scope"),
        (&outsider, "a member who did not add it and is no admin"),
    ] {
        let shown = world.held_cursor().await.unwrap().provider_accounts;
        let refused = world
            .server
            .sync
            .release_held_cursor(actor, world.connection, &shown)
            .await;
        assert!(matches!(refused, Err(SyncError::Denied(_))), "{why}");
    }
    assert!(!world.provider_account(STRAY).await.hold_released);
    assert_eq!(world.release_audits().await.len(), 0);
}

/// Two accounts hold the cursor, released at different times. The first
/// release is not used up while the second account still blocks the cursor,
/// and nothing is counted or audited until the cursor moves.
#[tokio::test]
async fn a_release_waits_for_the_cursor_to_move() {
    let world = world().await;
    let only_a = page(&[record("a1", "acct-a", "1")], &[], &[], "cursor-1", false);
    let both = page(
        &[record("a1", "acct-a", "1"), record("b1", "acct-b", "1")],
        &[],
        &[],
        "cursor-1",
        false,
    );
    world.script.page(only_a.clone());
    world.script.page(only_a);
    world.script.always(SYNC, StatusCode::OK, both);
    world.syncs(HELD_AFTER_SYNCS).await;
    let (a, b) = (
        world.provider_account("acct-a").await,
        world.provider_account("acct-b").await,
    );
    assert_eq!((a.held_syncs, b.held_syncs), (3, 1));
    assert_eq!(world.release().await.unwrap(), [a.id]);

    let blocked = world.sync().await;

    assert!(
        blocked
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("0 skipped")),
        "nothing is passed while another account blocks the cursor"
    );
    assert_eq!(world.connection().await.provider_state, None);
    assert!(world.provider_account("acct-a").await.hold_released);
    assert_eq!(world.provider_account("acct-a").await.held_syncs, 3);
    assert_eq!(world.audits("connection.cursor_passed").await.len(), 0);

    world.sync().await;
    let held = world.held_cursor().await.unwrap();
    assert_eq!(held.provider_accounts.len(), 2);
    assert!(!held.release_pending, "the newcomer was not released");
    assert_eq!(world.release().await.unwrap().len(), 2);

    let moved = world.sync().await;

    assert!(
        moved
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("2 skipped")),
        "both accounts' records are passed together"
    );
    assert_eq!(
        world.connection().await.provider_state.as_deref(),
        Some("cursor-1")
    );
    let passed = world.audits("connection.cursor_passed").await;
    assert_eq!(passed.len(), 1);
    assert!(
        serde_json::to_string(&passed[0].changes)
            .unwrap()
            .contains('2')
    );
    assert_eq!(world.held_cursor().await, None);
}

/// A release is not a standing permission: an account a completed fetch did
/// not have loses its hold and its release, and records it gets months later
/// are held again like any other.
#[tokio::test]
async fn a_stale_release_cannot_pass_later_records() {
    let world = world().await;
    world.stray_record_every_sync();
    world.syncs(HELD_AFTER_SYNCS).await;
    world.release().await.unwrap();
    world
        .script
        .always(SYNC, StatusCode::OK, page(&[], &[], &[], "cursor-2", false));

    world.sync().await;

    let stray = world.provider_account(STRAY).await;
    assert_eq!((stray.held_syncs, stray.hold_released), (0, false));
    assert_eq!(
        world.connection().await.provider_state.as_deref(),
        Some("cursor-2")
    );
    assert_eq!(world.held_cursor().await, None);

    world.script.always(
        SYNC,
        StatusCode::OK,
        page(&[record("later", STRAY, "1")], &[], &[], "cursor-3", false),
    );
    let run = world.sync().await;

    assert!(
        run.detail
            .as_deref()
            .is_some_and(|detail| detail.contains("0 skipped")),
        "later records are not passed"
    );
    assert_eq!(
        world.connection().await.provider_state.as_deref(),
        Some("cursor-2"),
        "the cursor stays before them"
    );
    let stray = world.provider_account(STRAY).await;
    assert_eq!((stray.held_syncs, stray.hold_released), (1, false));
    assert_eq!(world.audits("connection.cursor_passed").await.len(), 0);
}

/// Only the records that really fail are given up: the others are stored.
#[tokio::test]
async fn a_release_gives_up_only_the_records_that_fail() {
    let world = world().await;
    let checking = world.bound("acct-checking").await;
    world.server.inject_source_insert_failure().await;
    world.script.always(
        SYNC,
        StatusCode::OK,
        page(
            &[
                record("fine", "acct-checking", "1"),
                record(INJECTED_FAILURE_RECORD_ID, "acct-checking", "2"),
            ],
            &[],
            &[],
            "cursor-1",
            false,
        ),
    );
    world.syncs(HELD_AFTER_SYNCS).await;
    assert_eq!(world.transactions(checking).await.len(), 0);
    assert_eq!(world.held_cursor().await.unwrap().records, 2);
    world.release().await.unwrap();

    let run = world.sync().await;

    let stored = world.transactions(checking).await;
    assert_eq!(stored.len(), 1, "the record that can be stored is");
    assert_eq!(stored[0].sources[0].provider_txn_id, "fine");
    assert!(
        run.detail
            .as_deref()
            .is_some_and(|detail| detail.contains("1 skipped")),
        "only the failing record is passed and counted"
    );
    assert_eq!(
        world.connection().await.provider_state.as_deref(),
        Some("cursor-1")
    );
    let passed = world.audits("connection.cursor_passed").await;
    assert_eq!(passed.len(), 1);
    assert_eq!(
        serde_json::to_string(&passed[0].changes)
            .unwrap()
            .matches("\"after\":1")
            .count(),
        1,
        "the audit counts the one record"
    );
}

#[tokio::test]
async fn a_release_names_the_accounts_the_member_saw() {
    let world = world().await;
    world.stray_record_every_sync();
    world.syncs(HELD_AFTER_SYNCS).await;
    let stray = world.provider_account(STRAY).await;

    for shown in [Vec::new(), vec![stray.id, ProviderAccountId::new()]] {
        let refused = world
            .server
            .sync
            .release_held_cursor(&world.alice, world.connection, &shown)
            .await;
        assert!(
            matches!(refused, Err(SyncError::Conflict(_))),
            "a different set is refused"
        );
    }
    assert!(!world.provider_account(STRAY).await.hold_released);
    assert_eq!(world.release_audits().await.len(), 0);
}

#[tokio::test]
async fn a_removed_connection_shows_no_hold() {
    let world = world().await;
    world.stray_record_every_sync();
    world.syncs(HELD_AFTER_SYNCS).await;
    world.script.reply(REMOVE, StatusCode::OK, ITEM_REMOVE);

    world
        .server
        .sync
        .remove_connection(&world.alice, world.connection)
        .await
        .unwrap();

    assert_eq!(world.held_cursor().await, None);
}
