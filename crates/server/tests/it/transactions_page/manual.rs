//! Entering a transaction by hand and deleting it.

use axum::http::StatusCode;
use coffer_server::ledger::accounts::AccountPatch;
use jiff::civil::date;
use serde_json::json;

use super::{LIST, account, assert_redirect, assert_refused, detail, signed_in, world};

#[tokio::test]
async fn a_transaction_entered_by_hand_is_stored_as_typed_and_audited() {
    let mut world = world().await;
    let cash = world.cash.to_string();
    let page = world.page(LIST).await;
    assert!(page.body.contains("Enter a transaction by hand"));

    let reply = world
        .post(
            LIST,
            &[
                ("account", &cash),
                ("posted_on", "2026-10-05"),
                ("direction", "out"),
                ("amount", "12.50"),
                ("description", "Farmers market"),
                ("payee", "Market stall"),
                ("notes", "Apples\r\npears"),
            ],
        )
        .await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    let location = reply.header(axum::http::header::LOCATION).to_owned();
    let id = location
        .strip_prefix("/transactions/")
        .unwrap()
        .parse()
        .unwrap();
    let read = world.read(id).await;
    assert_eq!(read["account"], cash);
    assert_eq!(read["posted_on"], "2026-10-05");
    assert_eq!(
        read["amount"],
        json!({ "minor_units": -1_250, "currency": "USD" })
    );
    assert_eq!(read["description"], "Farmers market");
    assert_eq!(read["payee"], "Market stall");
    assert_eq!(read["notes"], "Apples\npears");
    assert_eq!(read["origin"], "manual");
    let event = &world.audit(id).await[0];
    assert_eq!(event.action, "transaction.create");
    assert_eq!(event.actor, signed_in(&world.alice));

    let reply = world
        .post(
            LIST,
            &[
                ("account", &cash),
                ("posted_on", "2026-10-06"),
                ("direction", "in"),
                ("amount", "3"),
                ("description", "Refund"),
                ("payee", ""),
                ("notes", ""),
            ],
        )
        .await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    let id = reply
        .header(axum::http::header::LOCATION)
        .strip_prefix("/transactions/")
        .unwrap()
        .parse()
        .unwrap();
    let read = world.read(id).await;
    assert_eq!(read["amount"]["minor_units"], 300);
    assert_eq!(read["payee"], json!(null));
    assert_eq!(read["notes"], json!(null));
}

#[tokio::test]
async fn what_cannot_be_entered_is_refused_in_fixed_words() {
    let mut world = world().await;
    let cash = world.cash.to_string();
    let entry = |amount: &'static str, description: &'static str, direction: &'static str| {
        vec![
            ("posted_on", "2026-10-05"),
            ("direction", direction),
            ("amount", amount),
            ("description", description),
            ("payee", ""),
            ("notes", ""),
        ]
    };
    let cases = [
        (
            entry("xyzzy", "Shop", "out"),
            "an amount is a number such as 12.50",
        ),
        (entry("-12.50", "Shop", "out"), "without a sign"),
        (
            entry("1.005", "Shop", "out"),
            "no more decimals than its currency",
        ),
        (entry("1", "Shop", "xyzzy"), "not an answer the form offers"),
        (
            entry("1", "", "out"),
            "description must be 1 to 500 characters",
        ),
    ];
    for (mut fields, words) in cases {
        fields.push(("account", &cash));
        let reply = world.post(LIST, &fields).await;
        assert_refused(&reply, StatusCode::UNPROCESSABLE_ENTITY, words, "xyzzy");
    }
    let mut fields = entry("1", "Shop", "out");
    fields.push(("account", ""));
    let reply = world.post(LIST, &fields).await;
    assert_refused(
        &reply,
        StatusCode::UNPROCESSABLE_ENTITY,
        "choose an account",
        "xyzzy",
    );

    world
        .server
        .ledger
        .update_account(
            &world.alice,
            world.cash,
            AccountPatch {
                closed: Some(true),
                ..AccountPatch::default()
            },
        )
        .await
        .unwrap();
    let page = world.page(LIST).await;
    assert!(
        !page.body.contains("Enter a transaction by hand"),
        "no open account takes one"
    );
    let mut fields = entry("1", "Shop", "out");
    fields.push(("account", &cash));
    let reply = world.post(LIST, &fields).await;
    assert_refused(
        &reply,
        StatusCode::CONFLICT,
        "the account is closed",
        "xyzzy",
    );
    let listed = world.api("/api/v1/transactions", StatusCode::OK).await;
    assert_eq!(listed["items"], json!([]), "nothing was entered");
}

#[tokio::test]
async fn deleting_one_entered_by_hand_removes_it_and_an_imported_one_stays() {
    let mut world = world().await;
    let id = world.manual(-1_250, "Bakery", date(2026, 10, 2)).await;
    let page = world.page(&detail(id)).await;
    assert!(page.body.contains("Delete this transaction"));

    let reply = world.post(&format!("{}/delete", detail(id)), &[]).await;
    assert_redirect(&reply, LIST);
    world
        .api(&format!("/api/v1/transactions/{id}"), StatusCode::NOT_FOUND)
        .await;
    let event = &world.audit(id).await[0];
    assert_eq!(event.action, "transaction.delete");
    assert_eq!(event.actor, signed_in(&world.alice));

    let checking = account(&world.server, &world.alice, "Checking", "USD").await;
    let imported = world.imported(checking, -900, "Corner shop").await;
    let reply = world
        .post(&format!("{}/delete", detail(imported)), &[])
        .await;
    assert_refused(
        &reply,
        StatusCode::CONFLICT,
        "only manual transactions can be deleted",
        "xyzzy",
    );
    world.read(imported).await;
}
