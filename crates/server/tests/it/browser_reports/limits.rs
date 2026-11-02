//! What is refused for its shape: size, content type, unknown fields.

use axum::http::{StatusCode, header};
use coffer_server::testkit::{TEST_PUBLIC_URL, log_marker};
use serde_json::json;

use super::{
    CSP, JSON, browser, csp_report, lines_with, log_since, post, post_csp, post_script,
    script_report,
};

/// The endpoint's limit on a body.
const LIMIT: usize = 8 * 1024;

/// `report` padded with whitespace, which JSON ignores, to exactly `size` bytes.
fn padded(report: &serde_json::Value, size: usize) -> String {
    let text = report.to_string();
    format!("{text}{}", " ".repeat(size - text.len()))
}

const FROM_PAGE: [(header::HeaderName, &str); 2] = [
    (header::CONTENT_TYPE, JSON),
    (header::ORIGIN, TEST_PUBLIC_URL),
];

#[tokio::test]
async fn a_body_at_the_limit_is_logged() {
    let (_server, mut browser) = browser().await;
    let body = padded(&script_report("size-at-limit"), LIMIT);
    post(&mut browser, &FROM_PAGE, body).await;
    assert_eq!(lines_with("size-at-limit").len(), 1);
}

#[tokio::test]
async fn a_body_past_the_limit_is_not_logged() {
    let (_server, mut browser) = browser().await;
    let start = log_marker();
    let body = padded(&script_report("size-past-limit"), LIMIT + 1);
    post(&mut browser, &FROM_PAGE, body).await;
    assert!(!log_since(&start).contains("size-past-limit"));
}

/// The declared length alone refuses it, before any of the body is read.
#[tokio::test]
async fn a_declared_length_past_the_limit_is_refused_unread() {
    let (_server, mut browser) = browser().await;
    let start = log_marker();
    let length = (LIMIT + 1).to_string();
    let headers = [
        (header::CONTENT_TYPE, JSON),
        (header::ORIGIN, TEST_PUBLIC_URL),
        (header::CONTENT_LENGTH, length.as_str()),
    ];
    post(
        &mut browser,
        &headers,
        script_report("size-declared").to_string(),
    )
    .await;
    assert!(!log_since(&start).contains("size-declared"));
}

#[tokio::test]
async fn only_the_exact_content_types_are_read() {
    let (_server, mut browser) = browser().await;
    let start = log_marker();
    for content_type in [
        "application/json; charset=utf-8",
        "APPLICATION/JSON",
        "application/json ",
        "text/plain",
        "application/x-www-form-urlencoded",
        "application/reports+json",
    ] {
        let headers = [
            (header::CONTENT_TYPE, content_type),
            (header::ORIGIN, TEST_PUBLIC_URL),
        ];
        post(
            &mut browser,
            &headers,
            script_report("type-refused").to_string(),
        )
        .await;
    }
    assert!(!log_since(&start).contains("type-refused"));
}

#[tokio::test]
async fn a_csp_report_under_any_other_type_is_not_read() {
    let (_server, mut browser) = browser().await;
    let start = log_marker();
    for content_type in [
        "application/csp-report; charset=utf-8",
        "text/plain",
        "application/json",
        "application/reports+json",
    ] {
        let headers = [(header::CONTENT_TYPE, content_type)];
        let body = csp_report("/login", "img-src", "inline").to_string();
        post(&mut browser, &headers, body).await;
    }
    assert!(!log_since(&start).contains("img-src blocked"));
}

#[tokio::test]
async fn a_script_report_posted_as_a_csp_report_is_not_read() {
    let (_server, mut browser) = browser().await;
    let start = log_marker();
    let headers = [
        (header::CONTENT_TYPE, CSP),
        (header::ORIGIN, TEST_PUBLIC_URL),
    ];
    post(
        &mut browser,
        &headers,
        script_report("type-crossed").to_string(),
    )
    .await;
    assert!(!log_since(&start).contains("type-crossed"));
}

/// First-party code sends exactly these fields, so anything else is forged.
#[tokio::test]
async fn a_script_report_with_an_unknown_field_is_dropped_whole() {
    let (_server, mut browser) = browser().await;
    let start = log_marker();
    let mut body = script_report("unknown-field");
    body["stack"] = json!("at somewhere");
    post_script(&mut browser, &body).await;
    assert!(!log_since(&start).contains("unknown-field"));
}

/// Browsers add fields of their own, so these are ignored, and none reaches
/// the log.
#[tokio::test]
async fn a_csp_report_keeps_only_its_known_fields() {
    let (_server, mut browser) = browser().await;
    let start = log_marker();
    let mut body = csp_report("/login", "script-src-elem", "inline");
    body["csp-report"]["script-sample"] = json!("sample-of-the-page");
    body["csp-report"]["referrer"] = json!("https://coffer.test/referrer-text");
    body["csp-report"]["new-browser-field"] = json!({ "nested": "new-field-text" });
    post_csp(&mut browser, &body).await;
    let log = log_since(&start);
    assert!(log.contains("script-src-elem blocked inline"));
    for text in ["sample-of-the-page", "referrer-text", "new-field-text"] {
        assert!(!log.contains(text), "{text}");
    }
}

#[tokio::test]
async fn a_kind_outside_the_enum_is_dropped() {
    let (_server, mut browser) = browser().await;
    let start = log_marker();
    for kind in ["warning", "ERROR", "", "csp"] {
        let mut body = script_report("kind-refused");
        body["kind"] = json!(kind);
        let reply = post_script(&mut browser, &body).await;
        assert_eq!(reply.status, StatusCode::NO_CONTENT, "{kind}");
    }
    assert!(!log_since(&start).contains("kind-refused"));
}

#[tokio::test]
async fn a_line_that_is_not_a_small_number_drops_the_report() {
    let (_server, mut browser) = browser().await;
    let start = log_marker();
    for line in [json!(-1), json!(1.5), json!("12"), json!(u64::MAX)] {
        let mut body = script_report("line-refused");
        body["line"] = line;
        post_script(&mut browser, &body).await;
    }
    assert!(!log_since(&start).contains("line-refused"));
}
