//! A bucket's section in words true of it: never that its money counts, since none of it is in
//! net worth, and never of a total or a number it doesn't show.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use super::buckets::{HELD, closed_on_the_20th, college, part};
use super::truth::hole_today;
use super::{THREE_MONTHS, page};
use crate::net_worth::{World, world};
use crate::pages::forecast::{section, text_of};

const FIRST: &str = "2026-10-01";

/// `label`'s fields for a card that is someone else's.
fn card_of(label: &str) -> Value {
    json!({
        "belongs_to": "someone_else",
        "beneficiary": label,
        "liquidity": "short_term_liability",
    })
}

/// A bucket of `label` holding one card, with these balances.
async fn card(
    world: &World,
    label: &str,
    balances: &[(&str, i64)],
) -> coffer_domain::ids::AccountId {
    let one = world
        .holding_with("Card", "credit_card", "USD", card_of(label))
        .await;
    for (on, minor) in balances {
        world.balance(one, on, *minor).await;
    }
    one
}

/// A bucket for every sentence a section can say.
async fn every_kind_of_trouble(world: &World) {
    world.display("USD").await;
    world.rates("USD", &[("2026-01-05", "1.1172")]).await;
    card(world, "In hand", &[("2026-11-01", 5_000)]).await;
    card(
        world,
        "Once in hand",
        &[(FIRST, 5_000), ("2026-10-28", -30_000)],
    )
    .await;
    let owing = card(world, "Owing", &[(FIRST, -30_000)]).await;
    closed_on_the_20th(world, owing).await;
    let kept = college(world, "Kept", "USD", Some("Paid off")).await;
    world.balance(kept, FIRST, 100_000).await;
    let paid = card(world, "Paid off", &[(FIRST, -30_000)]).await;
    closed_on_the_20th(world, paid).await;
    world.balance(paid, "2026-10-25", 0).await;
    let only = college(world, "Only", "USD", Some("All closed")).await;
    world.balance(only, FIRST, 100_000).await;
    closed_on_the_20th(world, only).await;
    let baht = college(world, "Baht", "THB", Some("No rate")).await;
    world.balance(baht, FIRST, 50_000).await;
    let holed = college(world, "Holed", "USD", Some("Hole")).await;
    world.balance(holed, FIRST, 50_000).await;
    hole_today(world, holed).await;
    let euros = college(world, "Euros", "EUR", Some("Old rate")).await;
    world.balance(euros, "2026-11-01", 100_000).await;
    let huge = college(world, "Huge", "EUR", Some("Huge")).await;
    world
        .balance(huge, "2026-11-01", 9_000_000_000_000_000_000)
        .await;
    college(world, "Empty", "USD", Some("Empty")).await;
    let gone = college(world, "Gone", "USD", Some("Stretch")).await;
    world.balance(gone, "2026-08-10", 100_000).await;
    world.at("2026-09-01");
    world.patch_holding(gone, &json!({"closed": true})).await;
    world.at(crate::net_worth::TODAY);
    let later = college(world, "Later", "USD", Some("Stretch")).await;
    world.balance(later, "2026-10-20", 200_000).await;
}

/// The whole section, attributes and all, with no word holding "count" but "account". Each kind
/// a report can reach here is asked for by name; `to_date_not_yet_filled`, which needs a table
/// behind the clock, is read in `words.rs`.
#[tokio::test]
async fn no_word_in_a_bucket_says_it_counts() {
    let world = world().await;
    every_kind_of_trouble(&world).await;

    let report = world.report(THREE_MONTHS).await;
    let reached: BTreeSet<&str> = report["buckets"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|bucket| bucket["cautions"].as_array().unwrap())
        .map(|caution| caution["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        reached,
        BTreeSet::from([
            "amount_too_large",
            "balance_stale",
            "closed_account_owing",
            "daily_value_missing",
            "liability_balance_positive",
            "nothing_counted_after_last_point",
            "rate_missing",
            "rate_stale",
        ])
    );

    let body = page(&world, "?range=3m").await.body;
    let held = section(&body, HELD);
    let without_accounts = held.to_lowercase().replace("account", "");
    assert!(
        !without_accounts.contains("count"),
        "{}",
        without_accounts
            .split("count")
            .next()
            .map_or("", |before| &before[before.len().saturating_sub(120)..])
    );
    let said = text_of(held);
    assert!(
        said.contains("Not in this total"),
        "a closed account's cell"
    );
    assert!(said.contains("Nothing in this total"), "a stretch's row");
    assert!(!said.contains("bucket's total"), "not every bucket has one");
    assert!(said.contains("This bucket has no open account"));
    assert!(!said.contains("Nothing is in"), "the table shows what is");
}

/// The household's rows keep their words beside a bucket's.
#[tokio::test]
async fn the_household_s_table_words_stay() {
    let world = world().await;
    every_kind_of_trouble(&world).await;
    let shut = world.holding("Shut", "USD").await;
    world.balance(shut, "2026-08-10", 100).await;
    world.at("2026-09-01");
    world.patch_holding(shut, &json!({"closed": true})).await;
    world.at(crate::net_worth::TODAY);
    let later = world.holding("Later", "USD").await;
    world.balance(later, "2026-10-20", 100).await;

    let body = page(&world, "?range=3m").await.body;

    assert!(text_of(section(&body, "accounts")).contains("Not counted"));
    assert!(text_of(section(&body, "history")).contains("Nothing counted"));
}

/// With no display currency: buckets with no total at all, each saying one sentence.
async fn figures_are_said(world: &World) {
    let only = world
        .holding_with(
            "Only",
            "education",
            "THB",
            json!({"beneficiary": "All closed"}),
        )
        .await;
    world.balance(only, FIRST, 100_000).await;
    closed_on_the_20th(world, only).await;
    card(world, "In hand", &[(FIRST, 5_000)]).await;
    card(
        world,
        "Once in hand",
        &[(FIRST, 5_000), ("2026-10-28", -30_000)],
    )
    .await;
    let owing = card(world, "Owing", &[(FIRST, -30_000)]).await;
    closed_on_the_20th(world, owing).await;
    let kept = college(world, "Kept", "THB", Some("Paid off")).await;
    world.balance(kept, FIRST, 100_000).await;
    let paid = card(world, "Paid off", &[(FIRST, -30_000)]).await;
    closed_on_the_20th(world, paid).await;
    world.balance(paid, "2026-10-25", 0).await;
}

fn reasons_under(body: &str, heading: &str) -> Vec<String> {
    part(section(body, HELD), heading)
        .split("<li>")
        .skip(1)
        .map(|rest| text_of(rest.split_once("</li>").unwrap().0))
        .collect()
}

fn assert_says(body: &str, heading: &str, wanted: &str) {
    let reasons = reasons_under(body, heading);
    assert!(
        reasons.iter().any(|reason| reason == wanted),
        "{wanted} is not in {reasons:?}"
    );
}

const NO_OPEN_ACCOUNT: &str = "This bucket has no open account from 20 October 2026 to 2 November \
                               2026, since every account in it had closed by then.";

#[tokio::test]
async fn with_no_display_currency_it_speaks_of_figures() {
    let world = world().await;
    figures_are_said(&world).await;

    let body = page(&world, "?range=3m").await.body;

    assert!(
        text_of(part(section(&body, HELD), "Held for All closed"))
            .starts_with("No total on 19 October 2026"),
        "no total to speak of"
    );
    assert_says(
        &body,
        "Held for All closed",
        &format!("{NO_OPEN_ACCOUNT} The figures here are from before that, not the value now."),
    );
    assert_says(
        &body,
        "Held for All closed",
        "The balance of Only is 18 days old, from 1 October 2026, so the figures may be out of \
         date.",
    );
    assert_says(
        &body,
        "Held for In hand",
        "Card is a card or loan, but shows 50.00 USD in hand rather than owed. It may have been \
         entered with the wrong sign, and it's in this bucket's figures as it stands.",
    );
    assert_says(
        &body,
        "Held for Once in hand",
        "Card showed 50.00 USD in hand on 27 October 2026, though it is a card or loan. It may \
         have been entered with the wrong sign, and that was in this bucket's figures as it \
         stood.",
    );
    assert_says(
        &body,
        "Held for Owing",
        "Card is closed but still owes 300.00 USD, which stays in this bucket's figures.",
    );
    assert_says(
        &body,
        "Held for Paid off",
        "Card is closed and owed 300.00 USD when last in this bucket's figures, on 24 October \
         2026.",
    );
    let said = text_of(section(&body, HELD));
    assert!(!said.contains("bucket's total"), "{said}");
    assert!(!said.contains("The number here"), "{said}");
    assert!(!said.contains("so the total"), "{said}");
}

/// A display currency chosen but a rate missing: as short of a total as having none.
#[tokio::test]
async fn with_a_missing_rate_it_speaks_of_figures() {
    let world = world().await;
    world.display("USD").await;
    let baht = college(&world, "Baht", "THB", Some("All closed")).await;
    world.balance(baht, FIRST, 100_000).await;
    closed_on_the_20th(&world, baht).await;

    let body = page(&world, "?range=3m").await.body;

    assert_says(
        &body,
        "Held for All closed",
        &format!("{NO_OPEN_ACCOUNT} The figures here are from before that, not the value now."),
    );
    assert_says(
        &body,
        "Held for All closed",
        "The balance of Baht is 18 days old, from 1 October 2026, so the figures may be out of \
         date.",
    );
}

/// With a total on show, "the number" and "the total" are that total.
#[tokio::test]
async fn with_a_total_it_speaks_of_the_number() {
    let world = world().await;
    world.display("USD").await;
    let only = college(&world, "Only", "USD", Some("All closed")).await;
    world.balance(only, FIRST, 100_000).await;
    closed_on_the_20th(&world, only).await;

    let body = page(&world, "?range=3m").await.body;

    assert!(
        text_of(part(section(&body, HELD), "Held for All closed"))
            .starts_with("1,000.00 USD on 19 October 2026")
    );
    assert_says(
        &body,
        "Held for All closed",
        &format!("{NO_OPEN_ACCOUNT} The number here is from before that, not the value now."),
    );
    assert_says(
        &body,
        "Held for All closed",
        "The balance of Only is 18 days old, from 1 October 2026, so the total may be out of \
         date.",
    );
}

/// The household's stale balance reads the same with no total, and whole.
#[tokio::test]
async fn the_household_s_stale_balance_keeps_its_words() {
    let world = world().await;
    let old = world.holding("Old", "USD").await;
    world.balance(old, FIRST, 100_000).await;

    let answer = text_of(section(&page(&world, "?range=3m").await.body, "answer"));

    assert!(answer.contains(
        "The balance of Old is 32 days old, from 1 October 2026, so the total may be out of date."
    ));
}
