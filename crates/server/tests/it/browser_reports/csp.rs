//! CSP violation reports, and the one violation Plaid's script always causes.

use coffer_server::assets::JS_COFFER_JS;
use serde_json::json;

use super::{browser, csp_events, csp_report};

/// What `body`, a CSP report, logs.
async fn logged(body: &serde_json::Value) -> String {
    let (_server, mut browser) = browser().await;
    csp_events(&mut browser, body).await
}

#[tokio::test]
async fn a_violation_logs_its_directive_and_page_template() {
    let log = logged(&csp_report(
        "/invite/abc123tok?a=b",
        "style-src-attr",
        "inline",
    ))
    .await;
    assert!(log.contains("kind=\"csp\""), "{log}");
    assert!(
        log.contains("client_message=\"style-src-attr blocked inline\""),
        "{log}"
    );
    assert!(log.contains("page=\"/invite/{token}\""), "{log}");
    assert!(!log.contains("abc123tok"), "{log}");
}

#[tokio::test]
async fn a_blocked_address_is_never_logged() {
    let log = logged(&csp_report(
        "/login",
        "img-src",
        "https://tracker.test/pixel?id=abc123tok",
    ))
    .await;
    assert!(
        log.contains("client_message=\"img-src blocked other\""),
        "{log}"
    );
    assert!(
        !log.contains("tracker.test") && !log.contains("abc123tok"),
        "{log}"
    );
}

#[tokio::test]
async fn a_same_origin_blocked_address_is_not_logged_either() {
    let log = logged(&csp_report(
        "/login",
        "img-src",
        "https://coffer.test/invite/abc123tok",
    ))
    .await;
    assert!(
        log.contains("client_message=\"img-src blocked same-origin\""),
        "{log}"
    );
    assert!(!log.contains("abc123tok"), "{log}");
}

#[tokio::test]
async fn a_directive_outside_the_list_is_other() {
    let log = logged(&csp_report("/login", "forged\r\nERROR text", "inline")).await;
    assert!(
        log.contains("client_message=\"other blocked inline\""),
        "{log}"
    );
    assert!(!log.contains("forged"), "{log}");
}

/// Older reports name only the violated directive.
#[tokio::test]
async fn the_violated_directive_stands_in_for_an_absent_effective_one() {
    let body = json!({ "csp-report": {
        "document-uri": "https://coffer.test/login",
        "violated-directive": "img-src 'self'",
        "blocked-uri": "inline",
    }});
    let log = logged(&body).await;
    assert!(
        log.contains("client_message=\"img-src blocked inline\""),
        "{log}"
    );
}

#[tokio::test]
async fn the_source_of_a_violation_follows_the_script_rule() {
    let mut body = csp_report("/login", "style-src", "inline");
    body["csp-report"]["source-file"] = json!(format!("https://coffer.test{}", JS_COFFER_JS.url));
    body["csp-report"]["column-number"] = json!(9);
    let log = logged(&body).await;
    assert!(
        log.contains(&format!("script=\"{}\"", JS_COFFER_JS.url)),
        "{log}"
    );
    assert!(log.contains("column=9"), "{log}");
}

/// Plaid's script checks for inline style support on every Link page, and the
/// threat model expects exactly that violation.
#[tokio::test]
async fn plaid_s_expected_style_check_is_not_logged() {
    for path in [
        "/connections/plaid/new",
        "/connections/0190b5f0-1111-7000-8000-000000000000/plaid/reconnect",
    ] {
        let log = logged(&csp_report(path, "style-src-elem", "inline")).await;
        assert_eq!(log, "", "{path}");
    }
}

#[tokio::test]
async fn the_same_violation_on_any_other_page_is_logged() {
    let log = logged(&csp_report("/login", "style-src-elem", "inline")).await;
    assert!(log.contains("style-src-elem blocked inline"), "{log}");
}

#[tokio::test]
async fn any_other_violation_on_a_link_page_is_logged() {
    let log = logged(&csp_report(
        "/connections/plaid/new",
        "script-src-elem",
        "inline",
    ))
    .await;
    assert!(log.contains("script-src-elem blocked inline"), "{log}");
    let log = logged(&csp_report(
        "/connections/plaid/new",
        "style-src-elem",
        "https://evil.test/sheet.css",
    ))
    .await;
    assert!(log.contains("style-src-elem blocked other"), "{log}");
}
