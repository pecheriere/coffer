//! What must stay true on the page: nothing said in the present that the latest point
//! doesn't hold, nothing shown as 0 that has no value, and nothing invented.

use serde_json::json;

use super::page;
use crate::net_worth::{Golden, World, world};
use crate::pages::forecast::{row, section, text_of};

const CARD: &str = "credit_card";

/// Display USD, an account holding 1,000.00 from 1 October so the range keeps its points,
/// and a card.
async fn with_a_card(world: &World) -> coffer_domain::ids::AccountId {
    world.display("USD").await;
    let kept = world.holding("Kept", "USD").await;
    world.balance(kept, "2026-10-01", 100_000).await;
    world
        .holding_with(
            "Card",
            CARD,
            "USD",
            json!({"liquidity": "short_term_liability"}),
        )
        .await
}

/// Takes one day's value of `holder` out of the table, as a lost write would.
pub(super) async fn hole_today(world: &World, holder: coffer_domain::ids::AccountId) {
    world
        .server
        .store
        .execute_unchecked_for_tests(
            "DELETE FROM account_daily_values WHERE account_id = ? AND on_date = '2026-11-02'",
            &holder.to_string(),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn a_closed_card_paid_off_is_not_said_to_still_owe() {
    let world = world().await;
    let card = with_a_card(&world).await;
    world.balance(card, "2026-10-01", -30_000).await;
    world.at("2026-10-20");
    world.patch_holding(card, &json!({"closed": true})).await;
    world.at(crate::net_worth::TODAY);
    world.balance(card, "2026-10-25", 0).await;

    let answer = text_of(section(&page(&world, "?range=3m").await.body, "answer"));

    assert!(
        answer.contains("Card is closed and owed 300.00 USD when last counted, on 24 October 2026")
    );
    assert!(!answer.contains("still owes"), "it doesn't now");
}

#[tokio::test]
async fn a_closed_card_still_owing_says_so() {
    let world = world().await;
    let card = with_a_card(&world).await;
    world.balance(card, "2026-10-01", -30_000).await;
    world.at("2026-10-20");
    world.patch_holding(card, &json!({"closed": true})).await;
    world.at(crate::net_worth::TODAY);

    let answer = text_of(section(&page(&world, "?range=3m").await.body, "answer"));

    assert!(answer.contains("Card is closed but still owes 300.00 USD"));
}

#[tokio::test]
async fn a_card_once_in_hand_and_now_owing_is_said_in_the_past() {
    let world = world().await;
    let card = with_a_card(&world).await;
    world.balance(card, "2026-10-01", 5_000).await;
    world.balance(card, "2026-10-28", -30_000).await;

    let answer = text_of(section(&page(&world, "?range=3m").await.body, "answer"));

    assert!(answer.contains("Card showed 50.00 USD in hand on 27 October 2026"));
    assert!(!answer.contains("shows 50.00"), "it doesn't now");
}

#[tokio::test]
async fn a_card_in_hand_now_says_so() {
    let world = world().await;
    let card = with_a_card(&world).await;
    world.balance(card, "2026-10-01", 5_000).await;

    let answer = text_of(section(&page(&world, "?range=3m").await.body, "answer"));

    assert!(answer.contains("Card is a card or loan, but shows 50.00 USD in hand"));
}

/// The golden household with an account nothing can convert, since 31 October.
async fn with_baht(world: &World) -> Golden {
    let golden = world.golden().await;
    let baht = world.holding("Baht account", "THB").await;
    world.balance(baht, "2026-10-31", 50_000).await;
    golden
}

#[tokio::test]
async fn a_class_with_no_amount_is_not_zero() {
    let world = world().await;
    with_baht(&world).await;

    let body = page(&world, "?range=3m").await.body;

    assert_eq!(row(section(&body, "latest"), "Cash"), ["No total"]);
}

#[tokio::test]
async fn an_account_with_no_rate_is_not_zero() {
    let world = world().await;
    with_baht(&world).await;

    let body = page(&world, "?range=3m").await.body;

    let table = section(&body, "accounts");
    assert_eq!(row(table, "Baht account")[1], "No rate");
}

#[tokio::test]
async fn a_closed_accounts_amount_is_marked_not_counted() {
    let world = world().await;
    let Golden { checking, .. } = world.golden().await;
    world.at("2026-10-29");
    world
        .patch_holding(checking, &json!({"closed": true}))
        .await;
    world.at(crate::net_worth::TODAY);

    let body = page(&world, "?range=3m").await.body;

    let table = section(&body, "accounts");
    assert_eq!(row(table, "Checking")[1], "Not counted");
    assert_eq!(row(table, "Checking")[0], "5,500.00 USD from 30 Oct 2026");
}

#[tokio::test]
async fn an_account_not_counted_has_a_dash_for_a_first_day() {
    let world = world().await;
    world.golden().await;

    let body = page(&world, "?range=3m").await.body;

    let table = section(&body, "accounts");
    assert_eq!(
        row(table, "Side"),
        ["Not counted", "Not counted", "\u{2013}"]
    );
}

/// A year of range before the first value isn't a stretch with nothing counted.
#[tokio::test]
async fn nothing_is_invented_before_the_first_point() {
    let world = world().await;
    world.golden().await;

    let body = page(&world, "").await.body;

    assert!(!text_of(section(&body, "history")).contains("Nothing counted"));
}

#[tokio::test]
async fn an_account_not_counted_explains_no_jump() {
    let world = world().await;
    world.golden().await;
    let hidden = world
        .holding_with(
            "Hidden pot",
            "savings",
            "USD",
            json!({"include_in_net_worth": false}),
        )
        .await;
    world.balance(hidden, "2026-10-29", 1_000).await;

    let body = page(&world, "?range=3m").await.body;

    assert!(!section(&body, "history").contains("Hidden pot"));
}

/// The opening tag of the table's `details`.
fn values_tag(body: &str) -> &str {
    let from = body.find("<details").unwrap();
    let len = body[from..].find('>').unwrap();
    &body[from..from + len]
}

#[tokio::test]
async fn the_table_is_closed_with_no_gap() {
    let world = world().await;
    world.golden().await;

    let body = page(&world, "?range=3m").await.body;

    assert!(!values_tag(&body).contains("open"));
}

#[tokio::test]
async fn the_table_opens_for_a_recent_gap_even_on_a_long_range() {
    let world = world().await;
    world.rates("USD", &[("2020-01-01", "1.1")]).await;
    world.display("USD").await;
    let kept = world.holding("Kept", "USD").await;
    world.balance(kept, "2020-01-01", 100_000).await;
    let baht = world.holding("Baht", "THB").await;
    world.balance(baht, "2026-10-20", 1_000).await;

    let body = page(&world, "?range=all").await.body;

    assert!(values_tag(&body).contains("open"));
}

/// An old gap on a long range: hundreds of rows would stand between the chart and the rest.
#[tokio::test]
async fn the_table_stays_closed_for_an_old_gap_on_a_long_range() {
    let world = world().await;
    world.rates("USD", &[("2020-01-01", "1.1")]).await;
    world.display("USD").await;
    let kept = world.holding("Kept", "USD").await;
    world.balance(kept, "2020-01-01", 100_000).await;
    let big = world.holding("Big", "EUR").await;
    world
        .balance(big, "2021-01-10", 9_000_000_000_000_000_000)
        .await;
    world.balance(big, "2021-03-10", 100).await;

    let body = page(&world, "?range=all").await.body;

    assert!(row(&body, "January 2021")[0].contains("No total"), "a gap");
    assert!(!values_tag(&body).contains("open"));
}

/// On a short range an old gap is still worth seeing: the table is only a few rows.
#[tokio::test]
async fn the_table_opens_for_any_gap_on_a_short_range() {
    let world = world().await;
    world.golden().await;
    let huge = world.holding("Huge", "EUR").await;
    world
        .balance(huge, "2026-10-26", 9_000_000_000_000_000_000)
        .await;
    world.balance(huge, "2026-10-27", 100_000).await;

    let body = page(&world, "?range=3m").await.body;

    assert!(
        row(&body, "26 October 2026")[0].contains("No total"),
        "a gap"
    );
    assert!(
        row(&body, "2 November 2026")[0].contains("7,434.40"),
        "none now"
    );
    assert!(values_tag(&body).contains("open"));
}

/// A run of days with the same reason names it on the newest, once.
#[tokio::test]
async fn a_run_names_each_account_once() {
    let world = world().await;
    with_baht(&world).await;

    let body = page(&world, "?range=3m").await.body;

    let history = text_of(section(&body, "history"));
    assert_eq!(
        history.matches("Baht account has no exchange rate").count(),
        1
    );
    assert!(
        row(&body, "2 November 2026")[0].contains("Baht account"),
        "the newest"
    );
    assert_eq!(row(&body, "1 November 2026"), ["No total, as above."]);
    assert_eq!(row(&body, "31 October 2026"), ["No total, as above."]);
}

#[tokio::test]
async fn a_negative_number_is_marked_and_a_positive_one_is_not() {
    let world = world().await;
    world.golden().await;
    let positive = page(&world, "").await.body;
    assert!(!section(&positive, "answer").contains("answer-short"));

    let world = crate::net_worth::world().await;
    world.display("USD").await;
    let loan = world.holding("Loan", "USD").await;
    world.balance(loan, "2026-10-26", -100).await;
    let negative = page(&world, "").await.body;
    assert!(section(&negative, "answer").contains("answer-short"));
}

#[tokio::test]
async fn no_total_says_why_and_claims_no_number() {
    let world = world().await;
    with_baht(&world).await;

    let answer = text_of(section(&page(&world, "?range=3m").await.body, "answer"));

    assert!(answer.contains("a total without it would look like money leaving"));
    assert!(answer.contains("The sums per currency are below."));
    assert!(answer.contains("Provisional: there's no total on this day"));
    assert!(!answer.contains("best number"), "there's none");
}

#[tokio::test]
async fn a_number_with_cautions_is_called_the_best_one() {
    let world = world().await;
    world.golden().await;
    world.at("2026-11-30");
    world.rebuild("2026-11-30").await;

    let answer = text_of(section(&page(&world, "?range=3m").await.body, "answer"));

    assert!(answer.contains("Provisional: this is coffer's best number"));
}

/// An account with no value that day isn't in any sum, which the sums must say.
#[tokio::test]
async fn sums_say_who_they_leave_out() {
    let world = world().await;
    let Golden { livret, .. } = world.golden().await;
    hole_today(&world, livret).await;

    let body = page(&world, "?range=3m").await.body;

    let latest = text_of(section(&body, "latest"));
    assert!(latest.contains("These sums leave out Livret, which has no value that day."));
    assert!(!latest.contains("EUR"), "its currency has nothing in it");
    assert_eq!(
        row(section(&body, "accounts"), "Livret")[0],
        "No value that day"
    );
}

#[tokio::test]
async fn no_sums_at_all_is_said() {
    let world = world().await;
    world.display("USD").await;
    let only = world.holding("Only", "USD").await;
    world.balance(only, "2026-10-26", 1_000).await;
    hole_today(&world, only).await;

    let body = page(&world, "?range=3m").await.body;

    let answer = text_of(section(&body, "answer"));
    assert!(
        !answer.contains("sums per currency are below"),
        "there are none"
    );
    let latest = section(&body, "latest");
    assert!(text_of(latest).contains("No account has a value that day."));
    assert!(!latest.contains("By currency"), "no heading over nothing");
}

/// A scale from 6,000 to 6,300 makes a small move look large.
#[tokio::test]
async fn a_scale_not_from_zero_is_said() {
    let world = world().await;
    world.golden().await;
    let body = page(&world, "?range=3m").await.body;
    assert!(text_of(section(&body, "history")).contains("The scale doesn't start at zero"));

    let world = crate::net_worth::world().await;
    world.display("USD").await;
    let moved = world.holding("Moved", "USD").await;
    world.balance(moved, "2026-10-26", -100).await;
    world.balance(moved, "2026-10-28", 100).await;
    let crossing = page(&world, "?range=3m").await.body;
    assert!(!text_of(section(&crossing, "history")).contains("The scale"));

    let world = crate::net_worth::world().await;
    world.display("USD").await;
    let flat = world.holding("Flat", "USD").await;
    world.balance(flat, "2026-10-26", 100).await;
    let body = page(&world, "?range=3m").await.body;
    assert!(
        !text_of(section(&body, "history")).contains("The scale"),
        "no scale at all"
    );
}

/// A very long number stays one: only the space before its currency can wrap, and it
/// takes a smaller size so that it fits.
#[tokio::test]
async fn a_long_number_is_kept_whole() {
    let world = world().await;
    world.display("USD").await;
    let huge = world.holding("Huge", "USD").await;
    world
        .balance(huge, "2026-10-26", -9_000_000_000_000_000_000)
        .await;

    let body = page(&world, "?range=3m").await.body;

    let answer = section(&body, "answer");
    assert!(answer.contains("hero-figure hero-longest"));
    assert!(answer.contains("<span class=\"hero-number\">-90,000,000,000,000,000.00</span> USD"));
}
