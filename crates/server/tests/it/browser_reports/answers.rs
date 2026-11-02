//! Whatever is sent, the answer is the same empty 204; and what the origin
//! rule means for this route.

use axum::http::{Method, StatusCode, header};
use coffer_server::testkit::TEST_PUBLIC_URL;
use coffer_server::testkit::browser::Reply;
use serde_json::json;

use super::{
    CSP, JSON, PATH, browser, csp_events, csp_on_fresh_line, csp_report, events_on, lines_with,
    post, post_script, script_report,
};

fn assert_same_answer(reply: &Reply, reference: &Reply, what: &str) {
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{what}");
    assert!(reply.bytes.is_empty(), "{what}");
    assert_eq!(reply.headers, reference.headers, "{what}");
}

#[tokio::test]
async fn every_input_gets_the_same_empty_204() {
    let (_server, mut browser) = browser().await;
    let logged = post_script(&mut browser, &script_report("answer-logged")).await;
    assert_eq!(logged.status, StatusCode::NO_CONTENT);
    let origin = (header::ORIGIN, TEST_PUBLIC_URL);
    let json_type = (header::CONTENT_TYPE, JSON);

    let cases = [
        (
            "not JSON",
            post(&mut browser, &[json_type.clone(), origin.clone()], "{").await,
        ),
        (
            "wrong type",
            post(
                &mut browser,
                &[(header::CONTENT_TYPE, "text/plain"), origin.clone()],
                "x",
            )
            .await,
        ),
        (
            "no type",
            post(&mut browser, std::slice::from_ref(&origin), "x").await,
        ),
        (
            "empty",
            post(&mut browser, &[json_type.clone(), origin.clone()], "").await,
        ),
        (
            "oversized",
            post(
                &mut browser,
                &[json_type.clone(), origin.clone()],
                "x".repeat(100_000),
            )
            .await,
        ),
        (
            "foreign origin",
            post(
                &mut browser,
                &[json_type.clone(), (header::ORIGIN, "https://evil.test")],
                "{}",
            )
            .await,
        ),
        (
            "no origin",
            post(&mut browser, std::slice::from_ref(&json_type), "{}").await,
        ),
    ];
    for (what, reply) in &cases {
        assert_same_answer(reply, &logged, what);
    }
}

#[tokio::test]
async fn a_report_dropped_over_the_limit_gets_the_same_answer() {
    let (_server, mut browser) = browser().await;
    let first = post_script(&mut browser, &script_report("answer-first")).await;
    let mut last = None;
    for _ in 0..30 {
        last = Some(post_script(&mut browser, &script_report("answer-more")).await);
    }
    assert_same_answer(&last.unwrap(), &first, "over the limit");
}

/// The router's origin rule would refuse a CSP report with no `Origin` with a
/// 403 before the handler ran.
#[tokio::test]
async fn csp_report_with_no_origin_is_logged() {
    let (_server, mut browser) = browser().await;
    let log = csp_events(&mut browser, &csp_report("/login", "img-src", "inline")).await;
    assert!(log.contains("img-src blocked inline"), "{log}");
}

#[tokio::test]
async fn csp_report_from_another_origin_is_dropped() {
    let (_server, mut browser) = browser().await;
    let (body, line) = csp_on_fresh_line(&csp_report("/login", "img-src", "inline"));
    let headers = [
        (header::CONTENT_TYPE, CSP),
        (header::ORIGIN, "https://evil.test"),
    ];
    let reply = post(&mut browser, &headers, body.to_string()).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    assert_eq!(events_on(line), "");
}

/// Another site's own policy can name this endpoint in its `report-uri`.
#[tokio::test]
async fn csp_report_about_a_document_of_another_origin_is_dropped() {
    let (_server, mut browser) = browser().await;
    let mut body = csp_report("/login", "img-src", "inline");
    body["csp-report"]["document-uri"] = json!("https://evil.test/login");
    assert_eq!(csp_events(&mut browser, &body).await, "");
}

#[tokio::test]
async fn script_report_from_another_origin_is_dropped() {
    let (_server, mut browser) = browser().await;
    let headers = [
        (header::CONTENT_TYPE, JSON),
        (header::ORIGIN, "https://evil.test"),
    ];
    let body = script_report("answer-foreign").to_string();
    let reply = post(&mut browser, &headers, body).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    assert_eq!(lines_with("answer-foreign").len(), 0);
}

/// A script always has an origin; only a script report's origin can prove it
/// came from a page of coffer.
#[tokio::test]
async fn script_report_with_no_origin_is_dropped() {
    let (_server, mut browser) = browser().await;
    let headers = [(header::CONTENT_TYPE, JSON)];
    let body = script_report("answer-no-origin").to_string();
    post(&mut browser, &headers, body).await;
    assert_eq!(lines_with("answer-no-origin").len(), 0);
}

/// Both accepted content types need a preflight from another site. Refusing
/// it, with no CORS header, is what stops a cross-site script from posting.
#[tokio::test]
async fn a_cross_site_preflight_is_not_granted() {
    let (_server, mut browser) = browser().await;
    let request = browser
        .builder(Method::OPTIONS, PATH)
        .header(header::ORIGIN, "https://evil.test")
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
        .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "content-type")
        .body(axum::body::Body::empty())
        .unwrap();
    let reply = browser.send(request).await;
    assert_eq!(reply.status, StatusCode::METHOD_NOT_ALLOWED);
    assert!(
        !reply
            .headers
            .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN)
    );
}

#[tokio::test]
async fn the_answer_carries_the_security_headers() {
    let (_server, mut browser) = browser().await;
    let reply = post_script(&mut browser, &script_report("answer-headers")).await;
    assert_eq!(
        reply.header(header::CONTENT_SECURITY_POLICY),
        "default-src 'self'; script-src 'self'; style-src 'self'; frame-ancestors 'none'; \
         form-action 'self'; base-uri 'none'; report-uri /browser-reports"
    );
    assert_eq!(reply.header(header::REFERRER_POLICY), "same-origin");
    assert_eq!(reply.header(header::X_CONTENT_TYPE_OPTIONS), "nosniff");
}
