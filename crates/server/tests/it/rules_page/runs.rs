//! Running a rule from its page and taking back what it filled, read back through the API and
//! audited as the signed-in member. Each answers 303 to a page saying what it did, which only
//! reads.

use axum::http::{StatusCode, header};
use serde_json::{Value, json};

use super::{by_member, csrf_of, filed_under, world};

#[tokio::test]
async fn running_fills_only_what_nobody_set_and_says_what_it_did() {
    let mut world = world().await;
    let groceries = world.category("Groceries").await;
    let dining = world.category("Dining").await;
    let everyday = world.account("Everyday").await;
    let open = world.transaction(everyday, -4_250, "CORNER MARKET").await;
    let chosen = world.transaction(everyday, -1_000, "MARKET CAFE").await;
    let (status, _) = world
        .api(
            axum::http::Method::PATCH,
            &format!("/api/v1/transactions/{chosen}"),
            Some(json!({"splits": [{"category": dining, "minor_units": -1000}]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let rule = world
        .rule(json!({"match_kind": "contains", "pattern": "market", "category": groceries}))
        .await;
    let csrf = csrf_of(&mut world.browser, &format!("/rules/{rule}")).await;

    let ran = world
        .browser
        .post_form(&format!("/rules/{rule}/apply"), &[("csrf", &csrf)])
        .await;

    assert_eq!(ran.status, StatusCode::SEE_OTHER);
    let results = ran.header(header::LOCATION).to_owned();
    assert_eq!(
        results,
        format!(
            "/rules/{rule}/ran?changed=1&categorized=1&assigned=0&reviewed=0&split=0\
             &transfer_leg=0&income_on_outflow=0&zero_amount=0&earlier_rule=0"
        )
    );
    let shown = world.browser.get(&results).await;
    assert_eq!(shown.status, StatusCode::OK);
    let outcome = shown.between(r#"id="outcome""#, "</section>");
    assert!(outcome.contains("It changed 1 transaction."));
    assert!(outcome.contains("It filed 1 transaction under Groceries."));
    assert!(outcome.contains("Nothing was marked reviewed."));
    let filed = world.read_transaction(open).await;
    assert_eq!(filed_under(&filed), Some(groceries.to_string()));
    assert_eq!(filed["review"], "unreviewed");
    assert_eq!(
        filed_under(&world.read_transaction(chosen).await),
        Some(dining.to_string()),
        "what a person chose stays"
    );
    let runs = world.events("rule.apply").await;
    assert_eq!(runs.len(), 1);
    assert!(by_member(&runs[0], world.alice.member));
    let reloaded = world.browser.get(&results).await;
    assert_eq!(reloaded.status, StatusCode::OK);
    assert_eq!(
        world.events("rule.apply").await.len(),
        1,
        "reloading runs nothing"
    );

    let again = world
        .browser
        .post_form(&format!("/rules/{rule}/apply"), &[("csrf", &csrf)])
        .await;
    assert_eq!(again.status, StatusCode::SEE_OTHER);
    let shown = world.browser.get(again.header(header::LOCATION)).await;
    assert!(shown.body.contains("It changed nothing"));
}

#[tokio::test]
async fn taking_back_reopens_what_it_filled_and_leaves_what_was_reviewed() {
    let mut world = world().await;
    let groceries = world.category("Groceries").await;
    let everyday = world.account("Everyday").await;
    let open = world.transaction(everyday, -4_250, "CORNER MARKET").await;
    let reviewed = world.transaction(everyday, -1_000, "MARKET HALL").await;
    let rule = world
        .rule(json!({"match_kind": "contains", "pattern": "market", "category": groceries}))
        .await;
    let csrf = csrf_of(&mut world.browser, &format!("/rules/{rule}")).await;
    let ran = world
        .browser
        .post_form(&format!("/rules/{rule}/apply"), &[("csrf", &csrf)])
        .await;
    assert_eq!(ran.status, StatusCode::SEE_OTHER);
    world.review(reviewed).await;

    let undone = world
        .browser
        .post_form(&format!("/rules/{rule}/undo"), &[("csrf", &csrf)])
        .await;

    assert_eq!(undone.status, StatusCode::SEE_OTHER);
    let results = undone.header(header::LOCATION).to_owned();
    assert_eq!(
        results,
        format!("/rules/{rule}/taken-back?changed=1&categorized=1&assigned=0&reviewed=1")
    );
    let shown = world.browser.get(&results).await;
    assert_eq!(shown.status, StatusCode::OK);
    let outcome = shown.between(r#"id="outcome""#, "</section>");
    assert!(outcome.contains("It emptied 1 transaction"));
    assert!(outcome.contains("It left 1 transaction someone has reviewed"));
    let reopened = world.read_transaction(open).await;
    assert_eq!(filed_under(&reopened), None);
    assert_eq!(reopened["categorized_by"], Value::Null, "open again");
    assert_eq!(
        filed_under(&world.read_transaction(reviewed).await),
        Some(groceries.to_string())
    );
    assert_eq!(world.rules().await.len(), 1, "the rule stays");
    let undos = world.events("rule.undo").await;
    assert_eq!(undos.len(), 1);
    assert!(by_member(&undos[0], world.alice.member));
}

/// Reloading what taking back did must not take back again: by then a corrected rule may have
/// filled those transactions anew.
#[tokio::test]
async fn reloading_what_taking_back_did_leaves_a_corrected_rule_s_new_fills() {
    let mut world = world().await;
    let groceries = world.category("Groceries").await;
    let everyday = world.account("Everyday").await;
    let open = world.transaction(everyday, -4_250, "CORNER MARKET").await;
    let rule = world
        .rule(json!({"match_kind": "contains", "pattern": "market", "category": groceries}))
        .await;
    let csrf = csrf_of(&mut world.browser, &format!("/rules/{rule}")).await;
    world
        .browser
        .post_form(&format!("/rules/{rule}/apply"), &[("csrf", &csrf)])
        .await;
    let undone = world
        .browser
        .post_form(&format!("/rules/{rule}/undo"), &[("csrf", &csrf)])
        .await;
    let results = undone.header(header::LOCATION).to_owned();
    world
        .browser
        .post_form(&format!("/rules/{rule}/apply"), &[("csrf", &csrf)])
        .await;

    let reloaded = world.browser.get(&results).await;

    assert_eq!(reloaded.status, StatusCode::OK);
    assert_eq!(world.events("rule.undo").await.len(), 1, "taken back once");
    assert_eq!(
        filed_under(&world.read_transaction(open).await),
        Some(groceries.to_string()),
        "the new fill stays"
    );
}

#[tokio::test]
async fn a_results_page_takes_only_whole_counts_and_never_repeats_its_address() {
    let mut world = world().await;
    let groceries = world.category("Groceries").await;
    let rule = world
        .rule(json!({"match_kind": "contains", "pattern": "market", "category": groceries}))
        .await;

    for query in [
        "changed=1&categorized=0&assigned=0&reviewed=zzqx",
        "changed=-1&categorized=0&assigned=0&reviewed=0",
        "changed=1&categorized=0&assigned=0&reviewed=0&zzqx=1",
        "changed=1",
    ] {
        let page = world
            .browser
            .get(&format!("/rules/{rule}/taken-back?{query}"))
            .await;
        assert_eq!(page.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(!page.body.contains("zzqx"), "the address isn't repeated");
    }
    let unknown = world
        .browser
        .get(
            "/rules/0190f5b4-0000-7000-8000-000000000000/taken-back\
             ?changed=0&categorized=0&assigned=0&reviewed=0",
        )
        .await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
}
