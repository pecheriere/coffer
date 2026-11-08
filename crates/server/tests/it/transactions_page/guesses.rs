//! Activity's count of what waits to be checked, and the button that confirms the guesses rules
//! made on the page shown: marks reviewed, as the person, every transaction there a rule filed or
//! assigned and nobody has touched since, and changes nothing else.

use axum::http::StatusCode;
use coffer_domain::ids::{CategoryId, TransactionId};
use coffer_domain::rules::RuleMatchKind;
use coffer_server::ledger::rules::RuleInput;
use coffer_server::testkit::browser::Reply;
use jiff::civil::date;
use serde_json::Value;

use super::{LIST, World, assert_redirect, detail, member, signed_in, world};

const QUEUE: &str = "/transactions?review=unreviewed";
/// Where the confirm form starts; it ends at the next `</form>`.
const FORM: &str = "id=\"confirm-guesses\"";

/// A rule filing every transaction whose description holds `pattern` under `category`, run once
/// by alice over what is there.
async fn file_by_rule(world: &World, pattern: &str, category: CategoryId) {
    let rule = world
        .server
        .ledger
        .create_rule(
            &world.alice,
            RuleInput {
                match_kind: RuleMatchKind::Contains,
                pattern: pattern.into(),
                account: None,
                amount: None,
                category: Some(category),
                assignee: None,
                position: None,
                enabled: None,
            },
            None,
        )
        .await
        .unwrap();
    world
        .server
        .ledger
        .apply_rule(&world.alice, rule.id)
        .await
        .unwrap();
}

/// The category a transaction the API reads is filed under, if it is in one.
fn filed_under(read: &Value) -> Option<String> {
    read["splits"][0]["category"].as_str().map(str::to_owned)
}

/// The confirm form's markup.
fn confirm_form(page: &Reply) -> String {
    page.between(FORM, "</form>")
}

/// The transactions a confirm form would send.
fn picks(form: &str) -> Vec<String> {
    form.match_indices("name=\"pick_")
        .map(|(at, _)| {
            let rest = &form[at + "name=\"pick_".len()..];
            rest[..rest.find('"').unwrap()].to_owned()
        })
        .collect()
}

#[tokio::test]
async fn the_page_says_how_many_wait() {
    let mut world = world().await;
    let page = world.page(LIST).await;
    assert!(page.body.contains("Nothing to check."));

    world.manual(-100, "Kiosk", date(2026, 10, 1)).await;
    let page = world.page(LIST).await;
    assert!(
        page.body
            .contains("<a href=\"/transactions?review=unreviewed\">1 transaction to check</a>")
    );

    let savings = super::account(&world.server, &world.alice, "Savings", "USD").await;
    world.manual(-200, "Market", date(2026, 10, 2)).await;
    world
        .manual_in(savings, -300, "Garage", date(2026, 10, 3))
        .await;
    let reviewed = world.manual(-400, "Bakery", date(2026, 10, 4)).await;
    world
        .post(
            &format!("{}/review", detail(reviewed)),
            &[("review", "reviewed")],
        )
        .await;
    let page = world.page(LIST).await;
    assert!(page.body.contains("3 transactions to check"));
    assert_eq!(
        world
            .api("/api/v1/transactions/review-count", StatusCode::OK)
            .await["unreviewed"],
        3,
        "the number the API answers"
    );
    let filtered = world.page(&format!("{LIST}?account={savings}")).await;
    assert!(
        filtered.body.contains("3 transactions to check"),
        "the whole queue, not the filter's share"
    );
    let (mut bob, _) = member(&world).await;
    let page = bob.get(LIST).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("3 transactions to check"));
}

#[tokio::test]
async fn confirms_only_untouched_rule_guesses() {
    let mut world = world().await;
    let groceries = world.category("Groceries").await;
    let dining = world.category("Dining").await;
    let guessed = world.manual(-100, "Corner market", date(2026, 10, 1)).await;
    let changed = world.manual(-200, "Market hall", date(2026, 10, 2)).await;
    let chosen = world.manual(-300, "Bakery", date(2026, 10, 3)).await;
    let reviewed = world.manual(-400, "Night market", date(2026, 10, 4)).await;
    file_by_rule(&world, "market", groceries).await;
    world
        .post(
            &format!("{}/category", detail(changed)),
            &[("category", &dining.to_string())],
        )
        .await;
    world
        .post(
            &format!("{}/category", detail(chosen)),
            &[("category", &groceries.to_string())],
        )
        .await;
    world
        .post(
            &format!("{}/review", detail(reviewed)),
            &[("review", "reviewed")],
        )
        .await;

    for path in [QUEUE, LIST] {
        let page = world.page(path).await;
        let form = confirm_form(&page);
        assert!(
            form.contains("Confirm the guess rules made on this page"),
            "{path}"
        );
        assert_eq!(picks(&form), [guessed.to_string()], "{path}");
        assert!(form.contains("name=\"review\" value=\"reviewed\""));
    }

    let page = world.page(QUEUE).await;
    let form = confirm_form(&page);
    assert!(form.contains("name=\"back\" value=\"review=unreviewed\""));
    let pick = format!("pick_{guessed}");
    let reply = world
        .post(
            "/transactions/batch",
            &[
                ("review", "reviewed"),
                ("back", "review=unreviewed"),
                (&pick, "yes"),
            ],
        )
        .await;
    assert_redirect(&reply, QUEUE);

    let read = world.read(guessed).await;
    assert_eq!(read["review"], "reviewed");
    assert_eq!(
        read["reviewed_by"]["member"],
        world.alice.member.to_string()
    );
    assert_eq!(filed_under(&read), Some(groceries.to_string()));
    assert_eq!(world.audit(guessed).await[0].actor, signed_in(&world.alice));
    for (id, category) in [(changed, dining), (chosen, groceries)] {
        let read = world.read(id).await;
        assert_eq!(read["review"], "unreviewed");
        assert_eq!(filed_under(&read), Some(category.to_string()));
    }
    assert_eq!(
        filed_under(&world.read(reviewed).await),
        Some(groceries.to_string())
    );
    let page = world.page(QUEUE).await;
    assert!(!page.body.contains(FORM), "nothing left to confirm");
}

#[tokio::test]
async fn the_button_counts_every_guess_on_the_page() {
    let mut world = world().await;
    let groceries = world.category("Groceries").await;
    let mut guessed: Vec<TransactionId> = Vec::new();
    for (minor_units, payee) in [(-100, "Corner market"), (-200, "Market hall")] {
        guessed.push(world.manual(minor_units, payee, date(2026, 10, 1)).await);
    }
    file_by_rule(&world, "market", groceries).await;

    let page = world.page(QUEUE).await;
    let form = confirm_form(&page);
    assert!(form.contains("Confirm the 2 guesses rules made on this page"));
    let mut sent = picks(&form);
    sent.sort();
    let mut expected: Vec<String> = guessed.iter().map(ToString::to_string).collect();
    expected.sort();
    assert_eq!(sent, expected);
}

#[tokio::test]
async fn no_guesses_no_button() {
    let mut world = world().await;
    let groceries = world.category("Groceries").await;
    let chosen = world.manual(-300, "Bakery", date(2026, 10, 3)).await;
    world.manual(-100, "Kiosk", date(2026, 10, 1)).await;
    world
        .post(
            &format!("{}/category", detail(chosen)),
            &[("category", &groceries.to_string())],
        )
        .await;

    for path in [QUEUE, LIST] {
        let page = world.page(path).await;
        assert!(!page.body.contains(FORM), "{path}");
        assert!(
            !page.body.contains("type=\"hidden\" name=\"pick_"),
            "{path}"
        );
        assert!(
            !page.body.contains("rules made on this page"),
            "no button, not a disabled one: {path}"
        );
    }
}

#[tokio::test]
async fn confirming_returns_to_the_same_list() {
    let mut world = world().await;
    let groceries = world.category("Groceries").await;
    let guessed = world.manual(-100, "Corner market", date(2026, 10, 1)).await;
    world.manual(-200, "Kiosk", date(2026, 10, 2)).await;
    file_by_rule(&world, "market", groceries).await;
    let filtered = format!("{LIST}?review=unreviewed&q=market");

    let page = world.page(&filtered).await;
    let form = confirm_form(&page);
    assert!(form.contains("name=\"back\" value=\"review=unreviewed&amp;q=market\""));
    let pick = format!("pick_{guessed}");
    let reply = world
        .post(
            "/transactions/batch",
            &[
                ("review", "reviewed"),
                ("back", "review=unreviewed&q=market"),
                (&pick, "yes"),
            ],
        )
        .await;
    assert_redirect(&reply, &filtered);
    let page = world.page(&filtered).await;
    assert!(page.body.contains("Nothing left to review."));
    assert!(
        page.body.contains("1 transaction to check"),
        "the kiosk waits"
    );
}
