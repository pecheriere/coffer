//! What a logged report holds: allow-listed fields, cut and cleaned, and never
//! a token, a query or another origin's address.

use coffer_server::assets::{JS_COFFER_JS, JS_PLAID_LINK_JS};
use coffer_server::testkit::log_marker;
use serde_json::{Value, json};

use super::{browser, log_since, post_script, script_report, the_event};

async fn event_for(body: &Value, marker: &str) -> String {
    let (_server, mut browser) = browser().await;
    post_script(&mut browser, body).await;
    the_event(marker)
}

async fn event_on_page(page: &str, marker: &str) -> String {
    let mut body = script_report(marker);
    body["page"] = json!(page);
    event_for(&body, marker).await
}

#[tokio::test]
async fn a_report_logs_one_warning_marked_as_untrusted() {
    let body = json!({
        "kind": "unhandled_rejection",
        "message": "shape-marker",
        "script": JS_COFFER_JS.url,
        "line": 12,
        "column": 34,
        "page": "/login",
    });
    let event = event_for(&body, "shape-marker").await;
    assert!(
        event.starts_with("WARN browser report (client-reported, untrusted)"),
        "{event}"
    );
    for field in [
        "source=\"browser\"",
        "trust=\"untrusted\"",
        "kind=\"unhandled_rejection\"",
        "client_message=\"shape-marker\"",
        "page=\"/login\"",
        "line=12",
        "column=34",
    ] {
        assert!(event.contains(field), "{field}: {event}");
    }
    assert!(
        event.contains(&format!("script=\"{}\"", JS_COFFER_JS.url)),
        "{event}"
    );
}

#[tokio::test]
async fn missing_fields_keep_the_shape() {
    let body = json!({ "kind": "passkey", "message": "shape-default", "page": "/login" });
    let event = event_for(&body, "shape-default").await;
    assert!(event.contains("script=\"none\""), "{event}");
    assert!(
        event.contains("line=0") && event.contains("column=0"),
        "{event}"
    );
}

#[tokio::test]
async fn a_report_naming_no_page_logs_other() {
    let (_server, mut browser) = browser().await;
    let start = log_marker();
    let body = json!({ "kind": "passkey", "message": "shape-no-page" });
    post_script(&mut browser, &body).await;
    assert!(log_since(&start).contains("page=\"other\""));
}

#[tokio::test]
async fn every_kind_of_the_enum_is_accepted() {
    for kind in ["error", "unhandled_rejection", "passkey", "plaid"] {
        let marker = format!("kind-{kind}");
        let mut body = script_report(&marker);
        body["kind"] = json!(kind);
        let event = event_for(&body, &marker).await;
        assert!(event.contains(&format!("kind=\"{kind}\"")), "{event}");
    }
}

#[tokio::test]
async fn a_long_message_is_cut_to_200_characters() {
    let long = format!("cut-marker{}", "é".repeat(500));
    let event = event_for(&script_report(&long), "cut-marker").await;
    let kept = format!("cut-marker{}", "é".repeat(190));
    assert!(
        event.contains(&format!("client_message=\"{kept}\"")),
        "{event}"
    );
}

#[tokio::test]
async fn control_and_format_characters_are_removed_from_a_message() {
    let raw =
        "clean-a\r\nclean-b\u{1b}[31mclean-c\u{7}\u{0}\u{202e}clean-d\u{2028}clean-e\u{85}clean-f";
    let event = event_for(&script_report(raw), "clean-a").await;
    assert!(
        event.contains("client_message=\"clean-a clean-b[31mclean-cclean-d clean-e clean-f\""),
        "{event}"
    );
}

#[tokio::test]
async fn a_message_holding_a_url_loses_it() {
    for (marker, raw) in [
        (
            "url-absolute",
            "url-absolute at https://coffer.test/invite/abc123tok?x=1 done",
        ),
        ("url-path", "url-path at /setup/abc123tok done"),
        ("url-encoded", "url-encoded at %2Finvite%2Fabc123tok done"),
        ("url-query", "url-query at login?next=abc123tok done"),
        ("url-glued", "url-glued at\u{200b}/invite/abc123tok done"),
    ] {
        let event = event_for(&script_report(raw), marker).await;
        assert!(!event.contains("abc123tok"), "{event}");
        assert!(event.contains("[url]"), "{event}");
    }
}

#[tokio::test]
async fn a_link_page_logs_its_template_and_not_its_token() {
    for (page, template, marker) in [
        ("/invite/abc123tok", "/invite/{token}", "tok-invite"),
        ("/setup/abc123tok", "/setup/{token}", "tok-setup"),
        (
            "https://coffer.test/invite/abc123tok?a=b#c",
            "/invite/{token}",
            "tok-full",
        ),
    ] {
        let event = event_on_page(page, marker).await;
        assert!(event.contains(&format!("page=\"{template}\"")), "{event}");
        assert!(!event.contains("abc123tok"), "{event}");
    }
}

#[tokio::test]
async fn a_page_logs_no_query_or_fragment() {
    let event = event_on_page("/login?code=abc123tok#frag-text", "query-marker").await;
    assert!(event.contains("page=\"/login\""), "{event}");
    assert!(
        !event.contains("abc123tok") && !event.contains("frag-text"),
        "{event}"
    );
}

#[tokio::test]
async fn an_id_in_a_page_path_is_logged_as_its_template() {
    let event = event_on_page(
        "/connections/0190b5f0-1111-7000-8000-000000000000/plaid/reconnect",
        "id-marker",
    )
    .await;
    assert!(
        event.contains("page=\"/connections/{id}/plaid/reconnect\""),
        "{event}"
    );
    assert!(!event.contains("0190b5f0"), "{event}");
}

#[tokio::test]
async fn every_page_of_coffer_has_a_template() {
    for (index, page) in [
        "/login",
        "/account",
        "/connections",
        "/connections/plaid/new",
        "/admin/members",
        "/admin/invites",
        "/admin/household",
    ]
    .into_iter()
    .enumerate()
    {
        let marker = format!("known-page-{index}");
        let event = event_on_page(page, &marker).await;
        assert!(event.contains(&format!("page=\"{page}\"")), "{event}");
    }
}

#[tokio::test]
async fn a_path_outside_the_table_logs_other_and_withholds_the_message() {
    let (_server, mut browser) = browser().await;
    let start = log_marker();
    let mut body = script_report("withheld-text");
    body["page"] = json!("/somewhere/abc123tok");
    post_script(&mut browser, &body).await;
    let log = log_since(&start);
    assert!(log.contains("page=\"other\""), "{log}");
    assert!(log.contains("client_message=\"[withheld]\""), "{log}");
    assert!(
        !log.contains("withheld-text") && !log.contains("abc123tok"),
        "{log}"
    );
}

/// The forecast page will show amounts. Until someone adds it to the table
/// with that in mind, a message from it stays out of the log.
#[tokio::test]
async fn a_message_from_the_forecast_page_is_withheld() {
    let (_server, mut browser) = browser().await;
    let start = log_marker();
    let mut body = script_report("cannot read 1234.56");
    body["page"] = json!("/forecast");
    post_script(&mut browser, &body).await;
    let log = log_since(&start);
    assert!(log.contains("client_message=\"[withheld]\""), "{log}");
    assert!(!log.contains("1234.56"), "{log}");
}

#[tokio::test]
async fn a_page_of_another_origin_is_other() {
    let (_server, mut browser) = browser().await;
    let start = log_marker();
    let mut body = script_report("foreign-page-text");
    for page in [
        "https://evil.test/login",
        "//evil.test/login",
        "http://coffer.test/login",
    ] {
        body["page"] = json!(page);
        post_script(&mut browser, &body).await;
    }
    let log = log_since(&start);
    assert_eq!(log.matches("page=\"other\"").count(), 3, "{log}");
}

#[tokio::test]
async fn a_first_party_script_logs_its_path_alone() {
    for (index, script) in [
        JS_COFFER_JS.url.to_owned(),
        format!("https://coffer.test{}", JS_PLAID_LINK_JS.url),
        format!("{}?v=abc123tok#frag-text", JS_COFFER_JS.url),
    ]
    .iter()
    .enumerate()
    {
        let marker = format!("script-own-{index}");
        let mut body = script_report(&marker);
        body["script"] = json!(script);
        let event = event_for(&body, &marker).await;
        assert!(event.contains("script=\"/static/js/"), "{event}");
        assert!(
            !event.contains("abc123tok") && !event.contains("frag-text"),
            "{event}"
        );
    }
}

#[tokio::test]
async fn any_other_script_is_other() {
    for (index, script) in [
        format!("https://evil.test{}", JS_COFFER_JS.url),
        format!("//evil.test{}", JS_COFFER_JS.url),
        "https://cdn.plaid.com/link/v2/stable/link-initialize.js".to_owned(),
        "/static/js/unknown.js".to_owned(),
        "/invite/abc123tok".to_owned(),
        "other".to_owned(),
        "data:text/javascript,alert(1)".to_owned(),
    ]
    .iter()
    .enumerate()
    {
        let marker = format!("script-other-{index}");
        let mut body = script_report(&marker);
        body["script"] = json!(script);
        let event = event_for(&body, &marker).await;
        assert!(event.contains("script=\"other\""), "{script}: {event}");
    }
}
