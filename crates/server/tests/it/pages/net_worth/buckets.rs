//! The page's section for money that isn't the household's (spec §8.11 B6, §8.10 N13): what the
//! API's buckets say, each on its own, and never a number that adds one to anything.

use std::collections::BTreeSet;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use coffer_domain::ids::AccountId;
use coffer_server::assets::JS_COFFER_JS;
use coffer_server::testkit::browser::Browser;
use coffer_server::testkit::{TEST_PUBLIC_URL, captured_log_text};
use secrecy::ExposeSecret as _;
use serde_json::{Value, json};

use super::truth::hole_today;
use super::{PAGE, THREE_MONTHS, alice, bob, day_heading, page, written};
use crate::net_worth::{World, world};
use crate::pages::forecast::{row, section, text_of};

const HELD: &str = "held";
const ONE_YEAR: &str = "from=2025-11-02&to=2026-11-02&interval=week";

/// An account of `kind` with a label, or with none.
async fn bucket_account(
    world: &World,
    name: &str,
    kind: &str,
    currency: &str,
    label: Option<&str>,
) -> AccountId {
    world
        .holding_with(name, kind, currency, json!({"beneficiary": label}))
        .await
}

async fn college(world: &World, name: &str, currency: &str, label: Option<&str>) -> AccountId {
    bucket_account(world, name, "education", currency, label).await
}

/// The golden household, with the Task 3 buckets beside it: 20,000.00 USD and 1,000.00 EUR for
/// Child A, and 5,000.00 USD given away.
async fn golden_with_buckets(world: &World) {
    world.golden().await;
    let fund = college(world, "College fund", "USD", Some("Child A")).await;
    world.balance(fund, "2026-10-26", 2_000_000).await;
    let utma = bucket_account(world, "UTMA", "custodial", "EUR", Some("Child A")).await;
    world.balance(utma, "2026-10-26", 100_000).await;
    let giving = bucket_account(world, "Giving", "charitable", "USD", None).await;
    world.balance(giving, "2026-10-26", 500_000).await;
}

/// The part of the held section under the `h3` reading `heading`.
fn part<'a>(held: &'a str, heading: &str) -> &'a str {
    let (_, rest) = held
        .split_once(&format!("<h3>{heading}</h3>"))
        .unwrap_or_else(|| panic!("no bucket headed {heading}"));
    rest.split_once("<h3>").map_or(rest, |(own, _)| own)
}

/// What the section's `h3`s read, in order.
fn headings(held: &str) -> Vec<String> {
    held.split("<h3>")
        .skip(1)
        .map(|rest| rest.split_once("</h3>").unwrap().0.to_owned())
        .collect()
}

/// `body` without its drawings, whose axis labels are round numbers no report holds.
fn without_charts(body: &str) -> String {
    let mut rest = body;
    let mut kept = String::new();
    while let Some((before, after)) = rest.split_once("<svg") {
        kept.push_str(before);
        rest = after.split_once("</svg>").unwrap().1;
    }
    kept.push_str(rest);
    kept
}

/// Whether `word` is an amount as the page writes it: `-1,234.56`.
fn is_figure(word: &str) -> bool {
    let unsigned = word.strip_prefix('-').unwrap_or(word);
    unsigned.split_once('.').is_some_and(|(whole, decimals)| {
        decimals.len() == 2
            && decimals.chars().all(|c| c.is_ascii_digit())
            && !whole.is_empty()
            && whole.chars().all(|c| c.is_ascii_digit() || c == ',')
    })
}

/// Every amount the page's text shows.
fn figures(body: &str) -> BTreeSet<String> {
    text_of(&without_charts(body))
        .split_whitespace()
        .map(|word| word.trim_end_matches(['.', ',', ':', ';']).to_owned())
        .filter(|word| is_figure(word))
        .collect()
}

/// Every amount anywhere in `answer`, written as the page writes it.
fn held_by_report(answer: &Value, into: &mut BTreeSet<String>) {
    match answer {
        Value::Object(fields) => {
            if let Some(minor) = fields.get("minor_units").and_then(Value::as_i64) {
                into.insert(written(minor));
            }
            for field in fields.values() {
                held_by_report(field, into);
            }
        }
        Value::Array(items) => {
            for item in items {
                held_by_report(item, into);
            }
        }
        _ => {}
    }
}

#[tokio::test]
async fn page_matches_the_api_on_the_golden_week() {
    let world = world().await;
    golden_with_buckets(&world).await;

    let body = page(&world, "?range=3m").await.body;
    let api = world.report(THREE_MONTHS).await;

    assert!(text_of(section(&body, "answer")).contains("6,317.20 USD"));
    let held = section(&body, HELD);
    assert_eq!(headings(held), ["Held for Child A", "Given away"]);
    for (index, heading, total) in [
        (0, "Held for Child A", "21,117.20 USD"),
        (1, "Given away", "5,000.00 USD"),
    ] {
        let own = part(held, heading);
        assert!(
            text_of(own).starts_with(&format!("{total} on 2 November 2026")),
            "{heading}: the latest total and its day"
        );
        let bucket = &api["buckets"][index];
        let points = bucket["points"].as_array().unwrap();
        assert_eq!(points.len(), 8, "the golden week and today");
        for point in points {
            let minor = point["total"]["minor_units"].as_i64().unwrap();
            let on = day_heading(point["on"].as_str().unwrap());
            assert_eq!(row(own, &on), [written(minor)], "{heading}: {on}");
        }
        let accounts = bucket["accounts"].as_array().unwrap().len();
        assert_eq!(
            own.matches("<tr").count(),
            points.len() + accounts + 2,
            "{heading}: a row per point and account, and two headers"
        );
    }
}

#[tokio::test]
async fn the_section_says_what_it_is() {
    let world = world().await;
    golden_with_buckets(&world).await;

    let body = page(&world, "?range=3m").await.body;

    let held = section(&body, HELD);
    assert!(held.contains("<h2>Not part of your net worth</h2>"));
    assert!(held.contains(
        "Held for someone else or given away. None of this is in the number above, and it isn't \
         added to it."
    ));
    assert!(
        body.find("id=\"accounts\"").unwrap() < body.find("id=\"held\"").unwrap(),
        "after the accounts"
    );
}

#[tokio::test]
async fn buckets_are_headed_in_the_report_s_order() {
    let world = world().await;
    world.display("USD").await;
    for (name, kind, label) in [
        ("Fund", "charitable", Some("Family fund")),
        ("Giving", "charitable", None),
        ("Named", "education", Some("Child A")),
        ("Unnamed", "education", None),
    ] {
        let one = bucket_account(&world, name, kind, "USD", label).await;
        world.balance(one, "2026-10-26", 100).await;
    }

    let body = page(&world, "").await.body;

    assert_eq!(
        headings(section(&body, HELD)),
        [
            "Held for someone else",
            "Held for Child A",
            "Given away",
            "Given away: Family fund",
        ]
    );
}

/// Each bucket's account is in its own table, with the same cells as the household's, and in
/// none of the household's.
#[tokio::test]
async fn accounts_show_in_their_own_bucket_only() {
    let world = world().await;
    golden_with_buckets(&world).await;

    let body = page(&world, "?range=3m").await.body;

    let held = section(&body, HELD);
    let child = part(held, "Held for Child A");
    assert_eq!(
        row(child, "College fund"),
        ["20,000.00 USD from 26 Oct 2026", "20,000.00", "26 Oct 2026"]
    );
    assert_eq!(
        row(child, "UTMA"),
        [
            "1,000.00 EUR from 26 Oct 2026",
            "1,117.20 at the rate of 30 Oct 2026",
            "26 Oct 2026"
        ]
    );
    for label in ["Balance", "In USD", "First day", "Total, USD"] {
        assert!(
            child.contains(&format!("data-label=\"{label}\"")),
            "{label}"
        );
    }
    assert!(!text_of(child).contains("Giving"), "another bucket's");
    assert!(!text_of(child).contains("Checking"), "the household's");
    assert!(text_of(part(held, "Given away")).contains("Giving"));
    let accounts = text_of(section(&body, "accounts"));
    for name in ["College fund", "UTMA", "Giving"] {
        assert!(!accounts.contains(name), "{name}");
    }
}

/// Two tables per bucket are told apart by whoever can't see where they are, and none of them
/// calls a bucket net worth.
#[tokio::test]
async fn each_table_is_named_for_its_bucket() {
    let world = world().await;
    golden_with_buckets(&world).await;

    let body = page(&world, "?range=3m").await.body;

    let held = section(&body, HELD);
    let child = part(held, "Held for Child A");
    assert!(child.contains("aria-label=\"Held for Child A: accounts at the latest point\""));
    assert!(child.contains("aria-label=\"Held for Child A: every period\""));
    assert!(!held.contains("aria-label=\"Net worth by period\""));
    assert!(section(&body, "history").contains("aria-label=\"Net worth by period\""));
}

/// Nothing on the page is a sum the report doesn't hold, whatever the range.
#[tokio::test]
async fn no_number_on_the_page_adds_them() {
    let world = world().await;
    golden_with_buckets(&world).await;

    for (query, range) in [("?range=3m", THREE_MONTHS), ("?range=1y", ONE_YEAR)] {
        let body = page(&world, query).await.body;
        let api = world.report(range).await;
        let mut held = BTreeSet::new();
        held_by_report(&api, &mut held);

        let shown = figures(&body);
        for wanted in ["6,317.20", "21,117.20", "5,000.00"] {
            assert!(shown.contains(wanted), "{query}: {wanted} is read");
        }
        let extra: Vec<&String> = shown.difference(&held).collect();
        assert!(extra.is_empty(), "{query}: not in the report: {extra:?}");
        for sum in ["27,434.40", "11,317.20", "26,117.20", "32,434.40"] {
            assert!(!body.contains(sum), "{query}: a sum shows, drawing or not");
        }
    }
}

/// One line on the page stays net worth.
#[tokio::test]
async fn no_chart_for_a_bucket() {
    let world = world().await;
    golden_with_buckets(&world).await;

    let body = page(&world, "?range=3m").await.body;

    assert_eq!(body.matches("chart-line\"").count(), 2, "wide and narrow");
    assert!(!section(&body, HELD).contains("<svg"));
}

#[tokio::test]
async fn the_hero_is_the_household_s() {
    let world = world().await;
    world.display("USD").await;
    let fund = college(&world, "College fund", "USD", Some("Child A")).await;
    world.balance(fund, "2026-10-26", 2_000_000).await;

    let body = page(&world, "?range=3m").await.body;

    let answer = text_of(section(&body, "answer"));
    assert!(answer.contains("No number yet"));
    assert!(answer.contains("Unavailable:"));
    assert!(!answer.contains("20,000.00"), "a bucket isn't the number");
    assert!(!body.contains("id=\"history\""));
    let own = part(section(&body, HELD), "Held for Child A");
    assert!(text_of(own).starts_with("20,000.00 USD on 2 November 2026"));
    assert_eq!(row(own, "2 November 2026"), ["20,000.00"]);
}

#[tokio::test]
async fn a_firm_bucket_has_no_status_in_words() {
    let world = world().await;
    golden_with_buckets(&world).await;

    let body = page(&world, "?range=3m").await.body;

    let held = text_of(section(&body, HELD));
    assert!(!held.contains("Firm"));
    assert!(!held.contains("nothing to warn you about"));
}

#[tokio::test]
async fn a_null_bucket_total_says_why() {
    let world = world().await;
    golden_with_buckets(&world).await;
    let baht = college(&world, "Baht fund", "THB", Some("Child B")).await;
    world.balance(baht, "2026-11-01", 50_000).await;

    let body = page(&world, "?range=3m").await.body;

    let answer = text_of(section(&body, "answer"));
    assert!(answer.contains("Firm:"), "the household's stays");
    assert!(answer.contains("6,317.20 USD"));
    assert!(!answer.contains("THB"), "not the household's trouble");

    let own = part(section(&body, HELD), "Held for Child B");
    let said = text_of(own);
    assert!(said.starts_with("No total on 2 November 2026"));
    assert!(said.contains("Provisional: there's no total on this day"));
    assert!(said.contains("THB can't be converted"));
    let cells = row(own, "2 November 2026");
    assert!(cells[0].contains("No total"), "no number");
    assert!(
        cells[0].contains("Baht fund has no exchange rate"),
        "says who"
    );
    assert_eq!(row(own, "THB"), ["500.00 THB"], "what can be said of it");
    assert!(!said.contains("leave out"), "it has a value that day");
    assert_eq!(row(own, "Baht fund")[1], "No rate");
    assert!(own.contains("<details class=\"values\" open>"), "the gap");
    let firm = part(section(&body, HELD), "Held for Child A");
    assert!(firm.contains("<details class=\"values\">"), "stays shut");
    assert!(!text_of(firm).contains("By currency"), "it has a total");
}

/// Its reasons name its own accounts: the report's top level doesn't list them.
#[tokio::test]
async fn a_bucket_s_reasons_use_its_own_names() {
    let world = world().await;
    golden_with_buckets(&world).await;
    let old = college(&world, "Old fund", "USD", Some("Child C")).await;
    world.balance(old, "2020-01-06", 100_000).await;

    let body = page(&world, "?range=3m").await.body;

    let said = text_of(part(section(&body, HELD), "Held for Child C"));
    assert!(said.contains("Provisional: this is coffer's best number"));
    assert!(said.contains("The balance of Old fund is"), "{said}");
    assert!(said.starts_with("1,000.00 USD on 2 November 2026"), "kept");
    assert!(!text_of(section(&body, "answer")).contains("Old fund"));
}

/// Nothing to convert into: each bucket's own sums per currency, and nothing across them.
#[tokio::test]
async fn no_display_currency_leaves_each_bucket_s_sums() {
    let world = world().await;
    let first = college(&world, "First", "USD", Some("Child A")).await;
    world.balance(first, "2026-10-26", 10_000).await;
    let second = college(&world, "Second", "USD", Some("Child B")).await;
    world.balance(second, "2026-10-26", 20_000).await;

    let body = page(&world, "").await.body;

    let held = section(&body, HELD);
    let own = part(held, "Held for Child A");
    let said = text_of(own);
    assert!(said.contains("No total on 2 November 2026"));
    assert!(said.contains("Unavailable:"));
    assert!(
        said.contains("The sums below are per currency"),
        "it has some"
    );
    assert_eq!(row(own, "USD"), ["100.00 USD"], "the sums it speaks of");
    assert!(!own.contains("<details"), "no total to list");
    assert_eq!(row(part(held, "Held for Child B"), "USD"), ["200.00 USD"]);
    assert!(!text_of(held).contains("300.00"), "never across buckets");
    assert!(!body.contains("300.00"));
}

/// A bucket is never in net worth, so "no account counts towards net worth" would say nothing.
#[tokio::test]
async fn a_bucket_with_no_value_says_so_in_its_own_words() {
    let world = world().await;
    golden_with_buckets(&world).await;
    college(&world, "Empty fund", "USD", Some("Child D")).await;

    let body = page(&world, "?range=3m").await.body;

    let own = part(section(&body, HELD), "Held for Child D");
    let said = text_of(own);
    assert!(said.starts_with("No total"), "{said}");
    assert!(said.contains("Unavailable:"));
    assert!(said.contains("None of these accounts has a value in this range"));
    assert!(said.contains("not a balance of zero"));
    assert!(!said.contains("net worth"), "{said}");
    assert_eq!(
        row(own, "Empty fund")[0],
        "No value in this range",
        "no day is shown to refer to"
    );
    assert!(!own.contains("<details"), "no periods");
}

/// With nothing to sum, no sums are promised.
#[tokio::test]
async fn no_display_currency_and_no_value_promises_no_sums() {
    let world = world().await;
    college(&world, "Empty fund", "USD", Some("Child A")).await;

    let body = page(&world, "").await.body;

    let said = text_of(part(section(&body, HELD), "Held for Child A"));
    assert!(said.contains("No display currency is chosen, so there is no single total."));
    assert!(!said.contains("sums"), "{said}");
    assert!(
        text_of(section(&body, "answer")).contains("The sums below"),
        "the household's stays"
    );
}

/// An account of a bucket that closed mid-range stands at the bucket's last day, not the
/// household's, on which it has no value.
#[tokio::test]
async fn a_bucket_s_accounts_stand_at_its_own_latest() {
    let world = world().await;
    world.display("USD").await;
    let kept = world.holding("Kept", "USD").await;
    world.balance(kept, "2026-08-03", 100).await;
    let gone = college(&world, "Gone", "USD", Some("Child A")).await;
    world.balance(gone, "2026-09-01", 10_000).await;
    world.at("2026-09-20");
    world.patch_holding(gone, &json!({"closed": true})).await;
    world.at(crate::net_worth::TODAY);

    let body = page(&world, "?range=3m").await.body;

    let own = part(section(&body, HELD), "Held for Child A");
    assert_eq!(
        row(own, "Gone")[..2],
        ["100.00 USD from 1 Sep 2026", "100.00"]
    );
}

/// As the household's sums do. The day is in the line above them, so here "that day" has one.
#[tokio::test]
async fn a_bucket_s_sums_say_who_they_leave_out() {
    let world = world().await;
    world.display("USD").await;
    let kept = college(&world, "Kept fund", "USD", Some("Child A")).await;
    world.balance(kept, "2026-10-01", 10_000).await;
    let holed = college(&world, "Holed fund", "USD", Some("Child A")).await;
    world.balance(holed, "2026-10-01", 20_000).await;
    hole_today(&world, holed).await;

    let body = page(&world, "?range=3m").await.body;

    let own = part(section(&body, HELD), "Held for Child A");
    assert!(text_of(own).starts_with("No total on 2 November 2026"));
    assert_eq!(row(own, "USD"), ["100.00 USD"]);
    assert!(text_of(own).contains("These sums leave out Holed fund, which has no value that day."));
    assert_eq!(row(own, "Holed fund")[0], "No value that day");
}

/// Every account of it without a row that day: nothing to sum, so no empty table of sums.
#[tokio::test]
async fn a_bucket_with_nothing_to_sum_shows_no_sums() {
    let world = world().await;
    world.display("USD").await;
    let holed = college(&world, "Holed fund", "USD", Some("Child A")).await;
    world.balance(holed, "2026-10-01", 20_000).await;
    hole_today(&world, holed).await;

    let body = page(&world, "?range=3m").await.body;

    let said = text_of(part(section(&body, HELD), "Held for Child A"));
    assert!(said.starts_with("No total on 2 November 2026"), "{said}");
    assert!(said.contains("Holed fund has no value"), "{said}");
    assert!(!said.contains("By currency"), "{said}");
}

/// Whose words a caution is read in, and the account fields that put an account there.
#[derive(Clone, Copy)]
enum Whose {
    Household,
    Bucket,
}

impl Whose {
    fn fields(self) -> Value {
        match self {
            Self::Household => json!({}),
            Self::Bucket => json!({"belongs_to": "someone_else", "beneficiary": "Child A"}),
        }
    }

    /// What the page says of this money's cautions, each in its own `li`.
    fn reasons(self, body: &str) -> Vec<String> {
        let from = match self {
            Self::Household => section(body, "answer"),
            Self::Bucket => part(section(body, HELD), "Held for Child A"),
        };
        from.split("<li>")
            .skip(1)
            .map(|rest| text_of(rest.split_once("</li>").unwrap().0))
            .collect()
    }
}

/// `Kept` holding 1,000.00 from 1 October so the range keeps its points, and a card, both
/// `whose`.
async fn kept_and_a_card(world: &World, whose: Whose) -> AccountId {
    world.display("USD").await;
    let kept = world
        .holding_with("Kept", "checking", "USD", whose.fields())
        .await;
    world.balance(kept, "2026-10-01", 100_000).await;
    let mut card = whose.fields();
    card["liquidity"] = json!("short_term_liability");
    world.holding_with("Card", "credit_card", "USD", card).await
}

async fn in_hand_now(world: &World, whose: Whose) {
    let card = kept_and_a_card(world, whose).await;
    world.balance(card, "2026-10-01", 5_000).await;
}

async fn in_hand_before(world: &World, whose: Whose) {
    let card = kept_and_a_card(world, whose).await;
    world.balance(card, "2026-10-01", 5_000).await;
    world.balance(card, "2026-10-28", -30_000).await;
}

/// Closes `holder` on 20 October, and comes back to today.
async fn closed_on_the_20th(world: &World, holder: AccountId) {
    world.at("2026-10-20");
    world.patch_holding(holder, &json!({"closed": true})).await;
    world.at(crate::net_worth::TODAY);
}

async fn closed_owing(world: &World, whose: Whose) {
    let card = kept_and_a_card(world, whose).await;
    world.balance(card, "2026-10-01", -30_000).await;
    closed_on_the_20th(world, card).await;
}

async fn closed_and_paid(world: &World, whose: Whose) {
    let card = kept_and_a_card(world, whose).await;
    world.balance(card, "2026-10-01", -30_000).await;
    closed_on_the_20th(world, card).await;
    world.balance(card, "2026-10-25", 0).await;
}

/// The only account closing: a 529, or the household's own.
async fn all_closed(world: &World, whose: Whose) {
    world.display("USD").await;
    let kind = match whose {
        Whose::Household => "checking",
        Whose::Bucket => "education",
    };
    let only = world
        .holding_with("Only", kind, "USD", whose.fields())
        .await;
    world.balance(only, "2026-10-01", 100_000).await;
    closed_on_the_20th(world, only).await;
}

/// Each of these says the money counts, which in a bucket would contradict "None of this is in
/// the number above". The household's sentences are pinned whole, so they can't move with them.
macro_rules! caution_reads {
    ($name:ident, $setup:ident, household: $household:literal, bucket: $bucket:literal $(,)?) => {
        #[tokio::test]
        async fn $name() {
            for (whose, wanted) in [(Whose::Household, $household), (Whose::Bucket, $bucket)] {
                let world = world().await;
                $setup(&world, whose).await;

                let reasons = whose.reasons(&page(&world, "?range=3m").await.body);

                assert!(
                    reasons.iter().any(|reason| reason == wanted),
                    "{wanted} is not in {reasons:?}"
                );
            }
        }
    };
}

caution_reads!(
    a_card_in_hand_now_reads,
    in_hand_now,
    household: "Card is a card or loan, but shows 50.00 USD in hand rather than owed. It may have \
                been entered with the wrong sign, and it counts as it stands.",
    bucket: "Card is a card or loan, but shows 50.00 USD in hand rather than owed. It may have \
             been entered with the wrong sign, and it's in this bucket's total as it stands.",
);

caution_reads!(
    a_card_once_in_hand_reads,
    in_hand_before,
    household: "Card showed 50.00 USD in hand on 27 October 2026, though it is a card or loan. It \
                may have been entered with the wrong sign, and that counted as it stood.",
    bucket: "Card showed 50.00 USD in hand on 27 October 2026, though it is a card or loan. It \
             may have been entered with the wrong sign, and that was in this bucket's total as \
             it stood.",
);

caution_reads!(
    a_closed_card_still_owing_reads,
    closed_owing,
    household: "Card is closed but still owes 300.00 USD, which keeps counting.",
    bucket: "Card is closed but still owes 300.00 USD, which stays in this bucket's total.",
);

caution_reads!(
    a_closed_card_paid_off_reads,
    closed_and_paid,
    household: "Card is closed and owed 300.00 USD when last counted, on 24 October 2026.",
    bucket: "Card is closed and owed 300.00 USD when last in this bucket's total, on 24 October \
             2026.",
);

caution_reads!(
    every_account_closed_reads,
    all_closed,
    household: "Nothing counts from 20 October 2026 to 2 November 2026, since every account \
                counted by then has closed. The number here is from before that, not the value \
                now.",
    bucket: "Nothing is in this bucket from 20 October 2026 to 2 November 2026, since every \
             account in it by then has closed. The number here is from before that, not the \
             value now.",
);

/// No day of the household's to look at: what the bucket holds on its own latest is still said
/// in the present.
#[tokio::test]
async fn the_present_tense_is_of_the_bucket_s_latest_with_no_household() {
    let world = world().await;
    in_hand_now(&world, Whose::Bucket).await;

    let reasons = Whose::Bucket.reasons(&page(&world, "?range=3m").await.body);

    assert!(
        reasons
            .iter()
            .any(|reason| reason.contains("but shows 50.00 USD in hand")),
        "{reasons:?}"
    );
}

/// The household's latest is earlier, when the card was still in hand: on the bucket's own, it
/// no longer is.
#[tokio::test]
async fn the_past_tense_is_of_the_bucket_s_latest_past_the_household_s() {
    let world = world().await;
    in_hand_before(&world, Whose::Bucket).await;
    let closing = world.holding("Closing", "USD").await;
    world.balance(closing, "2026-10-01", 100).await;
    closed_on_the_20th(&world, closing).await;

    let reasons = Whose::Bucket.reasons(&page(&world, "?range=3m").await.body);

    assert!(
        reasons
            .iter()
            .any(|reason| reason.contains("Card showed 50.00 USD in hand on 27 October 2026")),
        "{reasons:?}"
    );
}

/// The table runs over the bucket's own points, not the household's.
#[tokio::test]
async fn a_bucket_s_table_starts_at_its_first_value() {
    let world = world().await;
    golden_with_buckets(&world).await;
    let late = college(&world, "Late fund", "USD", Some("Child E")).await;
    world.balance(late, "2026-10-30", 100_000).await;

    let body = page(&world, "?range=3m").await.body;

    let own = part(section(&body, HELD), "Held for Child E");
    assert_eq!(row(own, "30 October 2026"), ["1,000.00"]);
    assert!(!text_of(own).contains("29 October 2026"));
    assert_eq!(
        row(&body, "29 October 2026"),
        ["5,813.00"],
        "the household's"
    );
}

/// The periods the bucket left out between two of its points are said, in the range's lengths.
#[tokio::test]
async fn a_stretch_with_nothing_in_a_bucket_is_said() {
    let world = world().await;
    world.display("USD").await;
    let kept = world.holding("Kept", "USD").await;
    world.balance(kept, "2026-08-01", 100).await;
    let gone = college(&world, "Gone", "USD", Some("Child A")).await;
    world.balance(gone, "2026-08-10", 100_000).await;
    world.at("2026-09-01");
    world.patch_holding(gone, &json!({"closed": true})).await;
    world.at("2026-11-02");
    let later = college(&world, "Later", "USD", Some("Child A")).await;
    world.balance(later, "2026-10-20", 200_000).await;

    let body = page(&world, "?range=1y").await.body;

    let own = part(section(&body, HELD), "Held for Child A");
    assert!(row(own, "Week to 4 October 2026")[0].contains("Nothing counted"));
    assert_eq!(row(own, "Week to 1 November 2026"), ["2,000.00"]);
    assert_eq!(
        row(&body, "Week to 4 October 2026"),
        ["1.00"],
        "the household's"
    );
}

#[tokio::test]
async fn no_buckets_no_section() {
    let world = world().await;
    world.golden().await;

    let body = page(&world, "?range=3m").await.body;

    assert!(!body.contains("id=\"held\""));
    assert!(!body.contains("Not part of your net worth"));
}

#[tokio::test]
async fn names_are_escaped() {
    const MARKUP: &str = "<script>alert(\"1\")</script>";
    const SAFE: &str = "&lt;script&gt;alert(&quot;1&quot;)&lt;/script&gt;";
    let world = world().await;
    world.display("USD").await;
    let named = college(&world, MARKUP, "THB", Some(MARKUP)).await;
    world.balance(named, "2026-10-26", 50_000).await;
    let gift = bucket_account(&world, "Gift", "charitable", "USD", Some(MARKUP)).await;
    world.balance(gift, "2026-10-26", 50_000).await;

    let body = page(&world, "?range=3m").await.body;

    assert!(!body.contains(MARKUP), "the markup stays text");
    let held = section(&body, HELD);
    let own = part(held, &format!("Held for {SAFE}"));
    assert!(own.contains(&format!("aria-label=\"Held for {SAFE}: every period\"")));
    assert!(
        own.contains(&format!("{SAFE} has no exchange rate")),
        "named"
    );
    assert!(held.contains(&format!("<h3>Given away: {SAFE}</h3>")));
}

#[tokio::test]
async fn a_token_can_t_read_the_page() {
    let world = world().await;
    golden_with_buckets(&world).await;
    let request = Request::builder()
        .uri(PAGE)
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", world.admin.expose_secret()),
        )
        .body(Body::empty())
        .unwrap();

    let reply = Browser::new(&world.server).send(request).await;

    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    assert_eq!(reply.header(header::LOCATION), "/login");
    assert!(!reply.body.contains("Child A"), "no label");
    assert!(!reply.body.contains("21,117.20"), "no number");
}

#[tokio::test]
async fn page_offers_no_new_write_action() {
    let world = world().await;
    golden_with_buckets(&world).await;

    let body = page(&world, "?range=3m").await.body;

    assert_eq!(body.matches("<form").count(), 1, "only sign-out");
    assert_eq!(body.matches("<button").count(), 1, "only sign-out");
    assert_eq!(body.matches("<input").count(), 1, "only its token");
    for tag in ["<form", "<button", "<input", "<select", "<textarea"] {
        assert!(!section(&body, HELD).contains(tag), "{tag}");
    }
    let posted = alice(&world).await.post_form(PAGE, &[]).await;
    assert_eq!(posted.status, StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn page_has_the_default_csp_and_no_inline_script_or_style() {
    let world = world().await;
    golden_with_buckets(&world).await;

    let reply = page(&world, "?range=3m").await;

    assert!(reply.body.contains("id=\"held\""), "the section is there");
    assert_eq!(
        reply.header(header::CONTENT_SECURITY_POLICY),
        "default-src 'self'; script-src 'self'; style-src 'self'; frame-ancestors 'none'; \
         form-action 'self'; base-uri 'none'; report-uri /browser-reports"
    );
    assert!(!reply.body.contains("style="), "an inline style attribute");
    assert!(!reply.body.contains("<style"), "a style element");
    for script in reply.body.split("<script").skip(1) {
        let (open, _) = script.split_once('>').unwrap();
        assert!(open.contains("src="), "an inline script");
    }
}

/// The page now holds a child's name, and stays out of the table of pages a report's message may
/// be logged from.
#[tokio::test]
async fn browser_reports_withhold_this_page() {
    const LINE: u32 = 7_000_011;
    let world = world().await;
    let body = json!({
        "kind": "error",
        "message": "cannot read Child A",
        "script": JS_COFFER_JS.url,
        "page": PAGE,
        "line": LINE,
    });
    let mut browser = Browser::new(&world.server);
    let request = browser
        .builder(Method::POST, "/browser-reports")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ORIGIN, TEST_PUBLIC_URL)
        .body(Body::from(body.to_string()))
        .unwrap();

    browser.send(request).await;

    let wanted = format!("line={LINE}");
    let log = captured_log_text();
    let event = log
        .lines()
        .find(|event| event.split_whitespace().any(|field| field == wanted));
    let Some(event) = event else {
        panic!("the report must be logged");
    };
    assert!(event.contains("page=\"other\""));
    assert!(event.contains("client_message=\"[withheld]\""));
    assert!(!event.contains("Child A"));
}

/// A session answers for the household, and a member's sees what an admin's does.
#[tokio::test]
async fn a_session_sees_every_bucket() {
    let world = world().await;
    golden_with_buckets(&world).await;

    let admin = alice(&world).await.get(PAGE).await.body;
    let member = bob(&world).await.get(PAGE).await.body;

    let held = section(&member, HELD);
    assert_eq!(headings(held), ["Held for Child A", "Given away"]);
    assert!(text_of(part(held, "Held for Child A")).contains("21,117.20 USD"));
    assert_eq!(held, section(&admin, HELD));
}
