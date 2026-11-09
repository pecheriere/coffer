//! Settling a planned item for one month through the real router (spec §8.9 R3, R9a): what
//! may be settled, for which months, and that a token short of `all` at `write` learns
//! nothing and changes nothing.

use axum::http::{Method, StatusCode};
use coffer_domain::audit::{AuditActor, AuditValue};
use coffer_domain::identity::{Actor, Role, Scope, TokenName, Via};
use coffer_domain::ids::PlannedItemId;
use coffer_domain::policy::AccountLevel::{Transactions, Write};
use coffer_domain::transactions::CategoryKind;
use coffer_server::secrets::SecretToken;
use coffer_store::{IdempotencyOwner, Reservation};
use jiff::Timestamp;
use serde_json::{Value, json};

mod changes;

use super::{World, all, goal, item_uri, on, one_off, recurring, with, world};

/// The day the clock says in this file: `2027-01` is this month.
const TODAY: &str = "2027-01-15T12:00:00Z";

fn at(text: &str) -> Timestamp {
    text.parse().unwrap()
}

async fn settling_world() -> World {
    let world = world().await;
    world.server.clock.set(at(TODAY));
    world
}

fn uri(id: impl std::fmt::Display, month: &str) -> String {
    format!("{}/settlements/{month}", item_uri(id))
}

fn paid() -> Value {
    json!({"kind": "paid"})
}

/// A bill that has been running for years, so any month nearby holds an occurrence.
fn long_running(category: coffer_domain::ids::CategoryId) -> Value {
    with(
        recurring(category),
        "schedule",
        json!({"type": "recurring", "every": 1, "unit": "months", "anchor_on": "2000-01-01"}),
    )
}

impl World {
    async fn put_settlement(
        &self,
        token: &SecretToken,
        id: &str,
        month: &str,
        body: &Value,
    ) -> (StatusCode, Value) {
        self.call(Method::PUT, &uri(id, month), token, Some(body))
            .await
    }

    /// Settles `id` for `month` as paid, which must succeed; returns the settlement.
    async fn settled(&self, id: &str, month: &str) -> Value {
        let (status, answer) = self.put_settlement(&self.admin, id, month, &paid()).await;
        assert_eq!(status, StatusCode::OK);
        answer
    }

    async fn item(&self, id: &str) -> Value {
        let (status, answer) = self
            .call(Method::GET, &item_uri(id), &self.admin, None)
            .await;
        assert_eq!(status, StatusCode::OK);
        answer
    }

    /// The months `id` is settled for, as an admin reads them.
    async fn months_of(&self, id: &str) -> Vec<String> {
        self.item(id).await["settlements"]
            .as_array()
            .unwrap()
            .iter()
            .map(|one| one["month"].as_str().unwrap().to_owned())
            .collect()
    }

    /// A second member, "bob", with a token of his own at `write`.
    async fn second_member(
        &self,
    ) -> (
        coffer_domain::ids::MemberId,
        coffer_domain::ids::AgentTokenId,
        SecretToken,
    ) {
        let invite = self
            .server
            .identity
            .create_invite(&self.alice, Role::Member, None)
            .await
            .unwrap();
        let bob = self
            .server
            .authenticator()
            .enroll(&invite, "bob", "Bob")
            .await;
        let actor = Actor {
            member: bob,
            role: Role::Member,
            via: Via::Session,
        };
        let (record, secret) = self
            .server
            .agent_tokens
            .create(
                &actor,
                TokenName::parse("bobs").unwrap(),
                Scope::Write,
                None,
                Vec::new(),
            )
            .await
            .unwrap();
        (bob, record.id, secret)
    }

    /// Whether `token`'s idempotency key `key` is still unused.
    async fn key_is_unused(&self, agent: coffer_domain::ids::AgentTokenId, key: &str) -> bool {
        let owner = IdempotencyOwner {
            member: self.alice.member,
            agent: Some(agent),
        };
        let mut tx = self.server.store.begin().await.unwrap();
        let reservation = tx
            .reserve_idempotency(owner, key, [0; 32], at(TODAY))
            .await
            .unwrap();
        reservation == Reservation::Fresh
    }
}

#[tokio::test]
async fn a_settlement_shows_on_the_item_with_who_and_when() {
    let world = settling_world().await;
    let id = world.create(&recurring(world.expense)).await;
    let (token_id, token) = world.token(Scope::Write, Vec::new()).await;

    let (status, answer) = world.put_settlement(&token, &id, "2027-01", &paid()).await;

    assert_eq!(status, StatusCode::OK);
    let expected = json!({
        "month": "2027-01",
        "kind": "paid",
        "member": world.alice.member,
        "agent": token_id,
        "settled_at": "2027-01-15T12:00:00Z",
    });
    assert_eq!(answer, expected);
    assert_eq!(world.item(&id).await["settlements"], json!([expected]));
    let (_, list) = world.call(Method::GET, super::ITEMS, &token, None).await;
    assert_eq!(list["items"][0]["settlements"], json!([expected]));
}

#[tokio::test]
async fn settling_changes_nothing_of_the_item() {
    let world = settling_world().await;
    let id = world.create(&recurring(world.expense)).await;
    let before = world.item(&id).await;

    world.server.clock.set(at("2027-01-16T12:00:00Z"));
    world.settled(&id, "2027-01").await;

    let mut after = world.item(&id).await;
    after["settlements"] = before["settlements"].clone();
    assert_eq!(after, before);
}

#[tokio::test]
async fn settling_is_audited_with_member_and_agent() {
    let world = settling_world().await;
    let id = world.create(&recurring(world.expense)).await;
    let (token_id, token) = world.token(Scope::Write, Vec::new()).await;

    world.put_settlement(&token, &id, "2027-01", &paid()).await;

    let events = world.audit().await;
    let event = &events[0];
    assert_eq!(event.action, "planned_item.settle");
    assert_eq!(event.entity_id, id);
    assert_eq!(
        event.actor,
        AuditActor::Member {
            member: world.alice.member,
            agent: Some(token_id),
        }
    );
    let change = |field: &str| {
        event
            .changes
            .iter()
            .find(|(name, _)| *name == field)
            .unwrap()
            .1
            .after
            .clone()
    };
    assert_eq!(change("month"), AuditValue::Text("2027-01".into()));
    assert_eq!(change("kind"), AuditValue::Text("paid".into()));
}

/// The same request again changes nothing: not who, not when, not the audit log.
#[tokio::test]
async fn settling_again_the_same_way_writes_nothing() {
    let world = settling_world().await;
    let id = world.create(&recurring(world.expense)).await;
    let first = world.settled(&id, "2027-01").await;
    let audited = world.audit().await.len();

    world.server.clock.set(at("2027-01-16T12:00:00Z"));
    let again = world.settled(&id, "2027-01").await;

    assert_eq!(again, first);
    assert_eq!(world.audit().await.len(), audited);
}

#[tokio::test]
async fn another_kind_replaces_the_months_settlement() {
    let world = settling_world().await;
    let id = world.create(&recurring(world.expense)).await;
    world.settled(&id, "2027-01").await;

    let (status, answer) = world
        .put_settlement(&world.admin, &id, "2027-01", &json!({"kind": "skipped"}))
        .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(answer["kind"], "skipped");
    assert_eq!(world.months_of(&id).await, ["2027-01"]);
    assert_eq!(world.item(&id).await["settlements"][0]["kind"], "skipped");
    let events = world.audit().await;
    let kind = events[0]
        .changes
        .iter()
        .find(|(name, _)| *name == "kind")
        .unwrap()
        .1;
    assert_eq!(kind.before, AuditValue::Text("paid".into()));
    assert_eq!(kind.after, AuditValue::Text("skipped".into()));
}

#[tokio::test]
async fn an_unknown_kind_is_refused() {
    let world = settling_world().await;
    let id = world.create(&recurring(world.expense)).await;

    let (status, _) = world
        .put_settlement(&world.admin, &id, "2027-01", &json!({"kind": "maybe"}))
        .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(world.months_of(&id).await, Vec::<String>::new());
}

#[tokio::test]
async fn deleting_a_settlement_is_audited_once() {
    let world = settling_world().await;
    let id = world.create(&recurring(world.expense)).await;
    world.settled(&id, "2027-01").await;
    let audited = world.audit().await.len();

    for _ in 0..2 {
        let (status, _) = world
            .call(Method::DELETE, &uri(&id, "2027-01"), &world.admin, None)
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }

    assert_eq!(world.months_of(&id).await, Vec::<String>::new());
    let events = world.audit().await;
    assert_eq!(events.len(), audited + 1);
    assert_eq!(events[0].action, "planned_item.unsettle");
}

#[tokio::test]
async fn a_missing_item_is_a_404_for_both_verbs() {
    let world = settling_world().await;
    let missing = PlannedItemId::new();
    let (put, _) = world
        .call(
            Method::PUT,
            &uri(missing, "2027-01"),
            &world.admin,
            Some(&paid()),
        )
        .await;
    let (delete, _) = world
        .call(Method::DELETE, &uri(missing, "2027-01"), &world.admin, None)
        .await;
    assert_eq!(put, StatusCode::NOT_FOUND);
    assert_eq!(delete, StatusCode::NOT_FOUND);
}

/// A budget is reduced by spending, and a goal has links.
#[tokio::test]
async fn budgets_and_goals_are_refused() {
    let world = settling_world().await;
    let budget = world
        .create(&with(recurring(world.expense), "spending", json!("budget")))
        .await;
    let summer = world.create(&goal(world.expense)).await;
    for id in [&budget, &summer] {
        let audited = world.audit().await.len();

        let (status, answer) = world
            .put_settlement(&world.admin, id, "2027-01", &paid())
            .await;

        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            answer["detail"],
            "only a recurring bill or a one-off outflow can be settled"
        );
        assert_eq!(world.audit().await.len(), audited, "nothing was written");
        assert_eq!(world.months_of(id).await, Vec::<String>::new());
    }
}

#[tokio::test]
async fn income_is_refused() {
    let world = settling_world().await;
    let pay = world.category(CategoryKind::Income).await.1;
    let salary = world
        .create(&with(recurring(pay), "direction", json!("inflow")))
        .await;

    let (status, _) = world
        .put_settlement(&world.admin, &salary, "2027-01", &paid())
        .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn a_one_off_is_settled_for_its_own_month_only() {
    let world = settling_world().await;
    let boiler = world.create(&one_off(world.expense)).await;

    let (other, answer) = world
        .put_settlement(&world.admin, &boiler, "2027-04", &paid())
        .await;
    let (own, _) = world
        .put_settlement(&world.admin, &boiler, "2027-03", &paid())
        .await;

    assert_eq!(other, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(answer["detail"], "the item has no occurrence in that month");
    assert_eq!(own, StatusCode::OK);
}

#[tokio::test]
async fn months_twelve_away_are_taken_and_further_ones_refused() {
    let world = settling_world().await;
    let id = world.create(&long_running(world.expense)).await;
    for month in ["2026-01", "2028-01"] {
        world.settled(&id, month).await;
    }
    let audited = world.audit().await.len();
    for month in ["2025-12", "2028-02", "1969-12", "2201-01"] {
        let (status, _) = world
            .put_settlement(&world.admin, &id, month, &paid())
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }
    assert_eq!(world.audit().await.len(), audited);
    assert_eq!(world.months_of(&id).await, ["2026-01", "2028-01"]);
}

#[tokio::test]
async fn a_month_not_written_yyyy_mm_is_refused() {
    let world = settling_world().await;
    let id = world.create(&long_running(world.expense)).await;
    for month in ["2027-13", "2027-1", "2027-01-15", "soon"] {
        let (status, _) = world
            .put_settlement(&world.admin, &id, month, &paid())
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
}

/// A month that has aged out of the window can't be written, but can still be cleared.
#[tokio::test]
async fn a_settlement_past_the_window_can_still_be_deleted() {
    let world = settling_world().await;
    let id = world.create(&long_running(world.expense)).await;
    world.settled(&id, "2027-01").await;
    world.server.clock.set(at("2029-06-15T12:00:00Z"));

    let (status, _) = world
        .call(Method::DELETE, &uri(&id, "2027-01"), &world.admin, None)
        .await;

    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(world.months_of(&id).await, Vec::<String>::new());
}

#[tokio::test]
async fn deleting_outside_the_plans_years_is_refused() {
    let world = settling_world().await;
    let id = world.create(&long_running(world.expense)).await;
    let (status, _) = world
        .call(Method::DELETE, &uri(&id, "1969-12"), &world.admin, None)
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

/// Sixty fit, a sixty-first doesn't, a month already held may still change, and clearing
/// one makes room.
#[tokio::test]
async fn an_item_holds_at_most_sixty_settlements() {
    let world = settling_world().await;
    let id = world.create(&long_running(world.expense)).await;
    let month_of = |index: i32| format!("{}-{:02}", 2030 + index / 12, index % 12 + 1);
    for index in 0..60 {
        let month = month_of(index);
        world.server.clock.set(at(&format!("{month}-15T12:00:00Z")));
        world.settled(&id, &month).await;
    }
    let extra = month_of(60);
    world.server.clock.set(at(&format!("{extra}-15T12:00:00Z")));

    let (status, answer) = world
        .put_settlement(&world.admin, &id, &extra, &paid())
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(answer["detail"], "an item holds at most 60 settlements");

    let (status, _) = world
        .put_settlement(
            &world.admin,
            &id,
            &month_of(59),
            &json!({"kind": "skipped"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "a month it holds may change");

    let (status, _) = world
        .call(Method::DELETE, &uri(&id, &month_of(0)), &world.admin, None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    world.settled(&id, &extra).await;
}

/// A settled item can't become something that couldn't have been settled: the settlement
/// would be left saying what no longer means anything.
#[tokio::test]
async fn a_settled_bill_cannot_become_a_budget_or_income() {
    let world = settling_world().await;
    let pay = world.category(CategoryKind::Income).await.1;
    let id = world.create(&recurring(world.expense)).await;
    world.settled(&id, "2027-01").await;
    let refused = "the item is settled for some months: delete those settlements first";

    for patch in [
        json!({"spending": "budget"}),
        json!({"direction": "inflow", "category": pay}),
    ] {
        let detail = world.patch_refused(&id, &patch, StatusCode::CONFLICT).await;
        assert_eq!(detail, refused);
    }

    world
        .call(Method::DELETE, &uri(&id, "2027-01"), &world.admin, None)
        .await;
    assert_eq!(
        world.patch(&id, &json!({"spending": "budget"})).await["spending"],
        "budget"
    );
}

/// A name or a category says nothing of what was paid.
#[tokio::test]
async fn a_change_to_anything_else_keeps_the_settlement() {
    let world = settling_world().await;
    let id = world.create(&recurring(world.expense)).await;
    let other = world.category(CategoryKind::Expense).await.1;
    world.settled(&id, "2027-01").await;

    let patched = world
        .patch(
            &id,
            &json!({"name": "Renamed", "category": other, "notes": "Noted", "active": false}),
        )
        .await;

    assert_eq!(patched["settlements"][0]["month"], "2027-01");
    assert_eq!(world.months_of(&id).await, ["2027-01"]);
    let events = world.audit().await;
    assert!(
        !events[0]
            .changes
            .iter()
            .any(|(name, _)| name == "settlements"),
        "nothing was removed"
    );
}

/// The amount and schedule it already has are no change, so nothing goes and nothing is
/// written.
#[tokio::test]
async fn a_patch_that_changes_nothing_keeps_the_settlement() {
    let world = settling_world().await;
    let id = world.create(&recurring(world.expense)).await;
    world.settled(&id, "2027-01").await;
    let before = world.item(&id).await;
    let audited = world.audit().await.len();

    let patched = world
        .patch(
            &id,
            &json!({"amount": before["amount"], "schedule": before["schedule"]}),
        )
        .await;

    assert_eq!(patched["settlements"], before["settlements"]);
    assert_eq!(world.months_of(&id).await, ["2027-01"]);
    assert_eq!(world.audit().await.len(), audited);
}

/// Sending back the amount and schedule it has, beside a real change, is a rename: what was
/// settled still stands.
#[tokio::test]
async fn resending_the_amount_and_schedule_with_a_rename_keeps_the_settlement() {
    let world = settling_world().await;
    let id = world.create(&recurring(world.expense)).await;
    world.settled(&id, "2027-01").await;
    let before = world.item(&id).await;

    let patched = world
        .patch(
            &id,
            &json!({
                "name": "Renamed",
                "amount": before["amount"],
                "schedule": before["schedule"],
            }),
        )
        .await;

    assert_eq!(patched["settlements"], before["settlements"]);
    assert_eq!(world.months_of(&id).await, ["2027-01"]);
}

/// A token short of `all` at `write` gets the same refusal for a hidden item and a missing
/// one, and its patch removes nothing.
#[tokio::test]
async fn restricted_token_cannot_end_a_settlement() {
    let world = settling_world().await;
    let (visible, hidden) = (
        world.account("Visible").await,
        world.account("Hidden").await,
    );
    let id = world
        .create(&with(
            recurring(world.expense),
            "account_hint",
            json!(hidden),
        ))
        .await;
    world.settled(&id, "2027-01").await;
    let (_, token) = world.token(Scope::Write, vec![on(visible, Write)]).await;
    let audited = world.audit().await.len();
    let patch = json!({"amount": {"minor_units": 5000, "currency": "USD"}});

    let answer = world
        .send(Method::PATCH, &item_uri(&id), &token, Some(&patch), None)
        .await;
    let absent = world
        .send(
            Method::PATCH,
            &item_uri(PlannedItemId::new()),
            &token,
            Some(&patch),
            None,
        )
        .await;

    assert_eq!(answer.0, StatusCode::FORBIDDEN);
    assert!(answer == absent, "byte-identical to a random id's");
    assert_eq!(world.months_of(&id).await, ["2027-01"]);
    assert_eq!(world.audit().await.len(), audited);
}

/// Replacing a month's settlement says who replaced it and when, not who first settled it.
#[tokio::test]
async fn replacing_a_settlement_records_the_new_who_and_when() {
    let world = settling_world().await;
    let id = world.create(&recurring(world.expense)).await;
    world.settled(&id, "2027-01").await;
    let (bob, bobs_token_id, bobs_token) = world.second_member().await;
    world.server.clock.set(at("2027-01-20T12:00:00Z"));

    let (status, _) = world
        .put_settlement(&bobs_token, &id, "2027-01", &json!({"kind": "skipped"}))
        .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        world.item(&id).await["settlements"],
        json!([{
            "month": "2027-01",
            "kind": "skipped",
            "member": bob,
            "agent": bobs_token_id,
            "settled_at": "2027-01-20T12:00:00Z",
        }])
    );
}

/// The refusal comes before the month's check on a delete too, so it says nothing of it.
#[tokio::test]
async fn restricted_token_is_refused_a_delete_before_the_month_is_checked() {
    let world = settling_world().await;
    let visible = world.account("Visible").await;
    let id = world.create(&recurring(world.expense)).await;
    let (_, token) = world.token(Scope::Write, vec![on(visible, Write)]).await;

    let (status, _) = world
        .call(Method::DELETE, &uri(&id, "1969-12"), &token, None)
        .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// What was removed and who removed it, member and agent.
#[tokio::test]
async fn a_delete_is_audited_with_its_kind_member_and_agent() {
    let world = settling_world().await;
    let id = world.create(&recurring(world.expense)).await;
    world
        .put_settlement(&world.admin, &id, "2027-01", &json!({"kind": "skipped"}))
        .await;
    let (token_id, token) = world.token(Scope::Write, Vec::new()).await;

    world
        .call(Method::DELETE, &uri(&id, "2027-01"), &token, None)
        .await;

    let events = world.audit().await;
    assert_eq!(events[0].action, "planned_item.unsettle");
    assert_eq!(
        events[0].actor,
        AuditActor::Member {
            member: world.alice.member,
            agent: Some(token_id),
        }
    );
    let before = |field: &str| {
        events[0]
            .changes
            .iter()
            .find(|(name, _)| *name == field)
            .unwrap()
            .1
            .before
            .clone()
    };
    assert_eq!(before("month"), AuditValue::Text("2027-01".into()));
    assert_eq!(before("kind"), AuditValue::Text("skipped".into()));
}

/// All of it for a hidden item and a random id: the same bytes, no audit entry and nothing
/// under the key.
#[tokio::test]
async fn restricted_token_is_refused_alike_for_hidden_and_missing() {
    let world = settling_world().await;
    let (visible, hidden) = (
        world.account("Visible").await,
        world.account("Hidden").await,
    );
    let on_hidden = world
        .create(&with(
            recurring(world.expense),
            "account_hint",
            json!(hidden),
        ))
        .await;
    let missing = PlannedItemId::new().to_string();
    for grants in [
        vec![on(visible, Write)],
        vec![all(Transactions), on(visible, Write)],
    ] {
        let (token_id, token) = world.token(Scope::Write, grants).await;
        let audited = world.audit().await.len();
        let body = paid();
        for (method, body, key) in [
            (Method::PUT, Some(&body), "put-key"),
            (Method::DELETE, None, "delete-key"),
        ] {
            let answer = world
                .send(
                    method.clone(),
                    &uri(&on_hidden, "2027-01"),
                    &token,
                    body,
                    Some(key),
                )
                .await;
            let absent = world
                .send(method, &uri(&missing, "2027-01"), &token, body, Some(key))
                .await;
            assert_eq!(answer.0, StatusCode::FORBIDDEN);
            assert!(answer == absent, "byte-identical to a random id's");
            assert!(!answer.1.contains(&on_hidden), "the body names nothing");
            assert!(world.key_is_unused(token_id, key).await, "no key was taken");
        }
        assert_eq!(world.audit().await.len(), audited, "no audit entry");
        assert_eq!(world.months_of(&on_hidden).await, Vec::<String>::new());
    }
}

/// The refusal comes before the month's checks, so it says nothing of them either.
#[tokio::test]
async fn restricted_token_is_refused_before_the_month_is_checked() {
    let world = settling_world().await;
    let visible = world.account("Visible").await;
    let id = world.create(&recurring(world.expense)).await;
    let (_, token) = world.token(Scope::Write, vec![on(visible, Write)]).await;

    let (status, _) = world.put_settlement(&token, &id, "2040-01", &paid()).await;

    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn read_token_cannot_settle() {
    let world = settling_world().await;
    let id = world.create(&recurring(world.expense)).await;
    let (_, token) = world.token(Scope::Read, Vec::new()).await;

    let (put, answer) = world.put_settlement(&token, &id, "2027-01", &paid()).await;
    let (delete, _) = world
        .call(Method::DELETE, &uri(&id, "2027-01"), &token, None)
        .await;

    assert_eq!(put, StatusCode::FORBIDDEN);
    assert_eq!(answer["detail"], "this action requires write rights");
    assert_eq!(delete, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn all_at_write_may_settle() {
    let world = settling_world().await;
    let hidden_from_none = world.account("Checking").await;
    let id = world
        .create(&with(
            recurring(world.expense),
            "account_hint",
            json!(hidden_from_none),
        ))
        .await;
    let (_, token) = world.token(Scope::Write, vec![all(Write)]).await;

    let (status, _) = world.put_settlement(&token, &id, "2027-01", &paid()).await;

    assert_eq!(status, StatusCode::OK);
}

/// Settling doesn't lift a hint: a reader who can't see the item learns no more of it.
#[tokio::test]
async fn a_settled_item_stays_hidden() {
    let world = settling_world().await;
    let (visible, hidden) = (
        world.account("Visible").await,
        world.account("Hidden").await,
    );
    let id = world
        .create(&with(
            recurring(world.expense),
            "account_hint",
            json!(hidden),
        ))
        .await;
    world.settled(&id, "2027-01").await;
    let (_, token) = world
        .token(Scope::Read, vec![on(visible, Transactions)])
        .await;

    let answer = world
        .send(Method::GET, &item_uri(&id), &token, None, None)
        .await;
    let absent = world
        .send(
            Method::GET,
            &item_uri(PlannedItemId::new()),
            &token,
            None,
            None,
        )
        .await;

    assert_eq!(answer.0, StatusCode::NOT_FOUND);
    assert!(answer == absent, "byte-identical to a random id's");
}

#[tokio::test]
async fn deleting_an_item_removes_its_settlements() {
    let world = settling_world().await;
    let id = world.create(&recurring(world.expense)).await;
    let kept = world.create(&recurring(world.expense)).await;
    world.settled(&id, "2027-01").await;
    world.settled(&kept, "2027-01").await;

    let (status, _) = world
        .call(Method::DELETE, &item_uri(&id), &world.admin, None)
        .await;

    assert_eq!(status, StatusCode::NO_CONTENT);
    let left = world.server.store.settlements().await.unwrap();
    let items: Vec<String> = left.iter().map(|one| one.item.to_string()).collect();
    assert_eq!(items, [kept]);
}

/// The audit log keeps what a deleted item held, its settlements too.
#[tokio::test]
async fn deleting_a_settled_item_audits_its_settlements() {
    let world = settling_world().await;
    let id = world.create(&recurring(world.expense)).await;
    world.settled(&id, "2027-01").await;

    world
        .call(Method::DELETE, &item_uri(&id), &world.admin, None)
        .await;

    let events = world.audit().await;
    assert_eq!(events[0].action, "planned_item.delete");
    let settlements = events[0]
        .changes
        .iter()
        .find(|(name, _)| *name == "settlements")
        .unwrap()
        .1;
    assert_eq!(
        settlements.before,
        AuditValue::Text(r#"[["2027-01","paid"]]"#.into())
    );
}
