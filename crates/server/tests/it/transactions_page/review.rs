//! Reviewing: one at a time, filed in a category, in a batch, and editing the payee, notes and
//! assignee. Each change is read back through the API and is in the audit log as the person's.

use axum::http::StatusCode;
use coffer_domain::audit::AuditValue;
use jiff::civil::date;
use serde_json::json;

use super::{LIST, assert_redirect, assert_refused, detail, member, signed_in, world};

#[tokio::test]
async fn marking_one_reviewed_records_who_and_putting_it_back_forgets() {
    let mut world = world().await;
    let id = world.manual(-1_250, "Bakery", date(2026, 10, 2)).await;
    let path = format!("{}/review", detail(id));

    let reply = world.post(&path, &[("review", "reviewed")]).await;
    assert_redirect(&reply, &detail(id));
    let read = world.read(id).await;
    assert_eq!(read["review"], "reviewed");
    assert_eq!(
        read["reviewed_by"]["member"],
        world.alice.member.to_string()
    );
    let page = world.page(&detail(id)).await;
    assert!(page.body.contains("Reviewed by Alice."));
    let event = &world.audit(id).await[0];
    assert_eq!(event.action, "transaction.update");
    assert_eq!(event.actor, signed_in(&world.alice));

    let reply = world.post(&path, &[("review", "unreviewed")]).await;
    assert_redirect(&reply, &detail(id));
    let read = world.read(id).await;
    assert_eq!(read["review"], "unreviewed");
    assert_eq!(read["reviewed_by"], json!(null));
}

#[tokio::test]
async fn filing_in_one_category_replaces_the_parts_and_can_review_with_it() {
    let mut world = world().await;
    let groceries = world.category("Groceries").await;
    let id = world.manual(-1_250, "Bakery", date(2026, 10, 2)).await;
    let path = format!("{}/category", detail(id));

    let reply = world
        .post(
            &path,
            &[("category", &groceries.to_string()), ("reviewed", "yes")],
        )
        .await;
    assert_redirect(&reply, &detail(id));
    let read = world.read(id).await;
    assert_eq!(read["splits"][0]["category"], groceries.to_string());
    assert_eq!(read["splits"][0]["amount"]["minor_units"], -1_250);
    assert_eq!(read["review"], "reviewed");
    assert_eq!(
        read["categorized_by"]["member"],
        world.alice.member.to_string()
    );

    let reply = world.post(&path, &[("category", "none")]).await;
    assert_redirect(&reply, &detail(id));
    assert_eq!(world.read(id).await["splits"], json!([]));

    let reply = world.post(&path, &[("category", "")]).await;
    assert_refused(
        &reply,
        StatusCode::UNPROCESSABLE_ENTITY,
        "choose a category",
        "xyzzy",
    );
    let reply = world.post(&path, &[("category", "xyzzy-category")]).await;
    assert_refused(
        &reply,
        StatusCode::UNPROCESSABLE_ENTITY,
        "not a category the form offers",
        "xyzzy",
    );
}

/// A browser sends a textarea's line breaks as CR LF, and its parser drops one line break
/// straight after the tag: notes that start with one are drawn with another before them, so
/// saving the form unchanged keeps them.
#[tokio::test]
async fn notes_keep_their_line_breaks_through_the_form() {
    let mut world = world().await;
    let id = world.manual(-900, "Kiosk", date(2026, 10, 1)).await;
    let path = format!("{}/details", detail(id));
    let reply = world
        .post(
            &path,
            &[
                ("payee", "Kiosk"),
                ("notes", "Line one\r\nLine two"),
                ("assignee", ""),
            ],
        )
        .await;
    assert_redirect(&reply, &detail(id));
    assert_eq!(world.read(id).await["notes"], "Line one\nLine two");

    let reply = world
        .post(
            &path,
            &[
                ("payee", "Kiosk"),
                ("notes", "\nIndented"),
                ("assignee", ""),
            ],
        )
        .await;
    assert_redirect(&reply, &detail(id));
    let page = world.page(&detail(id)).await;
    assert!(page.body.contains("rows=\"3\">\n\nIndented</textarea>"));
}

#[tokio::test]
async fn the_payee_notes_and_assignee_change_and_only_what_differs_is_sent() {
    let mut world = world().await;
    let (_, bob) = member(&world).await;
    let checking = super::account(&world.server, &world.alice, "Checking", "USD").await;
    let id = world.imported(checking, -900, "Corner shop").await;
    let path = format!("{}/details", detail(id));

    let reply = world
        .post(
            &path,
            &[
                ("payee", "Corner shop"),
                ("notes", "Milk\r\nand bread"),
                ("assignee", &bob.member.to_string()),
            ],
        )
        .await;
    assert_redirect(&reply, &detail(id));
    let read = world.read(id).await;
    assert_eq!(read["notes"], "Milk\nand bread");
    assert_eq!(read["assignee"], bob.member.to_string());
    let event = &world.audit(id).await[0];
    let fields: Vec<&str> = event.changes.iter().map(|(name, _)| name).collect();
    assert_eq!(
        fields,
        ["assignee", "notes"],
        "the payee shown was sent back unchanged"
    );

    let reply = world
        .post(
            &path,
            &[
                ("payee", "The corner shop"),
                ("notes", ""),
                ("assignee", ""),
            ],
        )
        .await;
    assert_redirect(&reply, &detail(id));
    let read = world.read(id).await;
    assert_eq!(read["payee"], "The corner shop");
    assert_eq!(read["notes"], json!(null));
    assert_eq!(read["assignee"], json!(null));

    let reply = world
        .post(&path, &[("payee", ""), ("notes", ""), ("assignee", "")])
        .await;
    assert_redirect(&reply, &detail(id));
    assert_eq!(
        world.read(id).await["payee"],
        "Corner shop",
        "back to the bank's"
    );

    let long = format!("xyzzy{}", "a".repeat(200));
    let reply = world
        .post(&path, &[("payee", &long), ("notes", ""), ("assignee", "")])
        .await;
    assert_refused(
        &reply,
        StatusCode::UNPROCESSABLE_ENTITY,
        "payee must be 1 to 200 characters",
        "xyzzy",
    );
}

#[tokio::test]
async fn a_batch_marks_every_ticked_one_and_goes_back_to_the_same_list() {
    let mut world = world().await;
    let first = world.manual(-100, "Kiosk", date(2026, 10, 1)).await;
    let second = world.manual(-200, "Market", date(2026, 10, 2)).await;
    let left = world.manual(-300, "Garage", date(2026, 10, 3)).await;
    let queue = world.page("/transactions?review=unreviewed").await;
    assert!(
        queue
            .body
            .contains("name=\"back\" value=\"review=unreviewed\"")
    );

    let reply = world
        .post(
            "/transactions/batch",
            &[
                ("review", "reviewed"),
                ("back", "review=unreviewed"),
                (&format!("pick_{first}"), "yes"),
                (&format!("pick_{second}"), "yes"),
            ],
        )
        .await;
    assert_redirect(&reply, "/transactions?review=unreviewed");
    assert_eq!(world.read(first).await["review"], "reviewed");
    assert_eq!(world.read(second).await["review"], "reviewed");
    assert_eq!(world.read(left).await["review"], "unreviewed");
    assert_eq!(world.audit(first).await[0].actor, signed_in(&world.alice));
    let queue = world.page("/transactions?review=unreviewed").await;
    assert!(queue.body.contains("Garage"));
    assert!(!queue.body.contains("Kiosk"));
}

#[tokio::test]
async fn a_batch_is_all_or_nothing_and_refuses_what_its_form_does_not_hold() {
    let mut world = world().await;
    let id = world.manual(-100, "Kiosk", date(2026, 10, 1)).await;
    let missing = coffer_domain::ids::TransactionId::new();
    let pick = format!("pick_{id}");
    let gone = format!("pick_{missing}");

    let reply = world
        .post(
            "/transactions/batch",
            &[
                ("review", "reviewed"),
                ("back", ""),
                (&pick, "yes"),
                (&gone, "yes"),
            ],
        )
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(world.read(id).await["review"], "unreviewed", "none applied");

    for fields in [
        vec![("review", "reviewed"), ("back", "")],
        vec![
            ("review", "reviewed"),
            ("back", ""),
            (pick.as_str(), "yes"),
            ("xyzzy", "1"),
        ],
        vec![
            ("review", "xyzzy-review"),
            ("back", ""),
            (pick.as_str(), "yes"),
        ],
        vec![
            ("review", "reviewed"),
            ("back", "xyzzy=1"),
            (pick.as_str(), "yes"),
        ],
    ] {
        let reply = world.post("/transactions/batch", &fields).await;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(!reply.body.contains("xyzzy"));
    }
    assert_eq!(world.read(id).await["review"], "unreviewed");
}

#[tokio::test]
async fn every_form_needs_the_sessions_token_and_refuses_unknown_fields() {
    let mut world = world().await;
    let id = world.manual(-100, "Kiosk", date(2026, 10, 1)).await;
    let base = detail(id);
    let forms = [
        (format!("{base}/review"), vec![("review", "reviewed")]),
        (format!("{base}/category"), vec![("category", "none")]),
        (format!("{base}/splits"), vec![]),
        (
            format!("{base}/details"),
            vec![("payee", "x"), ("notes", ""), ("assignee", "")],
        ),
        (format!("{base}/delete"), vec![]),
        (LIST.to_owned(), vec![("account", "x")]),
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
    let read = world.read(id).await;
    assert_eq!(read["review"], "unreviewed");
    assert_eq!(read["payee"], "Kiosk");
    assert!(
        world
            .audit(id)
            .await
            .iter()
            .all(|event| event.action == "transaction.create")
    );
}

/// A member does on these pages what the API lets a member do: here, everything an admin does
/// to a transaction, since a session reaches every account.
#[tokio::test]
async fn a_member_reviews_and_files_as_themselves() {
    let mut world = world().await;
    let groceries = world.category("Groceries").await;
    let id = world.manual(-1_250, "Bakery", date(2026, 10, 2)).await;
    let (mut bob, bob_actor) = member(&world).await;

    let page = bob.get(&detail(id)).await;
    assert_eq!(page.status, StatusCode::OK);
    let csrf = page.csrf();
    let reply = bob
        .post_form(
            &format!("{}/category", detail(id)),
            &[
                ("csrf", &csrf),
                ("category", &groceries.to_string()),
                ("reviewed", "yes"),
            ],
        )
        .await;
    assert_redirect(&reply, &detail(id));
    let read = world.read(id).await;
    assert_eq!(read["reviewed_by"]["member"], bob_actor.member.to_string());
    let event = &world.audit(id).await[0];
    assert_eq!(event.actor, signed_in(&bob_actor));
    let review = event
        .changes
        .iter()
        .find(|(name, _)| *name == "review")
        .map(|(_, change)| change.after.clone());
    assert_eq!(review, Some(AuditValue::Text("reviewed".into())));
    let page = world.page(&detail(id)).await;
    assert!(page.body.contains("Reviewed by Bob."));
}
