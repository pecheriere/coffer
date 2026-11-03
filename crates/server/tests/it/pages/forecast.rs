//! The read-only forecast page through the real router: it shows what
//! `GET /api/v1/forecast` answers, in words, and changes nothing.

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use coffer_domain::identity::Role;
use coffer_server::testkit::browser::{Browser, Reply};
use coffer_store::{AuditFilter, PageRequest};
use secrecy::ExposeSecret as _;
use serde_json::{Value, json};

use crate::forecast::answer::{Golden, golden, insert_pending};
use crate::forecast::settled::id_of;
use crate::forecast::unknowns::{fed, reported};
use crate::forecast::{AS_OF, BALANCE_ON, World, outflow, usd};

const PAGE: &str = "/forecast";

/// A browser signed in as `world`'s alice.
async fn signed_in(world: &World) -> Browser {
    let session = world
        .server
        .sessions
        .create(world.alice.member)
        .await
        .unwrap();
    let mut browser = Browser::new(&world.server);
    browser.cookie = Some(session.expose_secret().to_owned());
    browser
}

/// The forecast page, which must answer 200.
async fn page(world: &World) -> Reply {
    let reply = signed_in(world).await.get(PAGE).await;
    assert_eq!(reply.status, StatusCode::OK);
    reply
}

/// What the page's `<main>` says, without its tags.
async fn words(world: &World) -> String {
    let reply = page(world).await;
    let main = &reply.body[reply.body.find("<main").unwrap()..];
    text_of(main)
}

/// `html` without its tags, each tag read as a space, and spaces collapsed.
pub(super) fn text_of(html: &str) -> String {
    let mut text = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => {
                in_tag = true;
                text.push(' ');
            }
            '>' if in_tag => in_tag = false,
            _ if !in_tag => text.push(c),
            _ => {}
        }
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The table row whose heading starts with `heading`, from its first cell on.
fn found_row<'a>(body: &'a str, heading: &str) -> &'a str {
    let found = body
        .split("<tr")
        .skip(1)
        .filter_map(|one| one.split("</tr>").next())
        .find(|one| {
            one.split_once("<th")
                .is_some_and(|(_, cell)| text_of(&format!("<th{cell}")).starts_with(heading))
        });
    let Some(found) = found else {
        panic!("the page has no such row");
    };
    found
}

/// The data cells of the table row whose heading starts with `heading`.
pub(super) fn row(body: &str, heading: &str) -> Vec<String> {
    found_row(body, heading)
        .split("<td")
        .skip(1)
        .map(|cell| text_of(&format!("<td{cell}")))
        .collect()
}

/// The `<section>` of the page with this id.
pub(super) fn section<'a>(body: &'a str, id: &str) -> &'a str {
    let from = body.find(&format!("id=\"{id}\"")).unwrap();
    let len = body[from..].find("</section>").unwrap();
    &body[from..from + len]
}

/// A cell's amount in minor units of a two-decimal currency. Only `-1,234.56` is read:
/// groups of three, two decimals and no currency, so a cell written otherwise fails.
fn cents(cell: &str) -> i64 {
    let (sign, digits) = cell.strip_prefix('-').map_or((1, cell), |rest| (-1, rest));
    let (whole, decimals) = digits.split_once('.').unwrap();
    let mut groups = whole.split(',');
    let first = groups.next().unwrap();
    assert!((1..=3).contains(&first.len()), "a leading group of 1 to 3");
    let rest: Vec<&str> = groups.collect();
    assert!(rest.iter().all(|group| group.len() == 3), "groups of three");
    assert_eq!(decimals.len(), 2, "two decimals");
    sign * format!("{first}{}{decimals}", rest.concat())
        .parse::<i64>()
        .unwrap()
}

/// The amount of the one-cell table row whose heading starts with `heading`.
fn amount(body: &str, heading: &str) -> i64 {
    let cells = row(body, heading);
    let [only] = cells.as_slice() else {
        panic!("the row must hold one amount");
    };
    cents(only)
}

/// Each month's row says what the API's month says.
fn assert_months_match(body: &str, api: &Value) {
    for month in api["months"].as_array().unwrap() {
        let line = &month["lines"][0];
        let (year, number) = month["month"].as_str().unwrap().split_once('-').unwrap();
        let name = match number {
            "11" => "November",
            "12" => "December",
            "01" => "January",
            "02" => "February",
            _ => panic!("the golden example has no such month"),
        };
        let shown: Vec<i64> = row(body, &format!("{name} {year}"))
            .iter()
            .map(|cell| cents(cell))
            .collect();
        let answered: Vec<i64> = ["outflows", "goal_accrual", "inflows", "projected_cash"]
            .iter()
            .map(|field| line[*field]["minor_units"].as_i64().unwrap())
            .collect();
        assert_eq!(shown, answered);
    }
}

fn assert_sums(body: &str, available: i64, needed: i64) {
    assert_eq!(amount(body, "Cash available"), available);
    assert_eq!(amount(body, "Cash needed"), needed);
}

#[tokio::test]
async fn page_matches_the_api_on_the_golden_example() {
    let Golden {
        world,
        checking,
        utilities,
        ..
    } = golden().await;

    let before = page(&world).await;
    let api = world.forecast().await;
    assert_eq!(api["surplus"], usd(3_836_000), "the API's number");
    assert_sums(&before.body, 6_850_000, 3_014_000);
    assert_eq!(amount(&before.body, "Planned outflows"), 914_000);
    assert_eq!(amount(&before.body, "Goals"), 1_100_000);
    assert_eq!(amount(&before.body, "Safety floor"), 1_000_000);
    assert_eq!(amount(&before.body, "Surplus"), 3_836_000);
    assert_months_match(&before.body, &api);
    assert_eq!(row(&before.body, "Cash available"), ["68,500.00"]);
    let answer = text_of(section(&before.body, "answer"));
    assert!(answer.contains("Surplus 38,360.00 USD"));
    assert!(answer.contains("Provisional"));
    assert!(answer.contains("Utilities"), "the category by name");
    assert!(answer.contains("120.00 USD"), "the amount not seen");

    world.spend(checking, "2026-11-10", 12_000, utilities).await;

    let after = page(&world).await;
    let api = world.forecast().await;
    assert_eq!(api["surplus"], usd(3_848_000), "the API's number");
    assert_sums(&after.body, 6_850_000, 3_002_000);
    assert_months_match(&after.body, &api);
    let answer = text_of(section(&after.body, "answer"));
    assert!(answer.contains("Surplus 38,480.00 USD"));
    assert!(answer.contains("Firm"));
    assert!(!answer.contains("Utilities"), "no caution is left");
}

#[tokio::test]
async fn empty_household_shows_the_reason_and_no_number() {
    let world = crate::forecast::world().await;

    let text = words(&world).await;

    assert!(text.contains("Unavailable"));
    assert!(text.contains("No safety floor is set"));
    assert!(text.contains("No account counts as cash"));
    assert!(!text.contains("0.00"));
    assert!(!text.contains("Surplus"));
}

/// A floor not set blocks the number, so the sums that would leave it out are not shown.
#[tokio::test]
async fn blocked_forecast_shows_no_partial_sums() {
    let world = crate::forecast::world().await;
    world.funded("Checking", 123_456).await;

    let text = words(&world).await;

    assert!(text.contains("No safety floor is set"));
    assert!(!text.contains("1,234.56"));
    assert!(!text.contains("Cash needed"));
}

#[tokio::test]
async fn blocker_names_what_has_no_balance() {
    let world = crate::forecast::world().await;
    world.floor(0).await;
    world
        .account_with("Rainy day", "savings", "USD", json!({}))
        .await;

    let text = words(&world).await;

    assert!(text.contains("Rainy day has no balance yet"));
}

async fn mixed() -> World {
    let world = crate::forecast::world().await;
    world.floor(0).await;
    world.funded("Checking", 100_000).await;
    let euro = world
        .account_with("Euro", "checking", "EUR", json!({}))
        .await;
    world.balance(euro, BALANCE_ON, 50_000).await;
    world
}

#[tokio::test]
async fn mixed_currencies_show_each_currency_and_no_total() {
    let world = mixed().await;

    let reply = page(&world).await;

    let answer = text_of(section(&reply.body, "answer"));
    assert!(answer.contains("USD and EUR") || answer.contains("EUR and USD"));
    assert!(answer.contains("never added together"));
    let usd_part = section(&reply.body, "currency-USD");
    let eur_part = section(&reply.body, "currency-EUR");
    assert_eq!(row(usd_part, "Surplus"), ["1,000.00 USD"]);
    assert_eq!(row(eur_part, "Surplus"), ["500.00 EUR"]);
    assert_eq!(
        row(usd_part, "November 2026"),
        ["0.00 USD", "0.00 USD", "0.00 USD", "1,000.00 USD"],
        "each cell says which currency it is in, since a phone stacks them far from the heading"
    );
    assert!(!reply.body.contains("1,500.00"), "the two are never netted");
}

#[tokio::test]
async fn shortfall_is_called_a_shortfall() {
    let world = crate::forecast::world().await;
    world.funded("Checking", 1_000).await;
    world.floor(5_000).await;

    let text = words(&world).await;

    assert!(text.contains("Shortfall 40.00 USD"));
    assert!(!text.contains("-40.00"), "a shortfall is a magnitude");
    assert!(
        !text.to_lowercase().contains("surplus"),
        "it is not called a surplus"
    );
}

/// A household holding only `currency`, with `minor_units` in one account and no floor.
async fn holding(currency: &str, minor_units: i64) -> World {
    let world = crate::forecast::world().await;
    let only = world
        .account_with("Only", "checking", currency, json!({}))
        .await;
    world.balance(only, BALANCE_ON, minor_units).await;
    world
        .settings(&json!({"floor": {"kind": "fixed",
            "amount": {"minor_units": 0, "currency": currency}}}))
        .await;
    world
}

#[tokio::test]
async fn yen_are_grouped_without_decimals() {
    let world = holding("JPY", 1_234_567).await;

    let body = page(&world).await.body;

    assert!(text_of(section(&body, "answer")).contains("Surplus 1,234,567 JPY"));
    assert_eq!(row(&body, "Cash available"), ["1,234,567"]);
    assert_eq!(row(&body, "November 2026")[3], "1,234,567");
}

#[tokio::test]
async fn dinars_are_grouped_with_three_decimals() {
    let world = holding("KWD", 1_234_567).await;

    let body = page(&world).await.body;

    assert!(text_of(section(&body, "answer")).contains("Surplus 1,234.567 KWD"));
    assert_eq!(row(&body, "Cash available"), ["1,234.567"]);
    assert_eq!(row(&body, "November 2026")[3], "1,234.567");
}

/// One currency: its heading names it, so no cell repeats it.
#[tokio::test]
async fn negative_cash_keeps_its_sign_and_a_shortfall_its_groups() {
    let world = holding("USD", -123_456_789).await;

    let body = page(&world).await.body;

    assert!(text_of(section(&body, "answer")).contains("Shortfall 1,234,567.89 USD"));
    assert_eq!(row(&body, "Cash available"), ["-1,234,567.89"]);
    assert_eq!(row(&body, "Shortfall"), ["1,234,567.89"]);
    assert_eq!(row(&body, "November 2026")[3], "-1,234,567.89");
}

/// A category that can't be read costs its name, not the page: the number is the
/// forecast's, and the sentence reads without the name.
#[tokio::test]
async fn a_failed_name_lookup_leaves_the_page_and_its_number() {
    let Golden {
        world, utilities, ..
    } = golden().await;
    world
        .server
        .corrupt_category_name(utilities.to_string().as_str())
        .await;

    let body = page(&world).await.body;

    let answer = text_of(section(&body, "answer"));
    assert!(answer.contains("Surplus 38,360.00 USD"));
    assert!(answer.contains("in a category"));
    assert!(answer.contains("120.00 USD"));
}

#[tokio::test]
async fn firm_says_what_it_means() {
    let world = crate::forecast::world().await;
    world.funded("Checking", 1_000).await;
    world.floor(0).await;

    let answer = text_of(section(&page(&world).await.body, "answer"));

    assert!(answer.contains("Firm:"));
    assert!(answer.contains("nothing to warn you about"));
}

/// The worked example with its water bill settled: the page says so in plain words, and
/// what it shows is still what the API answers.
#[tokio::test]
async fn a_settled_item_is_shown_in_plain_words() {
    let Golden { world, .. } = golden().await;
    let water = id_of(&world.forecast().await, "Water");
    world.settle(&water, "2026-11", "paid").await;

    let body = page(&world).await.body;
    let answer = text_of(section(&body, "answer"));

    assert!(answer.contains("38,480.00 USD"));
    assert!(answer.contains("Firm:"));
    assert!(
        answer.contains("Water: 120.00 USD for November 2026 isn't counted, marked paid."),
        "says what was settled"
    );
}

/// It stays read-only: nothing on the page acts on a settlement.
#[tokio::test]
async fn a_settled_item_has_nothing_to_press() {
    let Golden { world, .. } = golden().await;
    let water = id_of(&world.forecast().await, "Water");
    world.settle(&water, "2026-11", "skipped").await;

    let reply = page(&world).await;
    let main = &reply.body[reply.body.find("<main").unwrap()..];

    assert!(text_of(main).contains("marked skipped"));
    assert!(!main.contains("<form"), "no form");
    assert!(!main.contains("<button"), "no button");
}

/// A settled item's name is the household's text, so it stays text.
#[tokio::test]
async fn a_settled_items_name_is_escaped() {
    const MARKUP: &str = "<script>alert(1)</script>";
    let (world, _) = crate::forecast::settled::holding().await;
    let home = world.expense("Home").await;
    let item = world
        .item(&outflow(
            MARKUP,
            1_000,
            home,
            &json!({"type": "one_off", "on": "2026-11-20"}),
        ))
        .await;
    world.settle(&item, "2026-11", "paid").await;

    let body = page(&world).await.body;

    assert!(!body.contains(MARKUP), "the markup stays text");
    assert!(
        section(&body, "answer").contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
        "and is shown"
    );
}

#[tokio::test]
async fn nothing_settled_says_nothing_of_it() {
    let Golden { world, .. } = golden().await;

    let answer = text_of(section(&page(&world).await.body, "answer"));

    assert!(
        !answer.contains("Settled"),
        "no heading with nothing under it"
    );
}

#[tokio::test]
async fn provisional_says_what_it_means_beside_its_reasons() {
    let Golden { world, .. } = golden().await;

    let answer = text_of(section(&page(&world).await.body, "answer"));

    assert!(answer.contains("Provisional:"));
    assert!(answer.contains("could change it"));
    assert!(
        answer.contains("Utilities"),
        "the reason is with the number"
    );
}

#[tokio::test]
async fn unavailable_says_what_it_means() {
    let world = crate::forecast::world().await;

    let answer = text_of(section(&page(&world).await.body, "answer"));

    assert!(answer.contains("Unavailable:"));
    assert!(answer.contains("can't give a number"));
}

#[tokio::test]
async fn cautions_are_shown() {
    let fed = fed().await;
    fed.sync(reported("USD")).await;
    let old = fed
        .world
        .account_with("Old savings", "savings", "USD", json!({}))
        .await;
    fed.world.balance(old, "2026-11-07", 500).await;

    let answer = text_of(section(&page(&fed.world).await.body, "answer"));

    assert!(answer.contains("Surplus 15.00 USD"));
    assert!(answer.contains("Old savings"));
    assert!(answer.contains("8 days old"));
    assert!(answer.contains("Everyday savings"));
    assert!(answer.contains("Example Bank"));
    assert!(answer.contains("isn't counted"));
}

#[tokio::test]
async fn spending_newer_than_a_balance_shows_both_amounts() {
    let world = crate::forecast::world().await;
    let checking = world.funded("Checking", 100_000).await;
    world.floor(0).await;
    let food = world.expense("Food").await;
    world.spend(checking, AS_OF, 2_500, food).await;
    insert_pending(&world, checking, food, 700).await;

    let answer = text_of(section(&page(&world).await.body, "answer"));

    assert!(answer.contains("25.00 USD posted after"));
    assert!(answer.contains("7.00 USD not posted yet"));
    assert!(answer.contains("Surplus 1,000.00 USD"), "the number stays");
}

#[tokio::test]
async fn a_months_floor_of_zero_is_explained() {
    let world = crate::forecast::world().await;
    world.funded("Checking", 1_000).await;
    world
        .settings(&json!({"floor": {"kind": "months_of_outflows", "months": 2}}))
        .await;

    let answer = text_of(section(&page(&world).await.body, "answer"));

    assert!(answer.contains("2 months of recurring"));
    assert!(answer.contains("comes to 0 in USD"));
}

#[tokio::test]
async fn a_closed_card_still_owing_is_explained() {
    let world = crate::forecast::world().await;
    world.funded("Checking", 100_000).await;
    world.floor(0).await;
    let card = world
        .account_with("Old card", "credit_card", "USD", json!({}))
        .await;
    world.balance(card, BALANCE_ON, -30_000).await;
    world.close(card).await;

    let answer = text_of(section(&page(&world).await.body, "answer"));

    assert!(answer.contains("Old card is closed"));
    assert!(answer.contains("still owes 300.00 USD"));
}

#[tokio::test]
async fn expected_income_not_counted_is_labelled() {
    let Golden { world, .. } = golden().await;

    let body = page(&world).await.body;

    assert_eq!(amount(&body, "Expected income, not counted"), 2_400_000);
    assert_eq!(amount(&body, "Income counted, taken off"), 0);
    assert!(
        body.contains("Expected income (not counted)"),
        "the monthly column says so too"
    );
}

#[tokio::test]
async fn income_counted_lowers_the_need_and_loses_the_label() {
    let Golden { world, .. } = golden().await;
    world.settings(&json!({"include_income": true})).await;

    let body = page(&world).await.body;

    assert_eq!(amount(&body, "Income counted, taken off"), 2_400_000);
    assert_eq!(amount(&body, "Cash needed"), 614_000);
    assert!(!body.contains("not counted"), "it is counted");
}

#[tokio::test]
async fn partial_months_are_marked() {
    let Golden { world, .. } = golden().await;

    let body = page(&world).await.body;

    let marked = |month: &str| text_of(found_row(&body, month)).contains("part of the month");
    assert!(marked("November 2026"), "the window starts on the 15th");
    assert!(!marked("December 2026"), "a whole month");
    assert!(marked("February 2027"), "the window ends on the 15th");
}

#[tokio::test]
async fn provider_text_and_names_are_escaped() {
    const MARKUP: &str = "<script>alert(1)</script>";
    let fed = fed().await;
    let mut report = reported("USD");
    report.name = MARKUP.into();
    report.institution = Some(MARKUP.into());
    fed.sync(report).await;
    let old = fed
        .world
        .account_with(MARKUP, "savings", "USD", json!({}))
        .await;
    fed.world.balance(old, "2026-11-07", 500).await;

    let body = page(&fed.world).await.body;

    assert!(!body.contains(MARKUP), "the markup stays text");
    assert!(body.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
}

#[tokio::test]
async fn signed_out_visitor_is_sent_to_log_in() {
    let Golden { world, .. } = golden().await;

    let reply = Browser::new(&world.server).get(PAGE).await;

    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    assert_eq!(reply.header(header::LOCATION), "/login");
    assert!(!reply.body.contains("68,500.00"), "no number");
}

#[tokio::test]
async fn bearer_token_cannot_read_the_page() {
    let Golden { world, .. } = golden().await;
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
    assert!(!reply.body.contains("68,500.00"), "no number");
}

/// Six items of 10^15 every day for the 60 months the household asks for are past `i64`.
#[tokio::test]
async fn totals_too_large_show_the_error_page_and_no_number() {
    let world = crate::forecast::world().await;
    let category = world.expense("Everything").await;
    world.funded("Checking", 123_456).await;
    world.floor(0).await;
    world.settings(&json!({"default_horizon_months": 60})).await;
    let daily = json!({"type": "recurring", "every": 1, "unit": "days", "anchor_on": AS_OF});
    for _ in 0..6 {
        world
            .item(&outflow("Huge", 1_000_000_000_000_000, category, &daily))
            .await;
    }

    let reply = signed_in(&world).await.get(PAGE).await;

    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(reply.body.contains("too large to represent"), "the reason");
    assert!(!reply.body.contains("1,234.56"), "no number");
    assert!(!reply.body.contains("id=\"answer\""), "not the forecast");
}

#[tokio::test]
async fn page_has_the_default_csp_and_no_inline_script_or_style() {
    let Golden { world, .. } = golden().await;

    let reply = page(&world).await;

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

#[tokio::test]
async fn page_is_never_cached_or_sent_as_a_referrer() {
    let Golden { world, .. } = golden().await;

    let reply = page(&world).await;

    assert_eq!(reply.header(header::CACHE_CONTROL), "no-store");
    assert_eq!(reply.header(header::REFERRER_POLICY), "same-origin");
}

#[tokio::test]
async fn with_nothing_to_settle_the_page_offers_no_write_action() {
    let Golden { world, .. } = golden().await;
    let water = id_of(&world.forecast().await, "Water");
    world.settle(&water, "2026-11", "paid").await;

    let body = page(&world).await.body;

    assert_eq!(body.matches("<form").count(), 1, "only sign-out");
    assert!(body.contains("<form method=\"post\" action=\"/logout\""));
    assert_eq!(body.matches("<button").count(), 1, "only sign-out");
    assert_eq!(body.matches("<input").count(), 1, "only its token");
    let posted = signed_in(&world).await.post_form(PAGE, &[]).await;
    assert_eq!(posted.status, StatusCode::METHOD_NOT_ALLOWED);
}

/// The buttons beside a caution settle what it asks about, as the API would: the caution goes, and
/// the page says what was settled.
#[tokio::test]
async fn a_bill_the_page_asks_about_can_be_settled_from_it() {
    let Golden { world, .. } = golden().await;
    let water = id_of(&world.forecast().await, "Water");
    let mut browser = signed_in(&world).await;
    let reply = browser.get(PAGE).await;
    let action = format!(
        "/forecast/items/{}/settlements/2026-11",
        water.as_str().unwrap()
    );
    assert!(reply.body.contains(&action), "the form is there");
    let token = reply
        .body
        .split(&format!("action=\"{action}\""))
        .nth(1)
        .and_then(|form| form.split("value=\"").nth(1))
        .and_then(|rest| rest.split('"').next())
        .unwrap()
        .to_owned();

    let refused = browser
        .post_form(&action, &[("csrf", "wrong"), ("kind", "paid")])
        .await;
    assert_ne!(refused.status, StatusCode::SEE_OTHER, "a forged form fails");
    assert_eq!(world.forecast().await["status"], "provisional");

    let posted = browser
        .post_form(&action, &[("csrf", token.as_str()), ("kind", "paid")])
        .await;
    assert_eq!(posted.status, StatusCode::SEE_OTHER);

    assert_eq!(world.forecast().await["status"], "firm");
    assert!(words(&world).await.contains("isn't counted, marked paid"));
}

/// The sentence says how, for a person and for an agent.
#[tokio::test]
async fn a_bill_unpaid_from_last_month_says_how_to_settle_it() {
    let (world, _) = crate::forecast::settled::holding().await;
    let home = world.expense("Home").await;
    let item = world
        .item(&outflow(
            "Boiler",
            1_000,
            home,
            &json!({"type": "one_off", "on": "2026-10-20"}),
        ))
        .await;

    let said = words(&world).await;

    assert!(said.contains("Boiler was due on 20 October 2026"));
    assert!(said.contains(&format!(
        "PUT /api/v1/plan-items/{}/settlements/2026-10",
        item.as_str().unwrap()
    )));
    assert!(said.contains("Mark Boiler paid"));
}

#[tokio::test]
async fn nav_links_to_the_forecast_for_every_member() {
    let Golden { world, .. } = golden().await;
    let invite = world
        .server
        .identity
        .create_invite(&world.alice, Role::Member, None)
        .await
        .unwrap();
    let mut bob = Browser::new(&world.server);
    let mut key = world.server.authenticator();
    bob.enroll(&mut key, &invite, "bob", "Bob").await;
    let mut alice = signed_in(&world).await;

    for browser in [&mut alice, &mut bob] {
        let elsewhere = browser.get("/account").await;
        assert!(
            elsewhere
                .body
                .contains("<a href=\"/forecast\">Forecast</a>")
        );
        let forecast = browser.get(PAGE).await;
        assert_eq!(forecast.status, StatusCode::OK);
        assert!(
            forecast
                .body
                .contains("<a href=\"/forecast\" aria-current=\"page\">Forecast</a>")
        );
        assert_eq!(amount(&forecast.body, "Cash available"), 6_850_000);
    }
}

#[tokio::test]
async fn reading_the_page_writes_nothing() {
    let Golden { world, .. } = golden().await;
    let audited = || async {
        let listed = world
            .server
            .store
            .list_audit(
                &AuditFilter::default(),
                &PageRequest {
                    after: None,
                    limit: u32::MAX,
                },
            )
            .await;
        let Ok(listed) = listed else {
            panic!("the audit log must answer");
        };
        listed.items.len()
    };
    let before = audited().await;

    page(&world).await;

    assert_eq!(audited().await, before, "no audit entry");
    assert!(before > 0, "the household's setup was audited");
}

/// The page shows the household's defaults, whatever the address asks for.
#[tokio::test]
async fn page_takes_no_query_of_its_own() {
    let Golden { world, .. } = golden().await;

    let asked = signed_in(&world)
        .await
        .get(&format!("{PAGE}?include_income=true"))
        .await;

    assert_eq!(amount(&asked.body, "Cash needed"), 3_014_000);
}
