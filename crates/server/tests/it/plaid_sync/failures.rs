//! What a sync does with bad data, errors and a dead item: which accounts fail
//! and when the cursor is held.

use super::{
    GET_ACCOUNTS, LOGIN_REQUIRED, MUTATION, SYNC, account_json, accounts_answer, page, record,
    world,
};
use axum::http::StatusCode;
use coffer_domain::accounts::AccountKind;
use coffer_domain::sync::SyncRunStatus;
use coffer_domain::time::Clock as _;
use coffer_server::sync::scheduler::{ScheduleEntry, due};
use coffer_server::testkit::INJECTED_FAILURE_RECORD_ID;
use coffer_store::ConnectionStatus;
use jiff::SignedDuration;
use serde_json::json;

#[tokio::test]
async fn mutation_during_pagination_restarts() {
    let world = world().await;
    let checking = world.bound("acct-checking").await;
    world.script.page(page(
        &[record("abandoned", "acct-checking", "1")],
        &[],
        &[],
        "stale",
        true,
    ));
    world.script.reply(SYNC, StatusCode::BAD_REQUEST, MUTATION);
    world.script.page(page(
        &[record("kept", "acct-checking", "2")],
        &[],
        &[],
        "fresh",
        true,
    ));
    world.script.page(page(&[], &[], &[], "done", false));

    let run = world.sync().await;

    assert_eq!(
        world.script.cursors(),
        [
            None,
            Some("stale".to_owned()),
            None,
            Some("fresh".to_owned())
        ],
        "the restart uses the cursor the fetch started with"
    );
    let stored = world.transactions(checking).await;
    assert_eq!(
        stored.len(),
        1,
        "nothing of the abandoned pass is committed"
    );
    assert_eq!(stored[0].sources[0].provider_txn_id, "kept");
    assert_eq!(
        world.connection().await.provider_state.as_deref(),
        Some("done")
    );
    assert_ne!(run.status, SyncRunStatus::Failed);
}

#[tokio::test]
async fn data_that_keeps_changing_gives_up_after_three_passes() {
    let world = world().await;
    let checking = world.bound("acct-checking").await;
    world.script.page(page(&[], &[], &[], "saved", false));
    world.sync().await;
    world.script.always(SYNC, StatusCode::BAD_REQUEST, MUTATION);
    let before = world.script.cursors().len();

    let run = world.sync().await;

    assert_eq!(world.script.cursors().len() - before, 3, "three passes");
    assert_eq!(run.status, SyncRunStatus::Failed);
    assert_eq!(
        run.detail.as_deref(),
        Some("Plaid data changed during sync; try again")
    );
    let connection = world.connection().await;
    assert_eq!(connection.provider_state.as_deref(), Some("saved"));
    assert_eq!(connection.status, ConnectionStatus::Error);
    assert_eq!(world.transactions(checking).await.len(), 0);
}

#[tokio::test]
async fn item_login_required_marks_reauth() {
    let world = world().await;
    world
        .script
        .reply(GET_ACCOUNTS, StatusCode::BAD_REQUEST, LOGIN_REQUIRED);

    let run = world.sync().await;

    assert_eq!(run.status, SyncRunStatus::Failed);
    let connection = world.connection().await;
    assert_eq!(connection.status, ConnectionStatus::ReauthRequired);
    let entry = ScheduleEntry {
        id: connection.id,
        status: connection.status,
        last_attempt_at: None,
        effective_interval: SignedDuration::from_hours(1),
    };
    assert_eq!(
        due(world.server.clock.now(), &[entry]),
        [],
        "scheduled syncs stop spending requests on it"
    );

    // The same from a page of transactions.
    let world2 = self::world().await;
    world2
        .script
        .reply(SYNC, StatusCode::BAD_REQUEST, LOGIN_REQUIRED);
    world2.sync().await;
    assert_eq!(
        world2.connection().await.status,
        ConnectionStatus::ReauthRequired
    );
}

/// One unreadable account leaves the others syncing. Its records were not
/// stored, so the cursor stays and the next sync fetches them again, without
/// duplicating what the others already hold.
#[tokio::test]
async fn bad_account_is_isolated_and_holds_the_cursor() {
    let world = world().await;
    let good = world.bound("good").await;
    let bad = world.bound("bad").await;
    world.script.reply(
        GET_ACCOUNTS,
        StatusCode::OK,
        r#"{"accounts":[
            {"account_id":"good","name":"Good","type":"depository","subtype":"checking",
             "balances":{"current":5,"iso_currency_code":"USD"}},
            {"account_id":"bad","name":"Bad","type":"depository",
             "balances":{"current":1e2,"iso_currency_code":"USD"}}]}"#,
    );
    let records = page(
        &[record("g1", "good", "1"), record("b1", "bad", "2")],
        &[],
        &[],
        "cursor-1",
        false,
    );
    world.script.page(records.clone());

    let run = world.sync().await;

    assert_eq!(run.status, SyncRunStatus::Partial);
    assert_eq!(world.transactions(good).await.len(), 1);
    assert_eq!(world.transactions(bad).await.len(), 0);
    assert!(
        world
            .provider_account("bad")
            .await
            .error
            .unwrap()
            .contains("could not read"),
        "the bad account carries the error"
    );
    assert_eq!(world.provider_account("good").await.error, None);
    assert_eq!(
        world.connection().await.provider_state,
        None,
        "the cursor does not pass records that were not stored"
    );

    world.script.reply(
        GET_ACCOUNTS,
        StatusCode::OK,
        accounts_answer(&[
            account_json("good", "depository", &json!(5)),
            account_json("bad", "depository", &json!(100)),
        ]),
    );
    world.script.page(records);

    let run = world.sync().await;

    assert_eq!(run.status, SyncRunStatus::Succeeded);
    assert_eq!(world.script.cursors(), [None, None]);
    assert_eq!(world.transactions(good).await.len(), 1, "not duplicated");
    assert_eq!(world.transactions(bad).await.len(), 1, "fetched again");
    assert_eq!(world.provider_account("bad").await.error, None);
    assert_eq!(
        world.connection().await.provider_state.as_deref(),
        Some("cursor-1")
    );
}

/// Records that could not be written are fetched again, as the engine tells
/// the member: the cursor does not pass them.
#[tokio::test]
async fn storage_failure_holds_the_cursor() {
    let world = world().await;
    let checking = world.bound("acct-checking").await;
    world.server.inject_source_insert_failure().await;
    world.script.page(page(
        &[record(INJECTED_FAILURE_RECORD_ID, "acct-checking", "1")],
        &[],
        &[],
        "cursor-1",
        false,
    ));

    let run = world.sync().await;

    assert_eq!(run.status, SyncRunStatus::Partial);
    assert_eq!(world.transactions(checking).await.len(), 0);
    assert_eq!(world.connection().await.provider_state, None);
}

/// A currency coffer cannot hold never becomes usable by fetching again:
/// the account says so, and the others' cursor moves on.
#[tokio::test]
async fn unsupported_currency_does_not_hold_the_cursor() {
    let world = world().await;
    world.script.page(page(
        &[record("coin", "acct-crypto", "1")],
        &[],
        &[],
        "cursor-1",
        false,
    ));

    let run = world.sync().await;

    assert_eq!(run.status, SyncRunStatus::Partial);
    let crypto = world.provider_account("acct-crypto").await;
    assert!(crypto.error.unwrap().contains("currency"));
    assert_eq!(crypto.currency_raw, "BTC");
    assert_eq!(
        world.connection().await.provider_state.as_deref(),
        Some("cursor-1")
    );
}

/// Records of an account Plaid did not list become that account's error, and
/// are fetched again rather than lost.
#[tokio::test]
async fn records_of_an_unlisted_account_hold_the_cursor() {
    let world = world().await;
    world.script.page(page(
        &[record("stray", "acct-new", "1")],
        &[],
        &[],
        "cursor-1",
        false,
    ));

    let run = world.sync().await;

    assert_eq!(run.status, SyncRunStatus::Partial);
    assert!(
        world
            .provider_account("acct-new")
            .await
            .error
            .unwrap()
            .contains("did not list")
    );
    assert_eq!(world.connection().await.provider_state, None);
}

/// A record that cannot be read never fails its account: it is counted, the
/// rest is applied, and the cursor moves on. One that names no account is
/// reported on the connection.
#[tokio::test]
async fn unusable_records_never_fail_the_account() {
    let world = world().await;
    let checking = world.bound("acct-checking").await;
    world.script.page(page(
        &[
            record("fine", "acct-checking", "1"),
            record("quoted", "acct-checking", "\"12\""),
            json!({ "transaction_id": "no-fields", "account_id": "acct-checking" }),
            json!({ "transaction_id": "no-account" }),
        ],
        &[],
        &[],
        "cursor-1",
        false,
    ));

    let run = world.sync().await;

    assert_eq!(run.status, SyncRunStatus::Partial);
    assert!(
        run.detail.unwrap().contains("3 skipped"),
        "all three are counted, the one of no account too"
    );
    assert_eq!(world.provider_account("acct-checking").await.error, None);
    assert_eq!(world.transactions(checking).await.len(), 1);
    let connection = world.connection().await;
    assert_eq!(connection.provider_state.as_deref(), Some("cursor-1"));
    assert!(
        connection
            .last_error
            .unwrap()
            .contains("assign to an account"),
        "a record of no known account is reported on the connection"
    );
}

/// Every Plaid text is sanitized before it is stored or shown.
#[tokio::test]
async fn provider_text_is_sanitized() {
    let world = world().await;
    let hostile = "Bank\u{202e}x https://user:pw-MARKER@h.example\u{7}";
    let account = world.bound("hostile").await;
    let mut listed = account_json("hostile", "depository", &json!(1));
    listed["name"] = json!(hostile);
    world.script.reply(
        GET_ACCOUNTS,
        StatusCode::OK,
        accounts_answer(&[listed, json!({ "account_id": "broken", "name": hostile })]),
    );
    let mut text = record("t", "hostile", "1");
    text["name"] = json!(hostile);
    text["merchant_name"] = json!(hostile);
    world.script.page(page(&[text], &[], &[], "c", false));

    world.sync().await;

    let mut stored = Vec::new();
    for id in ["hostile", "broken"] {
        stored.push(world.provider_account(id).await.name);
    }
    let transaction = world.transactions(account).await.remove(0);
    stored.push(transaction.raw_description.as_str().to_owned());
    stored.push(
        transaction
            .imported_payee
            .as_ref()
            .unwrap()
            .as_str()
            .to_owned(),
    );
    for text in stored {
        assert!(!text.contains("pw-MARKER"), "user information is hidden");
        assert!(!text.contains('\u{202e}'), "format characters are removed");
        assert!(!text.contains('\u{7}'), "control characters are removed");
    }
}

/// An Item whose first update is not ready answers with an empty cursor:
/// nothing is saved, and the next sync starts from the beginning.
#[tokio::test]
async fn empty_cursor_is_not_saved() {
    let world = world().await;
    world.script.page(page(&[], &[], &[], "", false));

    let run = world.sync().await;

    assert_ne!(run.status, SyncRunStatus::Failed);
    assert_eq!(world.connection().await.provider_state, None);
}

/// A cursor is sent back as it came, so one that would be stored altered is
/// refused, and the saved cursor stays.
#[tokio::test]
async fn unusable_cursor_is_refused() {
    let world = world().await;
    world.script.page(page(&[], &[], &[], "u:p@host", false));

    let run = world.sync().await;

    assert_eq!(run.status, SyncRunStatus::Failed);
    assert_eq!(
        run.detail.as_deref(),
        Some("Plaid sent a sync cursor coffer could not store")
    );
    assert_eq!(world.connection().await.provider_state, None);
}

/// An answer that never ends is cut off, with nothing stored.
#[tokio::test]
async fn endless_pagination_is_cut_off() {
    let world = world().await;
    let checking = world.bound("acct-checking").await;
    world.script.always(
        SYNC,
        StatusCode::OK,
        page(
            &[record("again", "acct-checking", "1")],
            &[],
            &[],
            "same",
            true,
        ),
    );

    let run = world.sync().await;

    assert_eq!(run.status, SyncRunStatus::Failed);
    assert_eq!(world.script.cursors().len(), 200);
    assert_eq!(world.transactions(checking).await.len(), 0);
    assert_eq!(world.connection().await.provider_state, None);
}

/// A passing failure at Plaid is `Unavailable`, which the connection shows as
/// an error it can retry; only Plaid's codes reach the text.
#[tokio::test]
async fn transient_answers_are_unavailable() {
    let world = world().await;
    world.script.reply(
        GET_ACCOUNTS,
        StatusCode::BAD_GATEWAY,
        "<html>bad gateway</html>",
    );

    let run = world.sync().await;

    assert_eq!(run.status, SyncRunStatus::Failed);
    assert_eq!(world.connection().await.status, ConnectionStatus::Error);
    let detail = run.detail.unwrap();
    assert!(detail.contains("HTTP_502"), "the status is named");
    assert!(!detail.contains("html"), "the body is not kept");
}

/// A currency Plaid did not name may come with the next answer, so unlike an
/// unsupported one it holds the cursor, whatever its records say.
#[tokio::test]
async fn missing_currency_holds_the_cursor() {
    let world = world().await;
    world.script.reply(
        GET_ACCOUNTS,
        StatusCode::OK,
        r#"{"accounts":[{"account_id":"acct-none","name":"None","type":"depository",
            "balances":{"current":5,"iso_currency_code":null,"unofficial_currency_code":null}}]}"#,
    );
    world.script.page(page(
        &[record("r1", "acct-none", "1")],
        &[],
        &[],
        "cursor-1",
        false,
    ));

    let run = world.sync().await;

    assert_eq!(run.status, SyncRunStatus::Partial);
    let held = world.provider_account("acct-none").await;
    assert!(held.error.unwrap().contains("currency"));
    assert_eq!(world.connection().await.provider_state, None);
}

/// An answer without a currency says nothing about the account's: the one a
/// good sync learned stays, and the refusal is recorded.
#[tokio::test]
async fn blank_currency_keeps_the_known_one() {
    let world = world().await;
    world.script.page(page(&[], &[], &[], "cursor-1", false));
    world.sync().await;
    assert_eq!(
        world.provider_account("acct-checking").await.currency_raw,
        "USD"
    );

    world.script.reply(
        GET_ACCOUNTS,
        StatusCode::OK,
        r#"{"accounts":[{"account_id":"acct-checking","name":"Checking","type":"depository",
            "balances":{"current":5,"iso_currency_code":null,"unofficial_currency_code":null}}]}"#,
    );
    world.script.page(page(&[], &[], &[], "cursor-2", false));
    world.sync().await;

    let refused = world.provider_account("acct-checking").await;
    assert_eq!(refused.currency_raw, "USD", "the known currency stays");
    assert!(
        refused.error.unwrap().contains("currency"),
        "the refusal is recorded"
    );
}

/// A failed account's report says nothing about what it is: what a good sync
/// learned about it stays.
#[tokio::test]
async fn failed_account_keeps_what_is_known() {
    let world = world().await;
    world.script.page(page(&[], &[], &[], "cursor-1", false));
    world.sync().await;
    let known = world.provider_account("acct-card").await;
    assert_eq!(known.kind_hint, Some(AccountKind::CreditCard));

    world.script.reply(
        GET_ACCOUNTS,
        StatusCode::OK,
        r#"{"accounts":[{"account_id":"acct-card","name":"Other","type":"depository",
            "balances":{"current":1e2,"iso_currency_code":"EUR"}}]}"#,
    );
    world.script.page(page(&[], &[], &[], "cursor-2", false));
    world.sync().await;

    let failed = world.provider_account("acct-card").await;
    assert!(failed.error.is_some(), "the failure is recorded");
    assert_eq!(failed.name, known.name);
    assert_eq!(failed.currency_raw, known.currency_raw);
    assert_eq!(failed.kind_hint, known.kind_hint);
}

/// A dead Item cannot come back by trying again: scheduled syncs stop spending
/// requests on it, from either request of a fetch.
#[tokio::test]
async fn dead_item_marks_reauth() {
    for code in ["ITEM_NOT_FOUND", "USER_PERMISSION_REVOKED"] {
        for path in [GET_ACCOUNTS, SYNC] {
            let world = world().await;
            world.script.reply(
                path,
                StatusCode::BAD_REQUEST,
                json!({ "error_type": "ITEM_ERROR", "error_code": code }).to_string(),
            );

            let run = world.sync().await;

            assert_eq!(run.status, SyncRunStatus::Failed);
            assert_eq!(
                world.connection().await.status,
                ConnectionStatus::ReauthRequired
            );
        }
    }
}

/// Binding the account to another currency can be undone, so its records are
/// fetched again rather than passed by.
#[tokio::test]
async fn currency_differing_from_the_binding_holds_the_cursor() {
    let world = world().await;
    let bound = world.bound("acct-eur").await;
    world.script.reply(
        GET_ACCOUNTS,
        StatusCode::OK,
        r#"{"accounts":[{"account_id":"acct-eur","name":"Euro","type":"depository",
            "balances":{"current":5,"iso_currency_code":"EUR"}}]}"#,
    );
    let mut euros = record("r1", "acct-eur", "1");
    euros["iso_currency_code"] = json!("EUR");
    world
        .script
        .page(page(&[euros], &[], &[], "cursor-1", false));

    let run = world.sync().await;

    assert_eq!(run.status, SyncRunStatus::Partial);
    assert_eq!(world.transactions(bound).await.len(), 0);
    assert_eq!(world.connection().await.provider_state, None);
}

/// A removal is a record too: a failed account holding only removals must see
/// them again.
#[tokio::test]
async fn failed_account_with_only_removals_holds_the_cursor() {
    let world = world().await;
    world.bound("acct-bad").await;
    world.script.reply(
        GET_ACCOUNTS,
        StatusCode::OK,
        r#"{"accounts":[{"account_id":"acct-bad","name":"Bad","type":"depository",
            "balances":{"current":1e2,"iso_currency_code":"USD"}}]}"#,
    );
    world.script.page(page(
        &[],
        &[],
        &[json!({ "transaction_id": "gone", "account_id": "acct-bad" })],
        "cursor-1",
        false,
    ));

    let run = world.sync().await;

    assert_eq!(run.status, SyncRunStatus::Partial);
    assert_eq!(world.connection().await.provider_state, None);
}
