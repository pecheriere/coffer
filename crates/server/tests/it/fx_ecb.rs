//! The ECB rate provider against a loopback mock, with the shapes its data API
//! showed on 2026-11-02. No test reaches the real service.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use coffer_domain::fx::RateObservation;
use coffer_domain::money::Currency;
use coffer_server::egress::{Egress, HostAllowList};
use coffer_server::fx::ecb::Ecb;
use coffer_server::fx::{RateError, RateProvider as _};
use coffer_server::testkit::{
    MockUpstream, RecordedRequest, capture_logs, captured_log_text, log_marker,
};
use jiff::civil::{Date, date};
use proptest::prelude::*;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use url::Url;

const HEADER: &str = "KEY,FREQ,CURRENCY,CURRENCY_DENOM,EXR_TYPE,EXR_SUFFIX,TIME_PERIOD,OBS_VALUE";
const SERIES_ROUTE: &str = "/service/data/EXR/{series}";
const USD_PATH: &str = "/service/data/EXR/D.USD.EUR.SP00.A";
const BASE_QUERY: &str = "format=csvdata&detail=dataonly";
const FIXTURE: &[u8] = include_bytes!("fixtures/ecb-usd.csv");
/// The body limit, written out: a test using the constant would follow it.
const TWO_MIB: usize = 2 * 1024 * 1024;
const MOST_ROWS: usize = 20_000;
/// The ECB's real answer for a series it doesn't have.
const NOT_FOUND_BODY: &str = r#"{"type":"/service/data/EXR/D.ROL.EUR.SP00.A","title":"Not Found","status":404,"detail":"No Series was returned for the query: <?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><DataQuery dataflowId=\"EXR\" detail=\"DATAONLY\" type=\"SINGLE_QUERY\" includeHistory=\"false\"><SeriesKeys><SeriesKey>D.ROL.EUR.SP00.A</SeriesKey></SeriesKeys></DataQuery>","instance":"/service/data/EXR/D.ROL.EUR.SP00.A"}"#;

const TODAY: Date = date(2026, 11, 2);

fn usd() -> Currency {
    Currency::parse("USD").unwrap()
}

fn row(on: &str, value: &str) -> String {
    format!("EXR.D.USD.EUR.SP00.A,D,USD,EUR,SP00,A,{on},{value}")
}

fn answer(rows: &[String]) -> String {
    let mut text = format!("{HEADER}\n");
    for line in rows {
        text.push_str(line);
        text.push('\n');
    }
    text
}

fn ecb_at(mock: &MockUpstream) -> Ecb {
    let port = mock.base.port().unwrap();
    Ecb::with_base(
        Egress::for_tests(),
        HostAllowList::loopback(port),
        mock.base.clone(),
    )
}

async fn serve(status: StatusCode, body: impl Into<Vec<u8>>) -> (MockUpstream, Ecb) {
    let body = body.into();
    let router = Router::new().route(
        SERIES_ROUTE,
        get(move || {
            let body = body.clone();
            async move { (status, body) }
        }),
    );
    let mock = MockUpstream::start(router).await;
    let ecb = ecb_at(&mock);
    (mock, ecb)
}

/// What the provider makes of `body` on a 200, for USD, with no `since`.
async fn fetched(body: impl Into<Vec<u8>>) -> Result<Vec<RateObservation>, RateError> {
    let (_mock, ecb) = serve(StatusCode::OK, body).await;
    ecb.fetch(usd(), None, TODAY).await
}

async fn fetched_with_status(
    status: StatusCode,
    body: impl Into<Vec<u8>>,
) -> Result<Vec<RateObservation>, RateError> {
    let (_mock, ecb) = serve(status, body).await;
    ecb.fetch(usd(), None, TODAY).await
}

async fn assert_refused(body: impl Into<Vec<u8>>) {
    assert_eq!(fetched(body).await, Err(RateError::Malformed));
}

async fn assert_row_refused(line: &str) {
    assert_refused(answer(&[line.to_owned()])).await;
}

fn days_and_rates(rates: &[RateObservation]) -> Vec<(String, String)> {
    rates
        .iter()
        .map(|found| (found.on.to_string(), found.rate.to_string()))
        .collect()
}

fn pairs(expected: &[(&str, &str)]) -> Vec<(String, String)> {
    expected
        .iter()
        .map(|&(on, rate)| (on.to_owned(), rate.to_owned()))
        .collect()
}

/// `count` rows on consecutive days from 1990-01-01, with `value` as the rate.
fn rows_from_1990(count: usize, value: &str) -> Vec<String> {
    let mut on = date(1990, 1, 1);
    let mut rows = Vec::new();
    for _ in 0..count {
        rows.push(row(&on.to_string(), value));
        on = on.tomorrow().unwrap();
    }
    rows
}

#[tokio::test]
async fn asks_for_the_series_and_nothing_else() {
    let seen: Arc<Mutex<Vec<HeaderMap>>> = Arc::default();
    let recorder = Arc::clone(&seen);
    let router = Router::new().route(
        SERIES_ROUTE,
        get(move |headers: HeaderMap| {
            recorder.lock().unwrap().push(headers);
            std::future::ready(StatusCode::OK.into_response())
        }),
    );
    let mock = MockUpstream::start(router).await;
    let ecb = ecb_at(&mock);

    ecb.fetch(usd(), None, TODAY).await.unwrap();
    ecb.fetch(usd(), Some(date(2026, 10, 23)), TODAY)
        .await
        .unwrap();

    let request = |query: String| RecordedRequest {
        method: Method::GET,
        path: USD_PATH.to_owned(),
        query: Some(query),
        authorization: false,
    };
    assert_eq!(
        mock.requests(),
        vec![
            request(BASE_QUERY.to_owned()),
            request(format!("{BASE_QUERY}&startPeriod=2026-10-23")),
        ]
    );
    for headers in seen.lock().unwrap().iter() {
        let agent = headers.get(header::USER_AGENT).unwrap().to_str().unwrap();
        assert!(agent.starts_with("coffer/"), "the user agent must be ours");
        let names: BTreeSet<&str> = headers.keys().map(header::HeaderName::as_str).collect();
        assert_eq!(names, BTreeSet::from(["accept", "host", "user-agent"]));
    }
}

/// The fixture stays as the ECB sent it, whatever a checkout does to it.
#[test]
fn the_fixture_ends_its_lines_as_the_ecb_does() {
    let text = std::str::from_utf8(FIXTURE).unwrap();
    assert!(text.ends_with("\r\n"), "the last line must end in CRLF");
    assert_eq!(text.matches('\n').count(), text.matches("\r\n").count());
    assert_eq!(text.matches('\r').count(), text.matches("\r\n").count());
}

#[tokio::test]
async fn reads_the_published_format() {
    let rates = fetched(FIXTURE).await.unwrap();
    assert_eq!(
        days_and_rates(&rates),
        pairs(&[
            ("2012-04-30", "1.3214"),
            ("2012-05-02", "1.3131"),
            ("2026-10-26", "1.1188"),
            ("2026-10-27", "1.115"),
            ("2026-10-28", "1.1133"),
            ("2026-10-29", "1.113"),
            ("2026-10-30", "1.1172"),
        ])
    );

    let isk = format!("{HEADER}\r\nEXR.D.ISK.EUR.SP00.A,D,ISK,EUR,SP00,A,2026-10-30,136\r\n");
    let (_mock, ecb) = serve(StatusCode::OK, isk).await;
    let rates = ecb
        .fetch(Currency::parse("ISK").unwrap(), None, TODAY)
        .await
        .unwrap();
    assert_eq!(days_and_rates(&rates), pairs(&[("2026-10-30", "136")]));
}

/// The ECB's daily series hold rows with no value, 62 of them in USD's from
/// 1999-12-31 to 2012-05-01: a day with no rate, which is not a bad answer.
#[tokio::test]
async fn a_day_with_no_value_has_no_rate() {
    let rates = fetched(answer(&[
        row("2012-04-30", "1.3214"),
        row("2012-05-01", ""),
        row("2012-05-02", "1.3131"),
    ]))
    .await
    .unwrap();
    assert_eq!(
        days_and_rates(&rates),
        pairs(&[("2012-04-30", "1.3214"), ("2012-05-02", "1.3131")])
    );
}

#[tokio::test]
async fn a_day_with_no_value_is_still_checked() {
    assert_row_refused(&row("1989-12-31", "")).await;
    assert_row_refused(&row("2026-11-04", "")).await;
    assert_row_refused("EXR.D.GBP.EUR.SP00.A,D,GBP,EUR,SP00,A,2012-05-01,").await;
    assert_refused(answer(&[row("2012-05-01", ""), row("2012-05-01", "1.3")])).await;
}

#[tokio::test]
async fn only_an_empty_field_means_no_value() {
    for blank in [" ", "NaN", "N/A", "-", "null"] {
        assert_row_refused(&row("2012-05-01", blank)).await;
    }
}

#[tokio::test]
async fn an_empty_answer_is_no_new_rates() {
    assert_eq!(fetched("").await, Ok(Vec::new()));
}

#[tokio::test]
async fn a_header_alone_is_no_new_rates() {
    assert_eq!(fetched(answer(&[])).await, Ok(Vec::new()));
}

#[tokio::test]
async fn no_series_is_not_published() {
    assert_eq!(
        fetched_with_status(StatusCode::NOT_FOUND, NOT_FOUND_BODY).await,
        Err(RateError::NotPublished)
    );
}

#[tokio::test]
async fn an_html_error_page_is_unavailable() {
    let page = "<html><head><title>Bad Request</title></head><body>quoted-marker</body></html>";
    let err = fetched_with_status(StatusCode::BAD_REQUEST, page)
        .await
        .unwrap_err();
    assert_eq!(err, RateError::Unavailable { status: 400 });
    for shown in [format!("{err:?}"), err.to_string()] {
        for text in ["html", "Bad Request", "title", "quoted-marker"] {
            assert!(!shown.contains(text), "the error holds the page's text");
        }
    }
}

#[tokio::test]
async fn any_other_status_is_unavailable() {
    for status in [
        StatusCode::NO_CONTENT,
        StatusCode::NOT_MODIFIED,
        StatusCode::TOO_MANY_REQUESTS,
        StatusCode::INTERNAL_SERVER_ERROR,
        StatusCode::SERVICE_UNAVAILABLE,
    ] {
        assert_eq!(
            fetched_with_status(status, "").await,
            Err(RateError::Unavailable {
                status: status.as_u16()
            })
        );
    }
}

#[tokio::test]
async fn a_valid_answer_under_an_error_status_is_not_read() {
    assert_eq!(
        fetched_with_status(StatusCode::ACCEPTED, FIXTURE).await,
        Err(RateError::Unavailable { status: 202 })
    );
}

#[tokio::test]
async fn a_200_html_page_is_malformed() {
    assert_refused("<html><body>Service maintenance</body></html>").await;
}

#[tokio::test]
async fn a_redirect_is_not_followed() {
    let (elsewhere, _) = serve(StatusCode::OK, FIXTURE).await;
    let target = elsewhere.base.join(USD_PATH).unwrap();
    let router = Router::new().route(
        SERIES_ROUTE,
        get(move || {
            let location = target.to_string();
            async move { (StatusCode::FOUND, [(header::LOCATION, location)]) }
        }),
    );
    let mock = MockUpstream::start(router).await;

    let result = ecb_at(&mock).fetch(usd(), None, TODAY).await;

    assert_eq!(result, Err(RateError::Unavailable { status: 302 }));
    assert_eq!(mock.requests().len(), 1);
    assert!(
        elsewhere.requests().is_empty(),
        "the redirect's target must see no request"
    );
}

#[tokio::test]
async fn wrong_header() {
    let good = row("2026-10-30", "1.1172");
    for header_line in [
        "KEY,FREQ,CURRENCY,CURRENCY_DENOM,EXR_TYPE,EXR_SUFFIX,TIME_PERIOD",
        "KEY,FREQ,CURRENCY,CURRENCY_DENOM,EXR_TYPE,EXR_SUFFIX,TIME_PERIOD,OBS_VALUE,OBS_STATUS",
        "key,freq,currency,currency_denom,exr_type,exr_suffix,time_period,obs_value",
        "FREQ,KEY,CURRENCY,CURRENCY_DENOM,EXR_TYPE,EXR_SUFFIX,TIME_PERIOD,OBS_VALUE",
        "\u{feff}KEY,FREQ,CURRENCY,CURRENCY_DENOM,EXR_TYPE,EXR_SUFFIX,TIME_PERIOD,OBS_VALUE",
        "KEY,FREQ,CURRENCY,CURRENCY_DENOM,EXR_TYPE,EXR_SUFFIX,TIME_PERIOD,OBS_VALUE ",
        "",
    ] {
        assert_refused(format!("{header_line}\n{good}\n")).await;
    }
}

#[tokio::test]
async fn wrong_currency_in_a_row() {
    assert_row_refused("EXR.D.USD.EUR.SP00.A,D,GBP,EUR,SP00,A,2026-10-30,1.1172").await;
}

#[tokio::test]
async fn wrong_key() {
    assert_row_refused("EXR.D.GBP.EUR.SP00.A,D,USD,EUR,SP00,A,2026-10-30,1.1172").await;
    assert_row_refused("EXR.M.USD.EUR.SP00.A,D,USD,EUR,SP00,A,2026-10-30,1.1172").await;
    assert_row_refused("EXR.D.USD.EUR.SP00.E,D,USD,EUR,SP00,A,2026-10-30,1.1172").await;
}

#[tokio::test]
async fn wrong_denominator() {
    assert_row_refused("EXR.D.USD.EUR.SP00.A,D,USD,GBP,SP00,A,2026-10-30,1.1172").await;
}

#[tokio::test]
async fn wrong_frequency() {
    assert_row_refused("EXR.D.USD.EUR.SP00.A,M,USD,EUR,SP00,A,2026-10-30,1.1172").await;
}

#[tokio::test]
async fn wrong_rate_type() {
    assert_row_refused("EXR.D.USD.EUR.SP00.A,D,USD,EUR,EN00,A,2026-10-30,1.1172").await;
}

#[tokio::test]
async fn wrong_suffix() {
    assert_row_refused("EXR.D.USD.EUR.SP00.A,D,USD,EUR,SP00,E,2026-10-30,1.1172").await;
}

#[tokio::test]
async fn too_few_or_many_fields() {
    assert_row_refused("EXR.D.USD.EUR.SP00.A,D,USD,EUR,SP00,A,2026-10-30").await;
    assert_row_refused(&row("2026-10-30", "1.1172,A")).await;
    assert_row_refused(&row("2026-10-30", "1.1172,")).await;
    assert_row_refused("").await;
}

#[tokio::test]
async fn date_after_tomorrow() {
    let tomorrow = fetched(answer(&[row("2026-11-03", "1.1")])).await.unwrap();
    assert_eq!(days_and_rates(&tomorrow), pairs(&[("2026-11-03", "1.1")]));
    assert_row_refused(&row("2026-11-04", "1.1")).await;
}

#[tokio::test]
async fn date_before_1990() {
    let first = fetched(answer(&[row("1990-01-01", "1.1")])).await.unwrap();
    assert_eq!(days_and_rates(&first), pairs(&[("1990-01-01", "1.1")]));
    assert_row_refused(&row("1989-12-31", "1.1")).await;
}

#[tokio::test]
async fn a_date_before_the_window_asked_for_is_refused() {
    let since = date(2026, 10, 27);
    let (_mock, ecb) = serve(StatusCode::OK, answer(&[row("2026-10-27", "1.1")])).await;
    let rates = ecb.fetch(usd(), Some(since), TODAY).await.unwrap();
    assert_eq!(days_and_rates(&rates), pairs(&[("2026-10-27", "1.1")]));

    let (_mock, ecb) = serve(StatusCode::OK, answer(&[row("2026-10-26", "1.1")])).await;
    assert_eq!(
        ecb.fetch(usd(), Some(since), TODAY).await,
        Err(RateError::Malformed)
    );
}

/// A window reaching back past the floor doesn't lower it.
#[tokio::test]
async fn a_window_before_1990_keeps_the_floor() {
    let since = date(1985, 6, 1);
    let (_mock, ecb) = serve(StatusCode::OK, answer(&[row("1990-01-01", "1.1")])).await;
    let rates = ecb.fetch(usd(), Some(since), TODAY).await.unwrap();
    assert_eq!(days_and_rates(&rates), pairs(&[("1990-01-01", "1.1")]));

    let (_mock, ecb) = serve(StatusCode::OK, answer(&[row("1989-12-31", "1.1")])).await;
    assert_eq!(
        ecb.fetch(usd(), Some(since), TODAY).await,
        Err(RateError::Malformed)
    );
}

#[tokio::test]
async fn a_date_in_any_other_form_is_refused() {
    for on in [
        "20261030",
        "2026-10-30T00:00:00",
        "2026-10-30 ",
        " 2026-10-30",
        "2026-1-30",
        "26-10-30",
        "+2026-10-30",
        "2026/10/30",
        "2026-02-30",
        "2026-13-01",
        "2026-W44",
        "",
    ] {
        assert_row_refused(&row(on, "1.1")).await;
    }
}

#[tokio::test]
async fn dates_out_of_order_or_repeated() {
    assert_refused(answer(&[
        row("2026-10-30", "1.1"),
        row("2026-10-29", "1.1"),
    ]))
    .await;
    assert_refused(answer(&[
        row("2026-10-30", "1.1"),
        row("2026-10-30", "1.1"),
    ]))
    .await;
}

#[tokio::test]
async fn rate_with_an_exponent() {
    assert_row_refused(&row("2026-10-30", "1e3")).await;
    assert_row_refused(&row("2026-10-30", "1.1E0")).await;
}

#[tokio::test]
async fn zero_rate() {
    for zero in ["0", "0.0", "0.000000"] {
        assert_row_refused(&row("2026-10-30", zero)).await;
    }
}

#[tokio::test]
async fn a_rate_outside_the_exact_form_is_refused() {
    for value in [
        "12345678901",
        "1.1234567",
        "12345.123456",
        "-1.1",
        "+1.1",
        " 1.1",
        "1.1 ",
        "1.",
        ".5",
        "Infinity",
        "0x10",
    ] {
        assert_row_refused(&row("2026-10-30", value)).await;
    }
}

#[tokio::test]
async fn not_utf8() {
    let mut body = answer(&[row("2026-10-30", "1.1172")]).into_bytes();
    body.extend_from_slice(&[0xff, 0xfe]);
    assert_refused(body).await;
}

#[tokio::test]
async fn one_bad_row_refuses_the_whole_answer() {
    assert_refused(answer(&[
        row("2026-10-28", "1.1133"),
        row("2026-10-29", "oops"),
        row("2026-10-30", "1.1172"),
    ]))
    .await;
}

#[tokio::test]
async fn both_line_endings_are_read() {
    let good = row("2026-10-30", "1.1172");
    let expected = pairs(&[("2026-10-30", "1.1172")]);
    for body in [
        format!("{HEADER}\n{good}\n"),
        format!("{HEADER}\r\n{good}\r\n"),
    ] {
        assert_eq!(days_and_rates(&fetched(body).await.unwrap()), expected);
    }
}

/// A real `1.1172` cut by a clean close must not read as a rate of 1.1.
#[tokio::test]
async fn a_row_cut_off_by_the_end_of_the_answer_is_refused() {
    assert_refused(format!("{HEADER}\n{}", row("2026-10-30", "1.1"))).await;
    assert_refused(format!("{HEADER}\r\n{}\r", row("2026-10-30", "1.1172"))).await;
}

#[tokio::test]
async fn a_header_cut_off_by_the_end_of_the_answer_is_refused() {
    assert_refused(HEADER).await;
}

#[tokio::test]
async fn nothing_else_is_trimmed() {
    let good = row("2026-10-30", "1.1172");
    for body in [
        format!("{HEADER}\n{good}\n\n"),
        format!("\n{HEADER}\n{good}\n"),
        format!("{HEADER}\n\n{good}\n"),
        format!("{HEADER}\n {good}\n"),
        format!("{HEADER}\n{good} \n"),
        format!("{HEADER}\n{good}\t\n"),
        format!("{HEADER}\r{good}\r"),
        format!("{HEADER}\n{good}\r"),
        format!("{HEADER}\n{good}\r\r\n"),
    ] {
        assert_refused(body).await;
    }
}

/// A plain body over the limit.
#[tokio::test]
async fn a_body_over_the_limit_is_malformed() {
    assert_refused(vec![b'x'; TWO_MIB + 1]).await;
}

/// A loopback server answering 200 with `size` bytes and then holding the
/// connection open: a client that stops at the limit answers at once, and one
/// that doesn't waits for more.
async fn endless_answer(size: usize) -> Ecb {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut head = [0; 1024];
        let _ = stream.read(&mut head).await.unwrap();
        stream.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await.unwrap();
        // The client hanging up once it has enough is the point of the test.
        let _ = stream.write_all(&vec![b'x'; size]).await;
        std::future::pending::<()>().await;
    });
    let mut base = Url::parse("https://127.0.0.1/").unwrap();
    base.set_scheme("http").unwrap();
    base.set_port(Some(port)).unwrap();
    Ecb::with_base(Egress::for_tests(), HostAllowList::loopback(port), base)
}

#[tokio::test]
async fn reading_stops_as_soon_as_the_limit_is_passed() {
    let ecb = endless_answer(TWO_MIB + 1).await;
    let result = tokio::time::timeout(Duration::from_secs(10), ecb.fetch(usd(), None, TODAY)).await;
    assert_eq!(result.unwrap(), Err(RateError::Malformed));
}

#[tokio::test]
async fn a_body_at_the_limit_is_still_being_read() {
    let ecb = endless_answer(TWO_MIB).await;
    let result =
        tokio::time::timeout(Duration::from_millis(500), ecb.fetch(usd(), None, TODAY)).await;
    assert!(result.is_err(), "the limit itself must not stop the read");
}

/// An oversized error page answers by its status, as a small one does.
#[tokio::test]
async fn an_error_page_over_the_limit_is_unavailable() {
    assert_eq!(
        fetched_with_status(StatusCode::BAD_GATEWAY, vec![b'x'; TWO_MIB + 1]).await,
        Err(RateError::Unavailable { status: 502 })
    );
}

#[tokio::test]
async fn an_oversized_answer_for_no_series_is_not_published() {
    assert_eq!(
        fetched_with_status(StatusCode::NOT_FOUND, vec![b'x'; TWO_MIB + 1]).await,
        Err(RateError::NotPublished)
    );
}

#[tokio::test]
async fn the_euro_is_never_asked_for() {
    let (mock, ecb) = serve(StatusCode::OK, FIXTURE).await;
    let rates = ecb
        .fetch(Currency::parse("EUR").unwrap(), None, TODAY)
        .await;
    assert_eq!(rates, Ok(Vec::new()));
    assert!(mock.requests().is_empty(), "the euro must ask for nothing");
}

/// Far enough ahead that this many distinct days pass the date rules, which
/// alone hold an answer to about 13,500 rows in 2026.
const FAR_AHEAD: Date = date(2050, 1, 1);

async fn fetched_far_ahead(body: String) -> Result<Vec<RateObservation>, RateError> {
    let (_mock, ecb) = serve(StatusCode::OK, body).await;
    ecb.fetch(usd(), None, FAR_AHEAD).await
}

#[tokio::test]
async fn too_many_rows_are_refused() {
    let at_most = fetched_far_ahead(answer(&rows_from_1990(MOST_ROWS, "1.1")))
        .await
        .unwrap();
    assert_eq!(at_most.len(), MOST_ROWS);
    assert_eq!(
        fetched_far_ahead(answer(&rows_from_1990(MOST_ROWS + 1, "1.1"))).await,
        Err(RateError::Malformed)
    );
}

#[tokio::test]
async fn rows_with_no_value_count_towards_the_limit() {
    assert_eq!(
        fetched_far_ahead(answer(&rows_from_1990(MOST_ROWS + 1, ""))).await,
        Err(RateError::Malformed)
    );
}

#[tokio::test]
async fn a_host_outside_the_allow_list_is_never_contacted() {
    let (mock, _) = serve(StatusCode::OK, FIXTURE).await;
    let ecb = Ecb::with_base(
        Egress::for_tests(),
        HostAllowList::none(),
        mock.base.clone(),
    );
    assert_eq!(ecb.fetch(usd(), None, TODAY).await, Err(RateError::Network));
    assert!(mock.requests().is_empty(), "the mock must see no request");
}

#[tokio::test]
async fn a_source_that_cannot_be_reached_is_a_network_failure() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let mut base = Url::parse("https://127.0.0.1/").unwrap();
    base.set_scheme("http").unwrap();
    base.set_port(Some(port)).unwrap();
    let ecb = Ecb::with_base(Egress::for_tests(), HostAllowList::loopback(port), base);
    assert_eq!(ecb.fetch(usd(), None, TODAY).await, Err(RateError::Network));
}

#[tokio::test]
async fn no_part_of_an_answer_reaches_a_log() {
    capture_logs();
    let kept = log_marker();
    let text = "ecb-answer-text-3f9a7c";
    let bad_row = row("2026-10-30", text);
    assert_eq!(fetched(answer(&[bad_row])).await, Err(RateError::Malformed));
    assert_eq!(
        fetched_with_status(StatusCode::BAD_GATEWAY, text).await,
        Err(RateError::Unavailable { status: 502 })
    );
    let logged = captured_log_text();
    assert!(logged.contains(&kept), "the capture lost what it must hold");
    assert!(!logged.contains(text), "the answer's text reached a log");
}

/// Arbitrary bytes, and real answers with bytes changed.
fn answers() -> impl Strategy<Value = Vec<u8>> {
    let changed =
        proptest::collection::vec((0..FIXTURE.len(), any::<u8>()), 1..6).prop_map(|changes| {
            let mut body = FIXTURE.to_vec();
            for (at, byte) in changes {
                body[at] = byte;
            }
            body
        });
    prop_oneof![proptest::collection::vec(any::<u8>(), 0..1024), changed]
}

#[test]
fn no_input_panics() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let current: Arc<Mutex<Vec<u8>>> = Arc::default();
    let served = Arc::clone(&current);
    let router = Router::new().route(
        SERIES_ROUTE,
        get(move || {
            let body = served.lock().unwrap().clone();
            std::future::ready((StatusCode::OK, body))
        }),
    );
    let (_mock, ecb) = runtime.block_on(async {
        let mock = MockUpstream::start(router).await;
        let ecb = ecb_at(&mock);
        (mock, ecb)
    });

    proptest!(ProptestConfig::with_cases(128), |(body in answers())| {
        current.lock().unwrap().clone_from(&body);
        let result = runtime.block_on(ecb.fetch(usd(), None, TODAY));
        match result {
            Ok(rates) => {
                let in_order = rates.windows(2).all(|pair| pair[0].on < pair[1].on);
                prop_assert!(in_order, "accepted rates must be in order");
                let in_bounds = rates
                    .iter()
                    .all(|found| found.on >= date(1990, 1, 1) && found.on <= date(2026, 11, 3));
                prop_assert!(in_bounds, "accepted rates must be within the date bounds");
            }
            Err(err) => prop_assert_eq!(err, RateError::Malformed),
        }
    });
}
