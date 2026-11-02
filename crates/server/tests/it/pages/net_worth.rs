//! The net worth page through the real router: it shows what
//! `GET /api/v1/reports/net-worth` answers, in words and a chart, and changes nothing.

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use coffer_domain::fx::FxFetchStatus;
use coffer_domain::identity::Role;
use coffer_server::assets::JS_COFFER_JS;
use coffer_server::testkit::browser::{Browser, Reply};
use coffer_server::testkit::{TEST_PUBLIC_URL, captured_log_text};
use coffer_store::{AuditFilter, PageRequest};
use secrecy::ExposeSecret as _;
use serde_json::{Value, json};

use super::forecast::{row, section, text_of};
use crate::net_worth::{Golden, World, unbuilt, world};

const PAGE: &str = "/net-worth";
const HOUSEHOLD: &str = "/admin/household";
const SET_CURRENCY: &str = "/admin/household/display-currency";
/// The three months the `3m` range covers on the golden example's day, by day.
const THREE_MONTHS: &str = "from=2026-08-02&to=2026-11-02&interval=day";

/// A browser signed in as `world`'s alice, an admin.
async fn alice(world: &World) -> Browser {
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

/// A browser signed in as bob, a member without the household's keys.
async fn bob(world: &World) -> Browser {
    let invite = world
        .server
        .identity
        .create_invite(&world.alice, Role::Member, None)
        .await
        .unwrap();
    let mut bob = Browser::new(&world.server);
    let mut key = world.server.authenticator();
    bob.enroll(&mut key, &invite, "bob", "Bob").await;
    bob
}

/// The page at `query`, which must answer 200.
async fn page(world: &World, query: &str) -> Reply {
    let reply = alice(world).await.get(&format!("{PAGE}{query}")).await;
    assert_eq!(reply.status, StatusCode::OK);
    reply
}

/// What the report's point says in the page's table, formatted as the page formats it:
/// the API's minor units, with the two decimals and groups of three written in.
fn written(minor: i64) -> String {
    let digits = minor.unsigned_abs().to_string();
    let digits = format!("{digits:0>3}");
    let (whole, cents) = digits.split_at(digits.len() - 2);
    let mut grouped = String::new();
    for (index, digit) in whole.char_indices() {
        if index > 0 && (whole.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    let sign = if minor < 0 { "-" } else { "" };
    format!("{sign}{grouped}.{cents}")
}

/// The heading of a day's row: `2 November 2026`.
fn day_heading(on: &str) -> String {
    let date: jiff::civil::Date = on.parse().unwrap();
    date.strftime("%-d %B %Y").to_string()
}

/// The line of the page's first drawing.
fn line_of(body: &str) -> String {
    let (_, rest) = body.split_once("chart-line\" d=\"").unwrap();
    rest.split_once('"').unwrap().0.to_owned()
}

#[tokio::test]
async fn page_matches_the_api_on_the_golden_week() {
    let world = world().await;
    let Golden { .. } = world.golden().await;

    let reply = page(&world, "?range=3m").await;
    let api = world.report(THREE_MONTHS).await;

    let answer = text_of(section(&reply.body, "answer"));
    assert!(
        answer.contains("6,317.20 USD"),
        "the hero is the API's last"
    );
    assert!(answer.contains("Firm:"));
    let points = api["points"].as_array().unwrap();
    assert_eq!(points.len(), 8, "the golden week and today");
    for point in points {
        let minor = point["total"]["minor_units"].as_i64().unwrap();
        let heading = day_heading(point["on"].as_str().unwrap());
        assert_eq!(row(&reply.body, &heading), [written(minor)]);
    }
    let latest = points.last().unwrap();
    let parts = text_of(section(&reply.body, "latest"));
    let class = |index: usize| latest["by_liquidity"][index]["amount"]["minor_units"].as_i64();
    assert!(parts.contains(&format!("Cash {}", written(class(0).unwrap()))));
    assert!(parts.contains(&format!(
        "Cards and short-term debts {}",
        written(class(1).unwrap())
    )));
    assert!(parts.contains("1,000.00 EUR"), "by currency");
    assert!(parts.contains("5,200.00 USD"), "by currency");
}

#[tokio::test]
async fn a_missing_rate_is_a_gap_named_in_the_table_and_in_words() {
    let world = world().await;
    world.golden().await;
    let baht = world.holding("Baht account", "THB").await;
    world.balance(baht, "2026-11-01", 50_000).await;

    let body = page(&world, "?range=3m").await.body;
    let api = world.report(THREE_MONTHS).await;

    assert_eq!(
        api["points"][6]["total"],
        Value::Null,
        "the API has no total"
    );
    assert_eq!(api["points"][7]["total"], Value::Null);
    let cells = row(&body, "2 November 2026");
    let [only] = cells.as_slice() else {
        panic!("one cell for the total");
    };
    assert!(only.contains("No total"), "no number");
    assert!(
        only.contains("Baht account has no exchange rate"),
        "says who"
    );
    assert!(!only.contains("0.00"), "never a zero");
    assert_eq!(
        row(&body, "31 October 2026"),
        ["6,317.20"],
        "the day before"
    );
    let line = line_of(&body);
    assert_eq!(line.matches('M').count(), 1, "the line stops");
    assert_eq!(line.matches('L').count(), 5, "at the last total");
    assert!(body.contains("chart-gap"), "the gap is shown");
    let answer = text_of(section(&body, "answer"));
    assert!(answer.contains("Provisional:"));
    assert!(answer.contains("THB can't be converted"), "the reason");
    assert!(answer.contains("isn't money leaving"));
    assert!(
        answer.contains("No total on 2 November 2026"),
        "no hero number"
    );
    assert!(
        !answer.contains("6,317.20"),
        "an old number isn't the answer"
    );
}

/// Days with no total between days with one: the line breaks and resumes, and what has
/// a total keeps it.
#[tokio::test]
async fn a_gap_in_the_middle_is_not_bridged() {
    let world = world().await;
    world.golden().await;
    let huge = world.holding("Huge", "EUR").await;
    world
        .balance(huge, "2026-10-28", 9_000_000_000_000_000_000)
        .await;
    world.balance(huge, "2026-10-30", 100_000).await;

    let body = page(&world, "?range=3m").await.body;

    let line = line_of(&body);
    assert_eq!(line.matches('M').count(), 2, "the line breaks and resumes");
    assert_eq!(
        line.matches('L').count(),
        1 + 3,
        "and doesn't cross the gap"
    );
    assert!(row(&body, "29 October 2026")[0].contains("Huge is too large to convert"));
    assert!(row(&body, "29 October 2026")[0].contains("No total"));
    assert_eq!(row(&body, "27 October 2026"), ["6,115.00"]);
    assert_eq!(row(&body, "30 October 2026"), ["7,434.40"], "resumes");
}

/// An account gone and the next not yet there: no period holds anything between them, and
/// a line across that stretch would say there was a value all along.
#[tokio::test]
async fn a_stretch_with_nothing_counted_breaks_the_line() {
    let world = world().await;
    world.display("USD").await;
    let gone = world.holding("Gone", "USD").await;
    world.balance(gone, "2026-08-10", 100_000).await;
    world.at("2026-09-01");
    world.patch_holding(gone, &json!({"closed": true})).await;
    world.at("2026-11-02");
    let later = world.holding("Later", "USD").await;
    world.balance(later, "2026-10-20", 200_000).await;
    world.balance(later, "2026-10-30", 210_000).await;

    let body = page(&world, "?range=1y").await.body;

    assert_eq!(line_of(&body).matches('M').count(), 2, "the line breaks");
    assert!(row(&body, "Week to 4 October 2026")[0].contains("Nothing counted"));
}

#[tokio::test]
async fn an_arrival_is_explained() {
    let world = world().await;
    world.golden().await;
    let late = world.holding("New savings", "USD").await;
    world.balance(late, "2026-10-29", 10_000).await;

    let body = page(&world, "?range=3m").await.body;

    let history = text_of(section(&body, "history"));
    assert!(
        history.contains("New savings first has a value on 29 October 2026"),
        "a jump reads as an account arriving"
    );
}

#[tokio::test]
async fn a_closing_is_explained() {
    let world = world().await;
    let Golden { checking, .. } = world.golden().await;
    world.at("2026-10-29");
    world
        .patch_holding(checking, &json!({"closed": true}))
        .await;
    world.at("2026-11-02");

    let body = page(&world, "?range=3m").await.body;

    let history = text_of(section(&body, "history"));
    assert!(history.contains("Checking closed on 29 October 2026"));
}

#[tokio::test]
async fn accounts_show_balance_dates_and_rate() {
    let world = world().await;
    world.golden().await;

    let body = page(&world, "?range=3m").await.body;

    let table = section(&body, "accounts");
    assert_eq!(
        row(table, "Checking"),
        ["5,500.00 USD from 30 Oct 2026", "5,500.00", "26 Oct 2026"],
        "no rate is involved"
    );
    assert_eq!(
        row(table, "Livret"),
        [
            "1,000.00 EUR from 26 Oct 2026",
            "1,117.20 at the rate of 30 Oct 2026",
            "26 Oct 2026"
        ]
    );
    assert_eq!(
        row(table, "Card")[0],
        "-300.00 USD from 28 Oct 2026",
        "owed keeps its sign"
    );
    assert!(text_of(table).contains("Side"), "visible, marked");
    assert!(row(table, "Side").iter().all(|cell| cell == "Not counted"));
}

#[tokio::test]
async fn a_negative_net_worth_keeps_its_sign_and_groups() {
    let world = world().await;
    world.display("USD").await;
    let loan = world.holding("Loan", "USD").await;
    world.balance(loan, "2026-10-26", -123_456_789).await;

    let body = page(&world, "?range=3m").await.body;

    assert!(text_of(section(&body, "answer")).contains("-1,234,567.89 USD"));
    assert_eq!(row(&body, "2 November 2026"), ["-1,234,567.89"]);
}

#[tokio::test]
async fn the_status_says_what_it_means() {
    let world = world().await;
    world.golden().await;
    let firm = text_of(section(&page(&world, "").await.body, "answer"));
    assert!(firm.contains("Firm:"));
    assert!(firm.contains("nothing to warn you about"));

    let empty = crate::net_worth::world().await;
    let answer = text_of(section(&page(&empty, "").await.body, "answer"));
    assert!(answer.contains("Unavailable:"));
    assert!(answer.contains("can't give a number"));
}

/// A word per blocker reached through the page; the rest are read in `words.rs`, whose
/// tests fail for a kind with no sample.
#[tokio::test]
async fn each_blocker_and_caution_has_words() {
    let world = unbuilt(coffer_server::testkit::TestServer::new().await).await;
    let answer = text_of(section(&page(&world, "").await.body, "answer"));
    assert!(answer.contains("working out its history again"));
    assert!(!answer.contains("rebuilding"), "not the wire name");

    let world = crate::net_worth::world().await;
    world.display("USD").await;
    let answer = text_of(section(&page(&world, "").await.body, "answer"));
    assert!(answer.contains("which is not a net worth of zero"));
    assert!(!answer.contains("no_counted"), "not the wire name");

    let world = crate::net_worth::world().await;
    world.display("USD").await;
    world.rates("USD", &[("2026-10-26", "1.1188")]).await;
    let baht = world.holding("Baht", "THB").await;
    world.balance(baht, "2026-10-26", 50_000).await;
    world
        .fetch_failed("THB", FxFetchStatus::NotPublished, None)
        .await;
    let answer = text_of(section(&page(&world, "").await.body, "answer"));
    assert!(answer.contains("doesn't publish it"));
    assert!(!answer.contains("rate_missing"), "not the wire name");
    assert!(!answer.contains("not_published"), "not the wire name");
}

#[tokio::test]
async fn no_display_currency_shows_sums_and_a_way_forward() {
    let world = world().await;
    let usd = world.holding("Dollars", "USD").await;
    world.balance(usd, "2026-10-26", 100_000).await;
    let eur = world.holding("Euros", "EUR").await;
    world.balance(eur, "2026-10-26", 50_000).await;

    let admin = page(&world, "").await.body;
    let answer = section(&admin, "answer");
    assert!(
        answer.contains("href=\"/admin/household\""),
        "a way forward"
    );
    assert!(text_of(answer).contains("No display currency is chosen"));
    let sums = text_of(section(&admin, "latest"));
    assert!(sums.contains("1,000.00 USD"));
    assert!(sums.contains("500.00 EUR"));
    assert!(!admin.contains("<svg class=\"chart"), "no chart");
    assert!(!admin.contains("1,500.00"), "never netted");

    let mut member = bob(&world).await;
    let shown = member.get(PAGE).await.body;
    let answer = section(&shown, "answer");
    assert!(
        !answer.contains("href=\"/admin/household\""),
        "not for a member"
    );
    assert!(text_of(answer).contains("An admin can choose one"));
    assert!(text_of(section(&shown, "latest")).contains("500.00 EUR"));
}

#[tokio::test]
async fn names_are_escaped() {
    const MARKUP: &str = "<script>alert(1)</script>";
    let world = world().await;
    world.display("USD").await;
    let named = world.holding(MARKUP, "THB").await;
    world.balance(named, "2026-10-26", 50_000).await;

    let body = page(&world, "?range=3m").await.body;

    assert!(!body.contains(MARKUP), "the markup stays text");
    let table = section(&body, "accounts");
    assert!(table.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(section(&body, "history").contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
}

#[tokio::test]
async fn signed_out_visitor_is_sent_to_log_in() {
    let world = world().await;
    world.golden().await;

    let reply = Browser::new(&world.server).get(PAGE).await;

    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    assert_eq!(reply.header(header::LOCATION), "/login");
    assert!(!reply.body.contains("6,317.20"), "no number");
}

#[tokio::test]
async fn bearer_token_cannot_read_the_page() {
    let world = world().await;
    world.golden().await;
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
    assert!(!reply.body.contains("6,317.20"), "no number");
}

#[tokio::test]
async fn page_has_the_default_csp_and_no_inline_script_or_style() {
    let world = world().await;
    world.golden().await;

    let reply = page(&world, "?range=3m").await;

    assert!(
        reply.body.contains("<svg class=\"chart"),
        "the chart is drawn"
    );
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
    let world = world().await;
    world.golden().await;

    let reply = page(&world, "").await;

    assert_eq!(reply.header(header::CACHE_CONTROL), "no-store");
    assert_eq!(reply.header(header::REFERRER_POLICY), "same-origin");
}

#[tokio::test]
async fn page_offers_no_write_action() {
    let world = world().await;
    world.golden().await;

    let body = page(&world, "").await.body;

    assert_eq!(body.matches("<form").count(), 1, "only sign-out");
    assert!(body.contains("<form method=\"post\" action=\"/logout\""));
    assert_eq!(body.matches("<button").count(), 1, "only sign-out");
    assert_eq!(body.matches("<input").count(), 1, "only its token");
    let posted = alice(&world).await.post_form(PAGE, &[]).await;
    assert_eq!(posted.status, StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn bad_range_is_400() {
    const SENT: &str = "nope";
    let world = world().await;
    world.golden().await;

    for query in [
        format!("?range=%3Cb%3E{SENT}%3C/b%3E"),
        "?range=".to_owned(),
        "?range=1Y".to_owned(),
        "?range=3m&range=1y".to_owned(),
        "?from=1970-01-01".to_owned(),
    ] {
        let reply = alice(&world).await.get(&format!("{PAGE}{query}")).await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{query}");
        assert!(reply.header(header::CONTENT_TYPE).starts_with("text/html"));
        assert!(!reply.body.contains(SENT), "the refusal does not echo");
        assert!(!reply.body.contains("6,317.20"), "no number");
    }
}

#[tokio::test]
async fn ranges_link_to_each_other() {
    let world = world().await;
    world.golden().await;

    for (query, current) in [
        ("", "1y"),
        ("?range=3m", "3m"),
        ("?range=1y", "1y"),
        ("?range=5y", "5y"),
        ("?range=all", "all"),
    ] {
        let body = page(&world, query).await.body;
        for range in ["3m", "1y", "5y", "all"] {
            let marked = if range == current {
                " aria-current=\"page\""
            } else {
                ""
            };
            assert!(
                body.contains(&format!("<a href=\"/net-worth?range={range}\"{marked}>")),
                "{query}: {range}"
            );
        }
    }
    let body = page(&world, "").await.body;
    let (before_chart, _) = body.split_once("<svg class=\"chart").unwrap();
    assert!(before_chart.contains("?range=3m"), "above the chart");
}

/// Each range asks for what the plan says: the same query through the API gives the same
/// numbers.
#[tokio::test]
async fn each_range_shows_its_periods() {
    let world = world().await;
    world.golden().await;

    let by_week = page(&world, "").await.body;
    let api = world
        .report("from=2025-11-02&to=2026-11-02&interval=week")
        .await;
    assert_eq!(api["points"].as_array().unwrap().len(), 2);
    assert_eq!(row(&by_week, "Week to 1 November 2026"), ["6,317.20"]);
    assert!(text_of(&by_week).contains("part of the week"), "today's");

    let by_month = page(&world, "?range=5y").await.body;
    assert_eq!(row(&by_month, "October 2026"), ["6,317.20"]);
    assert!(text_of(&by_month).contains("to 2 November 2026"), "partial");
}

#[tokio::test]
async fn all_range_stays_under_the_point_limit() {
    let world = world().await;
    world.display("USD").await;
    let old = world.holding("Old", "USD").await;
    world.balance(old, "1970-01-01", 1_000).await;

    let reply = page(&world, "?range=all").await;

    let api = world
        .report("from=1970-01-01&to=2026-11-02&interval=month")
        .await;
    let points = api["points"].as_array().unwrap().len();
    assert_eq!(points, 683, "every month from 1970");
    assert_eq!(row(&reply.body, "January 1970"), ["10.00"]);
    assert_eq!(row(&reply.body, "October 2026"), ["10.00"]);
}

#[tokio::test]
async fn nav_links_to_net_worth_for_every_member() {
    let world = world().await;
    world.golden().await;
    let mut alice = alice(&world).await;
    let mut bob = bob(&world).await;

    for browser in [&mut alice, &mut bob] {
        let elsewhere = browser.get("/account").await;
        assert!(
            elsewhere
                .body
                .contains("<a href=\"/net-worth\">Net worth</a>")
        );
        let shown = browser.get(PAGE).await;
        assert_eq!(shown.status, StatusCode::OK);
        assert!(
            shown
                .body
                .contains("<a href=\"/net-worth\" aria-current=\"page\">Net worth</a>")
        );
        assert!(text_of(section(&shown.body, "answer")).contains("6,317.20 USD"));
    }
}

#[tokio::test]
async fn reading_the_page_writes_nothing() {
    let world = world().await;
    world.golden().await;
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

    page(&world, "").await;
    page(&world, "?range=all").await;

    assert_eq!(audited().await, before, "no audit entry");
    assert!(before > 0, "the household's setup was audited");
}

/// The page shows amounts and names, so it stays out of the table of pages a report's
/// message may be logged from.
#[tokio::test]
async fn browser_reports_withhold_this_page() {
    const LINE: u32 = 7_000_007;
    let world = world().await;
    let body = json!({
        "kind": "error",
        "message": "cannot read 1234.56",
        "script": JS_COFFER_JS.url,
        "page": PAGE,
        "line": LINE,
    });
    let mut browser = Browser::new(&world.server);
    let request = browser
        .builder(axum::http::Method::POST, "/browser-reports")
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
    assert!(!event.contains("1234.56"));
}

/// Posts `fields` to the display currency form with `browser`'s token prepended.
async fn set_currency(browser: &mut Browser, fields: &[(&str, &str)]) -> Reply {
    let csrf = browser.get("/account").await.csrf();
    let mut all = vec![("csrf", csrf.as_str())];
    all.extend_from_slice(fields);
    browser.post_form(SET_CURRENCY, &all).await
}

fn has_entry(events: &[coffer_store::AuditEvent], action: &str) -> bool {
    events.iter().any(|event| event.action == action)
}

#[tokio::test]
async fn admin_sets_the_display_currency() {
    let world = world().await;
    let Golden { .. } = world.golden().await;
    let mut admin = alice(&world).await;
    let before = world.settings_events().await.len();

    let form = admin.get(HOUSEHOLD).await.body;
    assert!(form.contains(&format!("action=\"{SET_CURRENCY}\"")));
    assert!(
        form.contains("<option value=\"USD\" selected>"),
        "the current one"
    );
    assert!(form.contains("recomputes"), "says what changing it does");

    let reply = set_currency(&mut admin, &[("display_currency", "EUR")]).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    assert_eq!(reply.header(header::LOCATION), HOUSEHOLD);

    let events = world.settings_events().await;
    assert_eq!(events.len(), before + 1, "audited once");
    assert!(
        has_entry(&events, "household.settings"),
        "by the same service"
    );
    let report = world.report(THREE_MONTHS).await;
    assert_eq!(report["display_currency"], "EUR", "the report now converts");
    assert_eq!(report["points"][4]["total"]["minor_units"], 565_449);
    assert!(
        admin
            .get(HOUSEHOLD)
            .await
            .body
            .contains("<option value=\"EUR\" selected>")
    );
    assert!(text_of(section(&admin.get(PAGE).await.body, "answer")).contains("EUR"));
}

#[tokio::test]
async fn member_is_refused_whatever_is_sent() {
    let world = world().await;
    let mut member = bob(&world).await;
    let before = world.settings_events().await.len();

    for fields in [
        &[("display_currency", "USD")][..],
        &[("display_currency", "<b>nope</b>")][..],
        &[("display_currency", "USD"), ("extra", "1")][..],
        &[][..],
    ] {
        let reply = set_currency(&mut member, fields).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{fields:?}");
        assert!(!reply.body.contains("nope"), "does not echo");
    }

    assert_eq!(world.settings_events().await.len(), before);
    let report = world.report(THREE_MONTHS).await;
    assert_eq!(report["display_currency"], Value::Null, "nothing changed");
    assert_eq!(
        member.get(HOUSEHOLD).await.status,
        StatusCode::FORBIDDEN,
        "no form"
    );
}

#[tokio::test]
async fn forged_form_fails_whole() {
    let world = world().await;
    let mut admin = alice(&world).await;
    let before = world.settings_events().await.len();

    for fields in [
        &[("display_currency", "ZZZ")][..],
        &[("display_currency", "<b>nope</b>")][..],
        &[("display_currency", "")][..],
        &[("display_currency", "usd")][..],
        &[][..],
        &[("display_currency", "USD"), ("extra", "1")][..],
        &[("display_currency", "USD"), ("floor", "0")][..],
    ] {
        let reply = set_currency(&mut admin, fields).await;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY, "{fields:?}");
        assert!(reply.header(header::CONTENT_TYPE).starts_with("text/html"));
        assert!(!reply.body.contains("nope"), "does not echo");
    }

    assert_eq!(world.settings_events().await.len(), before, "not audited");
    let report = world.report(THREE_MONTHS).await;
    assert_eq!(report["display_currency"], Value::Null, "nothing changed");
}

#[tokio::test]
async fn csrf_is_required() {
    let world = world().await;
    let mut admin = alice(&world).await;
    let before = world.settings_events().await.len();

    for fields in [
        &[("display_currency", "USD")][..],
        &[("csrf", "wrong"), ("display_currency", "USD")][..],
    ] {
        let reply = admin.post_form(SET_CURRENCY, fields).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{fields:?}");
    }

    assert_eq!(world.settings_events().await.len(), before, "not audited");
    let report = world.report(THREE_MONTHS).await;
    assert_eq!(report["display_currency"], Value::Null, "nothing changed");
}
