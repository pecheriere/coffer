//! Creating, reading, changing and deleting planned items, and their audit.

use axum::http::{Method, StatusCode};
use coffer_domain::audit::{AuditActor, AuditValue, FieldChange};
use coffer_domain::identity::Scope;
use coffer_domain::transactions::CategoryKind;
use coffer_server::ledger::plan_items::{PlannedItemInput, PlannedItemPatch};
use coffer_store::AuditEvent;
use serde_json::{Value, json};

use super::{ITEMS, goal, item_uri, one_off, recurring, with, world};

fn change<'a>(event: &'a AuditEvent, field: &str) -> &'a FieldChange {
    event
        .changes
        .iter()
        .find_map(|(name, change)| (name == field).then_some(change))
        .unwrap()
}

fn text(value: &str) -> AuditValue {
    AuditValue::Text(value.to_owned())
}

#[tokio::test]
async fn create_each_type_and_read_it_back() {
    let world = world().await;
    let account = world.account("Checking").await;
    let income = world.category(CategoryKind::Income).await.1;
    let cases = [
        (
            with(
                recurring(world.expense),
                "schedule",
                json!({"type": "recurring", "every": 3, "unit": "weeks",
                       "anchor_on": "2027-01-31", "end_on": "2028-01-31"}),
            ),
            json!({"type": "recurring", "every": 3, "unit": "weeks",
                   "anchor_on": "2027-01-31", "end_on": "2028-01-31"}),
        ),
        (
            recurring(world.expense),
            json!({"type": "recurring", "every": 1, "unit": "months",
                   "anchor_on": "2027-01-01", "end_on": null}),
        ),
        (
            with(
                with(one_off(income), "direction", json!("inflow")),
                "account_hint",
                json!(account),
            ),
            json!({"type": "one_off", "on": "2027-03-15"}),
        ),
        (
            with(goal(world.expense), "notes", json!("first\nsecond")),
            json!({"type": "goal", "start_on": "2027-01-01", "due_on": "2027-07-01",
                   "completed": false}),
        ),
    ];
    let mut ids = Vec::new();
    for (body, schedule) in &cases {
        let (status, created) = world
            .call(Method::POST, ITEMS, &world.admin, Some(body))
            .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(created["name"], body["name"]);
        assert_eq!(created["category"], body["category"]);
        assert_eq!(created["amount"], body["amount"]);
        assert_eq!(created["direction"], body["direction"]);
        assert_eq!(created["schedule"], *schedule, "the schedule reads back");
        assert_eq!(created["active"], json!(true), "active by default");
        for (field, sent) in [
            ("account_hint", &body["account_hint"]),
            ("notes", &body["notes"]),
        ] {
            assert!(created.get(field).is_some(), "the field is always present");
            assert_eq!(created[field], *sent);
        }
        assert!(created["created_at"].is_string());
        assert_eq!(created["created_at"], created["updated_at"]);

        let id = created["id"].as_str().unwrap().to_owned();
        let (status, read) = world
            .call(Method::GET, &item_uri(&id), &world.admin, None)
            .await;
        assert_eq!(status, StatusCode::OK);
        assert!(read == created, "a read answers what the create did");
        ids.push(id);
    }
    assert!(
        world.listed(ITEMS, &world.admin).await == ids,
        "the list holds them in id order"
    );
}

#[tokio::test]
async fn list_hides_inactive_unless_asked() {
    let world = world().await;
    let active = world.create(&recurring(world.expense)).await;
    let inactive = world
        .create(&with(one_off(world.expense), "active", json!(false)))
        .await;
    let also_active = world
        .create(&with(goal(world.expense), "active", Value::Null))
        .await;

    let plain = world.listed(ITEMS, &world.admin).await;
    assert!(
        plain == [active.clone(), also_active.clone()],
        "inactive is left out"
    );
    let off = format!("{ITEMS}?include_inactive=false");
    assert!(
        world.listed(&off, &world.admin).await == plain,
        "false is the default"
    );
    let on = format!("{ITEMS}?include_inactive=true");
    assert!(
        world.listed(&on, &world.admin).await == [active, inactive, also_active],
        "asked for, it is listed"
    );
}

#[tokio::test]
async fn patch_changes_only_what_was_sent() {
    let world = world().await;
    let account = world.account("Checking").await;
    let other_category = world.category(CategoryKind::Expense).await.1;
    let body = with(
        with(recurring(world.expense), "account_hint", json!(account)),
        "notes",
        json!("Keep these"),
    );
    let id = world.create(&body).await;
    let (_, before) = world
        .call(Method::GET, &item_uri(&id), &world.admin, None)
        .await;

    // Absent keeps; a value changes that field and no other.
    let after = world.patch(&id, &json!({"name": "  Sewer bill  "})).await;
    assert_eq!(after["name"], json!("Sewer bill"), "the name is trimmed");
    for field in [
        "spending",
        "category",
        "amount",
        "direction",
        "account_hint",
        "notes",
        "active",
        "schedule",
        "id",
        "created_at",
    ] {
        assert_eq!(after[field], before[field], "an absent field stays");
    }

    let after = world
        .patch(
            &id,
            &json!({
                "category": other_category,
                "amount": {"minor_units": 5000, "currency": "EUR"},
                "active": false,
                "schedule": {"type": "recurring", "every": 2, "unit": "years",
                             "anchor_on": "2027-06-30"},
            }),
        )
        .await;
    assert_eq!(after["category"], json!(other_category));
    assert_eq!(
        after["amount"],
        json!({"minor_units": 5000, "currency": "EUR"})
    );
    assert_eq!(after["active"], json!(false));
    assert_eq!(
        after["schedule"],
        json!({"type": "recurring", "every": 2, "unit": "years",
               "anchor_on": "2027-06-30", "end_on": null})
    );
    assert_eq!(
        after["account_hint"],
        json!(account),
        "absent keeps the hint"
    );
    assert_eq!(
        after["notes"],
        json!("Keep these"),
        "absent keeps the notes"
    );

    // `null` clears; `""` clears notes, as on a transaction.
    let after = world.patch(&id, &json!({"account_hint": null})).await;
    assert_eq!(after["account_hint"], Value::Null, "null clears the hint");
    assert_eq!(after["notes"], json!("Keep these"));
    let after = world.patch(&id, &json!({"notes": ""})).await;
    assert_eq!(after["notes"], Value::Null, "empty notes are none");
    let after = world.patch(&id, &json!({"notes": "Again"})).await;
    assert_eq!(after["notes"], json!("Again"));
    let after = world.patch(&id, &json!({"notes": null})).await;
    assert_eq!(after["notes"], Value::Null, "null clears the notes");
    let after = world.patch(&id, &json!({"account_hint": account})).await;
    assert_eq!(
        after["account_hint"],
        json!(account),
        "a value sets the hint"
    );

    let (_, read) = world
        .call(Method::GET, &item_uri(&id), &world.admin, None)
        .await;
    assert!(read == after, "the change was stored");
}

#[tokio::test]
async fn empty_notes_are_stored_as_none() {
    let world = world().await;
    let id = world
        .create(&with(recurring(world.expense), "notes", json!("")))
        .await;
    let (_, read) = world
        .call(Method::GET, &item_uri(&id), &world.admin, None)
        .await;
    assert_eq!(read["notes"], Value::Null, "as a transaction's are");
}

#[tokio::test]
async fn noop_patch_writes_no_audit() {
    let world = world().await;
    let account = world.account("Checking").await;
    let body = with(
        with(recurring(world.expense), "account_hint", json!(account)),
        "notes",
        json!("Same"),
    );
    let id = world.create(&body).await;
    let (_, before) = world
        .call(Method::GET, &item_uri(&id), &world.admin, None)
        .await;
    let audited = world.audit().await.len();

    let same = json!({
        "name": " Water bill ",
        "category": before["category"],
        "amount": before["amount"],
        "direction": before["direction"],
        "account_hint": before["account_hint"],
        "notes": before["notes"],
        "active": before["active"],
        "schedule": before["schedule"],
        "spending": before["spending"],
    });
    for patch in [json!({}), same] {
        let after = world.patch(&id, &patch).await;
        assert!(after == before, "nothing changed, updated_at included");
    }
    assert_eq!(world.audit().await.len(), audited, "no audit entry");

    // Clearing what is already clear changes nothing either.
    let bare = world.create(&recurring(world.expense)).await;
    let audited = world.audit().await.len();
    for patch in [
        json!({"account_hint": null}),
        json!({"notes": null}),
        json!({"notes": ""}),
    ] {
        world.patch(&bare, &patch).await;
    }
    assert_eq!(world.audit().await.len(), audited, "no audit entry");
}

#[tokio::test]
async fn create_and_update_are_audited_field_by_field() {
    let world = world().await;
    let account = world.account("Checking").await;
    let body = with(
        with(recurring(world.expense), "account_hint", json!(account)),
        "notes",
        json!("Remember"),
    );
    let id = world.create(&body).await;

    let events = world.audit().await;
    assert_eq!(events.len(), 1);
    let created = &events[0];
    assert_eq!(created.action, "planned_item.create");
    assert_eq!(created.entity_id, id);
    let after = |field| change(created, field).after.clone();
    assert_eq!(after("name"), text("Water bill"));
    assert_eq!(after("category"), text(&world.expense.to_string()));
    assert_eq!(after("amount_minor"), AuditValue::Int(4200));
    assert_eq!(after("currency"), text("USD"));
    assert_eq!(after("direction"), text("outflow"));
    assert_eq!(after("account_hint"), text(&account.to_string()));
    assert_eq!(after("notes"), text("Remember"));
    assert_eq!(after("active"), AuditValue::Bool(true));
    assert_eq!(after("spending"), text("bill"));
    let AuditValue::Text(schedule) = after("schedule") else {
        panic!("the schedule is recorded as text");
    };
    assert_eq!(
        serde_json::from_str::<Value>(&schedule).unwrap(),
        json!({"type": "recurring", "every": 1, "unit": "months",
               "anchor_on": "2027-01-01", "end_on": null})
    );

    world
        .patch(
            &id,
            &json!({"name": "Sewer bill",
                    "amount": {"minor_units": 1, "currency": "EUR"},
                    "account_hint": null}),
        )
        .await;
    let events = world.audit().await;
    assert_eq!(events.len(), 2);
    let updated = events
        .iter()
        .find(|event| event.action == "planned_item.update")
        .unwrap();
    assert_eq!(updated.entity_type, "planned_item");
    assert_eq!(updated.entity_id, id);
    let mut fields: Vec<_> = updated.changes.iter().map(|(field, _)| field).collect();
    fields.sort_unstable();
    assert_eq!(
        fields,
        ["account_hint", "amount_minor", "currency", "name"],
        "only the changed fields"
    );
    let name = change(updated, "name");
    assert_eq!(name.before, text("Water bill"));
    assert_eq!(name.after, text("Sewer bill"));
    assert_eq!(change(updated, "account_hint").after, AuditValue::Null);
}

#[tokio::test]
async fn spending_defaults_to_bill_on_a_recurring_outflow_only() {
    let world = world().await;
    let income = world.category(CategoryKind::Income).await.1;
    let cases = [
        (recurring(world.expense), json!("bill")),
        (
            with(recurring(world.expense), "spending", Value::Null),
            json!("bill"),
        ),
        (
            with(recurring(world.expense), "spending", json!("bill")),
            json!("bill"),
        ),
        (
            with(recurring(world.expense), "spending", json!("budget")),
            json!("budget"),
        ),
        (one_off(world.expense), Value::Null),
        (goal(world.expense), Value::Null),
        (
            with(recurring(income), "direction", json!("inflow")),
            Value::Null,
        ),
    ];
    for (body, expected) in cases {
        let id = world.create(&body).await;
        let (_, read) = world
            .call(Method::GET, &item_uri(&id), &world.admin, None)
            .await;
        assert!(
            read.get("spending").is_some(),
            "the field is always present"
        );
        assert_eq!(read["spending"], expected);
    }
}

#[tokio::test]
async fn spending_is_patchable_and_audited() {
    let world = world().await;
    let id = world
        .create(&with(recurring(world.expense), "spending", json!("budget")))
        .await;

    // Absent or null keeps it, like any field that can't be cleared.
    let after = world.patch(&id, &json!({"name": "Groceries"})).await;
    assert_eq!(after["spending"], json!("budget"));
    let after = world.patch(&id, &json!({"spending": null})).await;
    assert_eq!(after["spending"], json!("budget"));

    let audited = world.audit().await.len();
    let after = world.patch(&id, &json!({"spending": "budget"})).await;
    assert_eq!(after["spending"], json!("budget"));
    assert_eq!(
        world.audit().await.len(),
        audited,
        "the same value is no change"
    );

    let after = world.patch(&id, &json!({"spending": "bill"})).await;
    assert_eq!(after["spending"], json!("bill"));
    let events = world.audit().await;
    assert_eq!(events.len(), audited + 1);
    let latest = events
        .iter()
        .find(|event| {
            event.action == "planned_item.update"
                && event.changes.iter().any(|(field, _)| field == "spending")
        })
        .unwrap();
    let fields: Vec<_> = latest.changes.iter().map(|(field, _)| field).collect();
    assert_eq!(fields, ["spending"], "only the changed field");
    assert_eq!(change(latest, "spending").before, text("budget"));
    assert_eq!(change(latest, "spending").after, text("bill"));

    world.patch(&id, &json!({"spending": "budget"})).await;
    let (status, _) = world
        .call(Method::DELETE, &item_uri(&id), &world.admin, None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let events = world.audit().await;
    let deleted = events
        .iter()
        .find(|event| event.action == "planned_item.delete")
        .unwrap();
    assert_eq!(change(deleted, "spending").before, text("budget"));
}

#[tokio::test]
async fn changing_direction_keeps_spending_in_step() {
    let world = world().await;
    let income = world.category(CategoryKind::Income).await.1;
    let id = world
        .create(&with(recurring(world.expense), "spending", json!("budget")))
        .await;

    // An inflow has no spending kind, so it goes, and the audit says so.
    let after = world
        .patch(&id, &json!({"direction": "inflow", "category": income}))
        .await;
    assert_eq!(after["spending"], Value::Null);
    let events = world.audit().await;
    let cleared = events
        .iter()
        .find(|event| event.changes.iter().any(|(field, _)| field == "direction"))
        .unwrap();
    assert_eq!(change(cleared, "spending").before, text("budget"));
    assert_eq!(change(cleared, "spending").after, AuditValue::Null);

    // Back to an outflow it takes the default, not what it once held.
    let after = world
        .patch(
            &id,
            &json!({"direction": "outflow", "category": world.expense}),
        )
        .await;
    assert_eq!(after["spending"], json!("bill"));
}

#[tokio::test]
async fn delete_is_audited_with_before_values() {
    let world = world().await;
    let account = world.account("Checking").await;
    let body = with(
        with(goal(world.expense), "account_hint", json!(account)),
        "notes",
        json!("Remember"),
    );
    let id = world.create(&body).await;
    let uri = item_uri(&id);

    let (status, text_body) = world
        .send(Method::DELETE, &uri, &world.admin, None, None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(text_body.is_empty(), "204 has no body");

    let (status, _) = world.call(Method::GET, &uri, &world.admin, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = world.call(Method::DELETE, &uri, &world.admin, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "it is already gone");

    let events = world.audit().await;
    assert_eq!(events.len(), 2, "the second delete wrote nothing");
    let deleted = events
        .iter()
        .find(|event| event.action == "planned_item.delete")
        .unwrap();
    assert_eq!(deleted.entity_type, "planned_item");
    assert_eq!(deleted.entity_id, id);
    let before = |field| change(deleted, field).before.clone();
    assert_eq!(before("name"), text("Summer trip"));
    assert_eq!(before("category"), text(&world.expense.to_string()));
    assert_eq!(before("amount_minor"), AuditValue::Int(300_000));
    assert_eq!(before("currency"), text("USD"));
    assert_eq!(before("direction"), text("outflow"));
    assert_eq!(before("account_hint"), text(&account.to_string()));
    assert_eq!(before("notes"), text("Remember"));
    assert_eq!(before("active"), AuditValue::Bool(true));
    assert_eq!(before("spending"), AuditValue::Null, "a goal has none");
    let AuditValue::Text(schedule) = before("schedule") else {
        panic!("the schedule is recorded as text");
    };
    assert_eq!(
        serde_json::from_str::<Value>(&schedule).unwrap(),
        json!({"type": "goal", "start_on": "2027-01-01", "due_on": "2027-07-01",
               "completed": false})
    );
    assert!(
        deleted
            .changes
            .iter()
            .all(|(_, change)| change.after == AuditValue::Null),
        "nothing remains"
    );
}

#[tokio::test]
async fn create_is_idempotent() {
    let world = world().await;
    let body = recurring(world.expense);

    let post = async |body: &Value| {
        world
            .send(Method::POST, ITEMS, &world.admin, Some(body), Some("key-1"))
            .await
    };
    let first = post(&body).await;
    assert_eq!(first.0, StatusCode::CREATED);
    assert!(
        post(&body).await == first,
        "the same key replays the response"
    );
    assert_eq!(world.listed(ITEMS, &world.admin).await.len(), 1);
    assert_eq!(world.audit().await.len(), 1, "the replay audited nothing");

    let (status, answer) = post(&one_off(world.expense)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let answer: Value = serde_json::from_str(&answer).unwrap();
    assert_eq!(
        answer["type"],
        json!("urn:coffer:problem:idempotency_key_reuse")
    );
    assert_eq!(world.listed(ITEMS, &world.admin).await.len(), 1);
}

#[tokio::test]
async fn refused_create_releases_its_key() {
    let world = world().await;
    let refused = with(recurring(world.expense), "name", json!(""));
    let (status, _) = world
        .send(
            Method::POST,
            ITEMS,
            &world.admin,
            Some(&refused),
            Some("key-1"),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let fixed = recurring(world.expense);
    let (status, _) = world
        .send(
            Method::POST,
            ITEMS,
            &world.admin,
            Some(&fixed),
            Some("key-1"),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "the key is free again");
}

#[tokio::test]
async fn every_change_attributes_member_and_agent() {
    let world = world().await;
    let (token_id, token) = world.token(Scope::Write, Vec::new()).await;
    let by_agent = AuditActor::Member {
        member: world.alice.member,
        agent: Some(token_id),
    };
    let by_session = AuditActor::Member {
        member: world.alice.member,
        agent: None,
    };

    // Through a token.
    let id = world.create_with(&token, &recurring(world.expense)).await;
    let uri = item_uri(&id);
    let patch = json!({"name": "Sewer bill"});
    let (status, _) = world.call(Method::PATCH, &uri, &token, Some(&patch)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = world.call(Method::DELETE, &uri, &token, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let events = world.audit().await;
    assert_eq!(events.len(), 3);
    assert!(
        events.iter().all(|event| event.actor == by_agent),
        "the agent is named"
    );

    // Through a passkey session.
    let input: PlannedItemInput = serde_json::from_value(recurring(world.expense)).unwrap();
    let view = world
        .server
        .ledger
        .create_planned_item(&world.alice, input, None)
        .await
        .unwrap();
    let patch: PlannedItemPatch = serde_json::from_value(patch).unwrap();
    world
        .server
        .ledger
        .update_planned_item(&world.alice, view.id, patch)
        .await
        .unwrap();
    world
        .server
        .ledger
        .delete_planned_item(&world.alice, view.id)
        .await
        .unwrap();
    let events = world.audit().await;
    assert_eq!(events.len(), 6);
    assert_eq!(
        events
            .iter()
            .filter(|event| event.actor == by_session)
            .count(),
        3,
        "the session's changes name no agent"
    );
}
