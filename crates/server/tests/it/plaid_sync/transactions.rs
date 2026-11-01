//! Pages, cursors, amounts and balances turning into stored transactions.

use super::{
    GET_ACCOUNTS, PAGE_1, PAGE_2, POSTED, account_json, accounts_answer, page, record, world,
};
use axum::http::StatusCode;
use coffer_domain::accounts::AccountKind;
use coffer_domain::sync::SyncRunStatus;
use coffer_domain::time::Clock as _;
use coffer_domain::transactions::SplitInput;
use coffer_server::ledger::transactions::TransactionPatch;
use coffer_store::ConnectionStatus;
use jiff::civil::date;
use serde_json::json;

#[tokio::test]
async fn first_sync_pages_until_done_and_saves_cursor() {
    let world = world().await;
    let checking = world.bound("acct-checking").await;
    world.script.page(PAGE_1);
    world.script.page(PAGE_2);

    let run = world.sync().await;

    assert_eq!(world.script.seen(GET_ACCOUNTS).len(), 1);
    assert_eq!(
        world.script.cursors(),
        [None, Some("cursor-page-1".to_owned())],
        "the second request carries the first page's cursor"
    );
    assert_eq!(
        world.connection().await.provider_state.as_deref(),
        Some("cursor-page-2")
    );
    assert_eq!(world.connection().await.status, ConnectionStatus::Active);
    // The crypto account's currency is refused, which the run reports.
    assert_eq!(run.status, SyncRunStatus::Partial);
    assert_eq!(world.transactions(checking).await.len(), 2);
}

#[tokio::test]
async fn next_sync_starts_from_saved_cursor() {
    let world = world().await;
    world.script.page(POSTED);
    world.sync().await;
    world
        .script
        .page(page(&[], &[], &[], "cursor-after", false));

    world.sync().await;

    assert_eq!(
        world.script.cursors(),
        [None, Some("cursor-page-3".to_owned())]
    );
    assert_eq!(
        world.connection().await.provider_state.as_deref(),
        Some("cursor-after")
    );
}

/// Plaid's positive is money out: a `12` from Plaid is -12.00 here, and an
/// income's negative comes out positive. Dates and texts map as documented.
#[tokio::test]
async fn plaid_amount_sign_inverted() {
    let world = world().await;
    let checking = world.bound("acct-checking").await;
    world.script.page(PAGE_1);
    world.script.page(page(
        &[record("income", "acct-checking", "-1500.25")],
        &[],
        &[],
        "c",
        false,
    ));

    world.sync().await;

    let transactions = world.transactions(checking).await;
    let spent = transactions
        .iter()
        .find(|t| t.raw_description.as_str() == "CORNER COFFEE 0001")
        .unwrap();
    assert_eq!(spent.amount_minor, -1200);
    assert_eq!(spent.currency.as_str(), "USD");
    assert_eq!(spent.posted_on, date(2027, 1, 10));
    assert_eq!(spent.transacted_on, Some(date(2027, 1, 9)));
    assert_eq!(
        spent.imported_payee.as_ref().unwrap().as_str(),
        "Corner Coffee"
    );
    assert!(!spent.pending);
    assert_eq!(
        spent.sources[0].amount_raw, "12",
        "the provider's exact text, in its own sign"
    );
    let income = transactions
        .iter()
        .find(|t| t.raw_description.as_str() == "SHOP income")
        .unwrap();
    assert_eq!(income.amount_minor, 150_025);
}

#[tokio::test]
async fn liability_balances_are_negative() {
    let world = world().await;
    let card = world.bound("acct-card").await;
    let checking = world.bound("acct-checking").await;
    world.script.page(page(&[], &[], &[], "c", false));

    world.sync().await;

    let today = world
        .server
        .clock
        .now()
        .to_zoned(world.server.config.timezone.clone())
        .date();
    let owed = world
        .server
        .store
        .latest_balance(card)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(owed.balance_minor, -41_000, "the amount owed is negative");
    assert_eq!(owed.on, today);
    let held = world
        .server
        .store
        .latest_balance(checking)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        held.balance_minor, 11_050,
        "a deposit balance keeps its sign"
    );
    assert_eq!(
        world.provider_account("acct-card").await.kind_hint,
        Some(AccountKind::CreditCard)
    );
    assert_eq!(
        world.provider_account("acct-checking").await.kind_hint,
        Some(AccountKind::Checking)
    );
}

/// The posted form, under a new id and with another amount, joins the pending
/// transaction through `pending_transaction_id`, and what the member did to
/// it stays. Plaid names the pending record as removed in the same answer.
#[tokio::test]
async fn posted_successor_attaches_via_pending_transaction_id() {
    let world = world().await;
    let card = world.bound("acct-card").await;
    world.script.page(PAGE_1);
    world.script.page(page(&[], &[], &[], "cursor-1", false));
    world.sync().await;
    let pending = world.transactions(card).await.remove(0);
    assert!(pending.pending);
    assert_eq!(pending.amount_minor, -450);
    let food = world.category().await;
    world
        .server
        .app_state()
        .ledger
        .update_transaction(
            &world.alice,
            pending.id,
            TransactionPatch {
                splits: Some(vec![SplitInput {
                    category: food,
                    minor_units: -450,
                }]),
                ..TransactionPatch::default()
            },
        )
        .await
        .unwrap();

    world.script.page(POSTED);
    world.sync().await;

    let after = world.transactions(card).await;
    assert_eq!(after.len(), 1, "no second transaction");
    let posted = &after[0];
    assert_eq!(posted.id, pending.id);
    assert!(!posted.pending);
    assert_eq!(posted.amount_minor, -475);
    assert_eq!(posted.posted_on, date(2027, 1, 13));
    assert_eq!(posted.splits.len(), 1, "the category is kept");
    assert_eq!(posted.splits[0].category, food);
    assert_eq!(posted.splits[0].minor_units, -475, "the split follows");
    let sources: Vec<_> = posted
        .sources
        .iter()
        .map(|source| source.provider_txn_id.as_str())
        .collect();
    assert_eq!(sources, ["txn-posted"], "the pending record is gone");
}

#[tokio::test]
async fn removed_ids_remove_sources() {
    let world = world().await;
    let checking = world.bound("acct-checking").await;
    world.script.page(page(
        &[
            record("stays", "acct-checking", "1"),
            record("goes", "acct-checking", "2"),
        ],
        &[],
        &[],
        "c1",
        false,
    ));
    world.sync().await;
    assert_eq!(world.transactions(checking).await.len(), 2);

    world.script.page(page(
        &[],
        &[],
        &[json!({ "transaction_id": "goes", "account_id": "acct-checking" })],
        "c2",
        false,
    ));
    world.sync().await;

    let left = world.transactions(checking).await;
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].sources[0].provider_txn_id, "stays");
}

/// A removal counts for the account it names: naming another account leaves
/// the record where it is.
#[tokio::test]
async fn removal_is_attributed_to_its_account() {
    let world = world().await;
    let checking = world.bound("acct-checking").await;
    let card = world.bound("acct-card").await;
    world.script.page(page(
        &[
            record("in-checking", "acct-checking", "1"),
            record("in-card", "acct-card", "2"),
        ],
        &[],
        &[],
        "c1",
        false,
    ));
    world.sync().await;

    world.script.page(page(
        &[],
        &[],
        &[
            json!({ "transaction_id": "in-checking", "account_id": "acct-card" }),
            json!({ "transaction_id": "in-card", "account_id": "acct-card" }),
        ],
        "c2",
        false,
    ));
    world.sync().await;

    assert_eq!(world.transactions(checking).await.len(), 1);
    assert_eq!(world.transactions(card).await.len(), 0);
}

/// Pages are netted: a record's last state in the fetch counts, so one added
/// and removed within a fetch is never stored, and a modification wins.
#[tokio::test]
async fn pages_are_netted_by_record() {
    let world = world().await;
    let checking = world.bound("acct-checking").await;
    world.script.page(page(
        &[
            record("short-lived", "acct-checking", "1"),
            record("changed", "acct-checking", "2"),
        ],
        &[],
        &[],
        "c1",
        true,
    ));
    world.script.page(page(
        &[],
        &[record("changed", "acct-checking", "3")],
        &[json!({ "transaction_id": "short-lived", "account_id": "acct-checking" })],
        "c2",
        false,
    ));

    world.sync().await;

    let stored = world.transactions(checking).await;
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].sources[0].provider_txn_id, "changed");
    assert_eq!(stored[0].amount_minor, -300);
    assert_eq!(world.imported_updates().await, 0, "nothing to rewrite");
}

/// Balances are valuations: one with more decimals than its currency applies
/// rounded half-even, and the raw field keeps the exact text.
#[tokio::test]
async fn too_precise_balance_is_rounded_and_keeps_its_text() {
    let world = world().await;
    let retirement = world.bound("acct-401k").await;
    world.script.reply(
        GET_ACCOUNTS,
        StatusCode::OK,
        accounts_answer(&[account_json("acct-401k", "investment", &json!(23631.9805))]),
    );
    world.script.page(page(
        &[record("r1", "acct-401k", "1")],
        &[],
        &[],
        "cursor-1",
        false,
    ));

    let run = world.sync().await;

    assert_eq!(run.status, SyncRunStatus::Succeeded);
    let balance = world
        .server
        .store
        .latest_balance(retirement)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(balance.balance_minor, 2_363_198);
    let provider_account = world.provider_account("acct-401k").await;
    assert_eq!(provider_account.error, None);
    assert_eq!(
        provider_account.last_balance.unwrap().amount_raw,
        "23631.9805"
    );
    assert_eq!(world.transactions(retirement).await.len(), 1);
    assert_eq!(
        world.connection().await.provider_state.as_deref(),
        Some("cursor-1")
    );
}

/// A balance too large to store is dropped and noted; the records still apply,
/// the cursor moves, and a later usable balance clears the note.
#[tokio::test]
async fn too_large_balance_is_noted_and_records_still_apply() {
    let world = world().await;
    let bound = world.bound("acct-big").await;
    world.script.reply(
        GET_ACCOUNTS,
        StatusCode::OK,
        accounts_answer(&[account_json(
            "acct-big",
            "depository",
            &json!(999_999_999_999_999_999_u64),
        )]),
    );
    world.script.page(page(
        &[record("r1", "acct-big", "1")],
        &[],
        &[],
        "cursor-1",
        false,
    ));

    world.sync().await;

    assert_eq!(world.transactions(bound).await.len(), 1);
    assert!(
        world
            .server
            .store
            .latest_balance(bound)
            .await
            .unwrap()
            .is_none(),
        "the balance is dropped"
    );
    let noted = world.provider_account("acct-big").await;
    assert!(
        noted.error.unwrap().contains("balance"),
        "the provider account says why"
    );
    assert_eq!(
        world.connection().await.provider_state.as_deref(),
        Some("cursor-1")
    );

    world.script.reply(
        GET_ACCOUNTS,
        StatusCode::OK,
        accounts_answer(&[account_json("acct-big", "depository", &json!(5))]),
    );
    world.script.page(page(&[], &[], &[], "cursor-2", false));
    world.sync().await;

    assert_eq!(world.provider_account("acct-big").await.error, None);
}

/// The stored text is what Plaid wrote, and a zero never gains a sign.
#[tokio::test]
async fn amount_text_is_exact_and_zero_is_unsigned() {
    let world = world().await;
    let checking = world.bound("acct-checking").await;
    world.script.reply(
        GET_ACCOUNTS,
        StatusCode::OK,
        accounts_answer(&[
            account_json("acct-checking", "depository", &json!(5)),
            account_json("acct-card", "credit", &json!(0)),
        ]),
    );
    world.script.page(page(
        &[
            record("zero", "acct-checking", "0"),
            record("cents", "acct-checking", "4.5"),
        ],
        &[],
        &[],
        "cursor-1",
        false,
    ));

    world.sync().await;

    let transactions = world.transactions(checking).await;
    let text = |description: &str| {
        transactions
            .iter()
            .find(|t| t.raw_description.as_str() == description)
            .unwrap()
            .sources[0]
            .amount_raw
            .clone()
    };
    assert_eq!(text("SHOP zero"), "0");
    assert_eq!(text("SHOP cents"), "4.5");
    assert_eq!(
        world
            .provider_account("acct-card")
            .await
            .last_balance
            .unwrap()
            .amount_raw,
        "0",
        "a zero balance owed gains no sign"
    );
}
