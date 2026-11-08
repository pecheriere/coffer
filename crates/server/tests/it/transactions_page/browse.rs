//! The list, its filters and pages, and one transaction's page.

use axum::http::StatusCode;
use coffer_domain::ids::TransactionId;
use jiff::civil::date;

use super::{LIST, account, detail, world};

#[tokio::test]
async fn the_list_shows_each_transaction_with_its_account_amount_and_filing() {
    let mut world = world().await;
    let groceries = world.category("Groceries").await;
    let bakery = world.manual(-1_250, "Bakery", date(2026, 10, 2)).await;
    let checking = account(&world.server, &world.alice, "Checking", "USD").await;
    world.imported(checking, 250_000, "Employer").await;
    world
        .post(
            &format!("{}/category", detail(bakery)),
            &[("category", &groceries.to_string())],
        )
        .await;

    let page = world.page(LIST).await;
    assert!(
        page.body
            .contains("href=\"/transactions\" aria-current=\"page\""),
        "in the nav"
    );
    let bakery_row = row(&page.body, "Bakery");
    assert!(bakery_row.contains("2 October 2026"));
    assert!(bakery_row.contains("Cash"));
    assert!(bakery_row.contains("-12.50 USD"));
    assert!(bakery_row.contains("Groceries group: Groceries"));
    let pay_row = row(&page.body, "Employer");
    assert!(pay_row.contains("Checking"));
    assert!(pay_row.contains("2,500.00 USD"));
    assert!(pay_row.contains("Uncategorized"));
    assert!(pay_row.contains("To review"));
    assert!(
        page.body.find("Employer").unwrap() < page.body.find("Bakery").unwrap(),
        "newest first"
    );
}

#[tokio::test]
async fn the_review_queue_and_the_filters_list_only_what_matches() {
    let mut world = world().await;
    let reviewed = world.manual(-500, "Florist", date(2026, 10, 1)).await;
    world.manual(-700, "Cinema", date(2026, 10, 9)).await;
    world
        .post(
            &format!("{}/review", detail(reviewed)),
            &[("review", "reviewed")],
        )
        .await;

    let queue = world.page("/transactions?review=unreviewed").await;
    assert!(queue.body.contains("<h1>To review</h1>"));
    assert!(queue.body.contains("Cinema"));
    assert!(!queue.body.contains("Florist"));

    let until = world.page("/transactions?until=2026-10-01").await;
    assert!(until.body.contains("Florist"), "until includes its day");
    assert!(!until.body.contains("Cinema"));
    let from = world
        .page("/transactions?from=2026-10-02&account=&q=")
        .await;
    assert!(from.body.contains("Cinema"));
    assert!(!from.body.contains("Florist"));
    let search = world.page("/transactions?q=cine").await;
    assert!(search.body.contains("Cinema"));
    assert!(!search.body.contains("Florist"));
    let uncategorized = world.page("/transactions?category=none").await;
    assert!(uncategorized.body.contains("Cinema"));
}

#[tokio::test]
async fn the_list_pages_with_the_api_cursor_and_keeps_its_filters() {
    let mut world = world().await;
    for day in 1..=28 {
        world
            .manual(-100, &format!("Shop{day:02}"), date(2026, 9, day))
            .await;
        world
            .manual(-100, &format!("Shop{:02}", day + 30), date(2026, 10, day))
            .await;
    }
    let first = world.page("/transactions?review=unreviewed").await;
    assert!(first.body.contains("Shop58"));
    assert!(!first.body.contains("Shop01"));
    let next = first
        .between("<a href=\"/transactions?review=unreviewed&amp;after=", "\"")
        .replace("&amp;", "&");
    let second = world
        .page(&format!("/transactions?review=unreviewed&after={next}"))
        .await;
    assert!(second.body.contains("Shop01"));
    assert!(!second.body.contains("Shop58"));
    assert!(!second.body.contains("Older transactions"), "the last page");
    assert!(second.body.contains("Back to the newest"));
}

#[tokio::test]
async fn a_filter_the_list_does_not_offer_is_refused_without_echo() {
    let mut world = world().await;
    for query in [
        "sort=amount",
        "review=xyzzy-review",
        "from=2026-02-30",
        "account=xyzzy-account",
        "category=xyzzy-category",
        "flag=xyzzy-flag",
    ] {
        let reply = world.browser.get(&format!("{LIST}?{query}")).await;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY, "{query}");
        assert!(!reply.body.contains("xyzzy"), "{query} is not echoed");
    }
    let reply = world.browser.get("/transactions?after=xyzzy-cursor").await;
    assert_eq!(
        reply.status,
        StatusCode::BAD_REQUEST,
        "the service's own refusal"
    );
    assert!(!reply.body.contains("xyzzy"));
}

#[tokio::test]
async fn a_transactions_page_says_what_it_is_and_a_missing_one_is_not_found() {
    let mut world = world().await;
    let checking = account(&world.server, &world.alice, "Checking", "USD").await;
    let id = world.imported(checking, -4_200, "Corner shop").await;

    let page = world.page(&detail(id)).await;
    assert!(page.body.contains("<h1>Corner shop"));
    assert!(page.body.contains("-42.00 USD"));
    assert!(page.body.contains("3 October 2026"), "posted");
    assert!(page.body.contains("2 October 2026"), "made");
    assert!(page.body.contains("CARD 0042"), "the bank's description");
    assert!(page.body.contains("Imported from the bank"));
    assert!(page.body.contains("Nobody has reviewed it yet."));
    assert!(
        !page.body.contains("Delete this transaction"),
        "imported ones stay"
    );

    for path in [detail(TransactionId::new()), format!("{LIST}/not-an-id")] {
        let reply = world.browser.get(&path).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND);
    }
}

#[tokio::test]
async fn the_pages_need_a_session() {
    let mut world = world().await;
    let id = world.manual(-100, "Kiosk", date(2026, 10, 1)).await;
    world.browser.cookie = None;
    for path in [LIST.to_owned(), detail(id)] {
        let reply = world.browser.get(&path).await;
        assert_eq!(reply.status, StatusCode::SEE_OTHER);
    }
}

/// The table row holding `name`, from its start to its end.
fn row<'a>(body: &'a str, name: &str) -> &'a str {
    body.split("<tr")
        .find(|one| one.contains(name))
        .and_then(|one| one.split("</tr>").next())
        .unwrap()
}
