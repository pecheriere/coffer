//! The read-only forecast page through the real router: it shows what
//! `GET /api/v1/forecast` answers, in words, and changes nothing.

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use coffer_domain::identity::Role;
use coffer_domain::money::{Currency, format_minor_units};
use coffer_server::testkit::browser::{Browser, Reply};
use coffer_store::{AuditFilter, PageRequest};
use secrecy::ExposeSecret as _;
use serde_json::{Value, json};

use crate::forecast::answer::{Golden, golden, insert_pending};
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
fn text_of(html: &str) -> String {
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
fn row(body: &str, heading: &str) -> Vec<String> {
    found_row(body, heading)
        .split("<td")
        .skip(1)
        .map(|cell| text_of(&format!("<td{cell}")))
        .collect()
}

/// The `<section>` of the page with this id.
fn section<'a>(body: &'a str, id: &str) -> &'a str {
    let from = body.find(&format!("id=\"{id}\"")).unwrap();
    let len = body[from..].find("</section>").unwrap();
    &body[from..from + len]
}

fn dollars(minor_units: i64) -> String {
    format_minor_units(minor_units, Currency::parse("USD").unwrap())
}

/// `field` of `value`, an amount in the API's JSON, as the page writes it.
fn formatted(value: &Value, field: &str) -> String {
    let amount = &value[field];
    format_minor_units(
        amount["minor_units"].as_i64().unwrap(),
        Currency::parse(amount["currency"].as_str().unwrap()).unwrap(),
    )
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
        assert_eq!(
            row(body, &format!("{name} {year}")),
            [
                formatted(line, "outflows"),
                formatted(line, "goal_accrual"),
                formatted(line, "inflows"),
                formatted(line, "projected_cash"),
            ],
        );
    }
}

fn assert_sums(body: &str, available: i64, needed: i64) {
    assert_eq!(row(body, "Cash available"), [dollars(available)]);
    assert_eq!(row(body, "Cash needed"), [dollars(needed)]);
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
    assert_eq!(row(&before.body, "Planned outflows"), [dollars(914_000)]);
    assert_eq!(row(&before.body, "Goals"), [dollars(1_100_000)]);
    assert_eq!(row(&before.body, "Safety floor"), [dollars(1_000_000)]);
    assert_eq!(row(&before.body, "Surplus"), [dollars(3_836_000)]);
    assert_months_match(&before.body, &api);
    let answer = text_of(section(&before.body, "answer"));
    assert!(answer.contains("Surplus 38360.00 USD"));
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
    assert!(answer.contains("Surplus 38480.00 USD"));
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
    assert!(!text.contains("1234.56"));
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
    assert_eq!(row(usd_part, "Surplus"), ["1000.00"]);
    assert_eq!(row(eur_part, "Surplus"), ["500.00"]);
    assert!(!reply.body.contains("1500.00"), "the two are never netted");
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

#[tokio::test]
async fn firm_says_what_it_means() {
    let world = crate::forecast::world().await;
    world.funded("Checking", 1_000).await;
    world.floor(0).await;

    let answer = text_of(section(&page(&world).await.body, "answer"));

    assert!(answer.contains("Firm:"));
    assert!(answer.contains("nothing to warn you about"));
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
    assert!(answer.contains("Surplus 1000.00 USD"), "the number stays");
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

    assert_eq!(
        row(&body, "Expected income, not counted"),
        [dollars(2_400_000)]
    );
    assert_eq!(row(&body, "Income counted, taken off"), [dollars(0)]);
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

    assert_eq!(
        row(&body, "Income counted, taken off"),
        [dollars(2_400_000)]
    );
    assert_eq!(row(&body, "Cash needed"), [dollars(614_000)]);
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
    assert!(!reply.body.contains("68500.00"), "no number");
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
    assert!(!reply.body.contains("68500.00"), "no number");
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
    assert!(!reply.body.contains("1234.56"), "no number");
    assert!(!reply.body.contains("id=\"answer\""), "not the forecast");
}

#[tokio::test]
async fn page_has_the_default_csp_and_no_inline_script_or_style() {
    let Golden { world, .. } = golden().await;

    let reply = page(&world).await;

    assert_eq!(
        reply.header(header::CONTENT_SECURITY_POLICY),
        "default-src 'self'; script-src 'self'; style-src 'self'; frame-ancestors 'none'; \
         form-action 'self'; base-uri 'none'"
    );
    assert!(!reply.body.contains("style="), "an inline style attribute");
    assert!(!reply.body.contains("<style"), "a style element");
    for script in reply.body.split("<script").skip(1) {
        let (open, _) = script.split_once('>').unwrap();
        assert!(open.contains("src="), "an inline script");
    }
}

#[tokio::test]
async fn page_offers_no_write_action() {
    let Golden { world, .. } = golden().await;

    let body = page(&world).await.body;

    assert_eq!(body.matches("<form").count(), 1, "only sign-out");
    assert!(body.contains("<form method=\"post\" action=\"/logout\""));
    assert_eq!(body.matches("<button").count(), 1, "only sign-out");
    assert_eq!(body.matches("<input").count(), 1, "only its token");
    let posted = signed_in(&world).await.post_form(PAGE, &[]).await;
    assert_eq!(posted.status, StatusCode::METHOD_NOT_ALLOWED);
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
        assert_eq!(row(&forecast.body, "Cash available"), [dollars(6_850_000)]);
    }
}

#[tokio::test]
async fn reading_the_page_writes_nothing() {
    let Golden { world, .. } = golden().await;
    let audited = || async {
        world
            .server
            .store
            .list_audit(&AuditFilter::default(), &PageRequest::default())
            .await
            .map(|page| page.items.len())
            .ok()
    };
    let before = audited().await;

    page(&world).await;

    assert!(audited().await == before, "no audit entry");
}

/// The page shows the household's defaults, whatever the address asks for.
#[tokio::test]
async fn page_takes_no_query_of_its_own() {
    let Golden { world, .. } = golden().await;

    let asked = signed_in(&world)
        .await
        .get(&format!("{PAGE}?include_income=true"))
        .await;

    assert_eq!(row(&asked.body, "Cash needed"), [dollars(3_014_000)]);
}
