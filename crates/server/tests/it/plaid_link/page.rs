//! The Link page: its content security policy, nonce and the link token it
//! carries.

use super::{
    CREDENTIALS, DEFAULT_CSP, EXCHANGE_URL, LINK_CSP, LINK_MARKER, LINK_SCRIPT, LINK_TOKEN,
    LINK_TOKEN_CREATE, NEW_PAGE, confirm_path, csp, policy_nonce, reconnect_path, tags, world,
};
use axum::http::{StatusCode, header};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use coffer_server::assets;
use coffer_server::sync::provider::ProviderKind;
use coffer_server::testkit::browser::{Browser, with_admin};
use coffer_server::testkit::{TEST_PUBLIC_URL, TestServer, captured_log_text, log_marker};
use serde_json::json;

#[tokio::test]
async fn plaid_pages_absent_without_config() {
    let (server, mut admin, _) = with_admin().await;
    let alice = server
        .sessions
        .resolve(admin.cookie.as_deref().unwrap())
        .await
        .unwrap();
    let id = server
        .seed_connection(ProviderKind::Plaid, "Old", alice.member, CREDENTIALS)
        .await;
    let csrf = admin.get("/connections").await.csrf();

    assert_eq!(admin.get(NEW_PAGE).await.status, StatusCode::NOT_FOUND);
    assert_eq!(
        admin.get(&reconnect_path(id)).await.status,
        StatusCode::NOT_FOUND
    );
    for path in [EXCHANGE_URL.to_owned(), confirm_path(id)] {
        let reply = admin
            .post_json_with(&path, Some(TEST_PUBLIC_URL), Some(&csrf), &json!({}))
            .await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND);
    }
    let page = admin.get("/connections").await;
    assert!(
        !page.body.contains("/plaid/"),
        "no Plaid link without Plaid configured"
    );
    assert!(!page.body.contains("cdn.plaid.com"));
}

#[tokio::test]
async fn link_page_has_plaid_csp_only_there() {
    let mut world = world().await;
    let id = world.needing_login().await;
    world.script.link_token();
    world.script.link_token();

    for path in [NEW_PAGE.to_owned(), reconnect_path(id)] {
        let page = world.admin.get(&path).await;
        assert_eq!(page.status, StatusCode::OK);
        let expected = LINK_CSP.replace("{nonce}", &policy_nonce(&page));
        assert_eq!(csp(&page), expected);
        assert!(!csp(&page).contains("unsafe-"), "never an unsafe source");
    }

    for path in [
        "/connections",
        "/account",
        "/login",
        "/healthz",
        "/api/v1/me",
        "/no-such-page",
        assets::CSS_BASE_CSS.url,
        assets::JS_PLAID_LINK_JS.url,
    ] {
        let reply = world.admin.get(path).await;
        assert_eq!(csp(&reply), DEFAULT_CSP, "every other route: {path}");
    }

    // The endpoints the page calls keep the default too, in success and
    // refusal, and the same pages on a server without Plaid.
    for path in ["/connections", "/account", "/login"] {
        let reply = world.admin.get(path).await;
        assert!(!reply.body.contains("nonce"), "no other page has a nonce");
    }
    let refused = world.exchange(&json!({})).await;
    assert_eq!(csp(&refused), DEFAULT_CSP);
    let confirmed = world.confirm(id).await;
    assert_eq!(confirmed.status, StatusCode::OK);
    assert_eq!(csp(&confirmed), DEFAULT_CSP);

    let (_, mut plain, _) = with_admin().await;
    for path in ["/connections", "/account", NEW_PAGE] {
        assert_eq!(csp(&plain.get(path).await), DEFAULT_CSP);
    }
    let mut anonymous = Browser::new(&TestServer::new().await);
    assert_eq!(csp(&anonymous.get("/login").await), DEFAULT_CSP);
}

#[tokio::test]
async fn link_page_failure_keeps_default_csp() {
    let mut world = world().await;
    world.script.reply(
        LINK_TOKEN_CREATE,
        StatusCode::BAD_REQUEST,
        json!({ "error_type": "INVALID_REQUEST", "error_code": "MISSING_FIELDS" }).to_string(),
    );

    let page = world.admin.get(NEW_PAGE).await;

    assert_eq!(page.status, StatusCode::BAD_GATEWAY);
    assert_eq!(csp(&page), DEFAULT_CSP);
    assert!(!page.body.contains(LINK_SCRIPT), "no third-party script");
    assert!(!page.body.contains("data-link-token"));
    assert!(!page.body.contains("nonce"), "no nonce on a failed page");
}

/// Plaid's script reads its own tag's nonce for the stylesheet it adds, so the
/// tag and the policy must agree, and no other element may hold the nonce.
#[tokio::test]
async fn nonce_is_on_plaids_script_tag_only() {
    let mut world = world().await;
    let id = world.needing_login().await;
    world.script.link_token();
    world.script.link_token();

    for path in [NEW_PAGE.to_owned(), reconnect_path(id)] {
        let page = world.admin.get(&path).await;
        let nonce = policy_nonce(&page);
        let scripts = tags(&page.body, "script");
        let plaid: Vec<&&str> = scripts
            .iter()
            .filter(|script| script.contains(LINK_SCRIPT))
            .collect();
        assert_eq!(plaid.len(), 1, "one Plaid script");
        assert!(
            plaid[0].contains(&format!(" nonce=\"{nonce}\"")),
            "the tag's nonce is the policy's"
        );
        assert_eq!(
            page.body.matches("nonce=").count(),
            1,
            "one nonce attribute"
        );
        assert_eq!(
            page.body.matches(&nonce).count(),
            1,
            "the value appears once"
        );
    }
}

#[tokio::test]
async fn every_render_has_a_fresh_nonce_and_logs_none() {
    let mut world = world().await;
    world.script.link_token();
    world.script.link_token();
    let marker = log_marker();

    let first = policy_nonce(&world.admin.get(NEW_PAGE).await);
    let second = policy_nonce(&world.admin.get(NEW_PAGE).await);

    assert!(first != second, "two renders never share a nonce");
    let bytes = URL_SAFE_NO_PAD.decode(&first).unwrap();
    assert!(bytes.len() >= 16, "at least 128 bits");
    let log = captured_log_text();
    assert!(log.contains(&marker), "the capture still holds the marker");
    for nonce in [&first, &second] {
        assert!(!log.contains(nonce.as_str()), "a nonce is never logged");
    }
}

#[tokio::test]
async fn link_page_has_no_inline_script() {
    let mut world = world().await;
    world.script.link_token();

    let page = world.admin.get(NEW_PAGE).await;

    let scripts = tags(&page.body, "script");
    assert!(
        scripts.iter().all(|script| script.contains(" src=\"")),
        "every script has a src"
    );
    for src in [
        assets::JS_COFFER_JS.url,
        LINK_SCRIPT,
        assets::JS_PLAID_LINK_JS.url,
    ] {
        let tag = format!(" src=\"{src}\"");
        assert!(
            scripts.iter().any(|script| script.contains(&tag)),
            "a script is missing from the page"
        );
    }
    assert_eq!(scripts.len(), 3, "no other script");
    assert!(!page.body.contains("style="), "no inline style");
    assert_eq!(tags(&page.body, "style").len(), 0);
    for handler in ["onclick", "onload", "onerror"] {
        assert!(!page.body.contains(handler), "no inline event handler");
    }
    assert!(page.body.contains("aria-current=\"page\">Connections<"));

    let script = world.admin.get(assets::JS_PLAID_LINK_JS.url).await;
    assert_eq!(script.status, StatusCode::OK);
    assert!(
        script
            .header(header::CONTENT_TYPE)
            .starts_with("text/javascript")
    );
}

#[tokio::test]
async fn link_token_is_in_the_page_and_no_log() {
    let mut world = world().await;
    world.script.link_token();
    let marker = log_marker();

    let page = world.admin.get(NEW_PAGE).await;

    let attribute = format!("data-link-token=\"{LINK_TOKEN}\"");
    assert!(page.body.contains(&attribute), "the token is in the page");
    assert_eq!(page.header(header::CACHE_CONTROL), "no-store");
    assert_eq!(page.header(header::REFERRER_POLICY), "same-origin");
    let log = captured_log_text();
    assert!(log.contains(&marker), "the capture still holds the marker");
    assert!(!log.contains(LINK_MARKER), "the token must not be logged");
    assert!(
        !world.audit_text().await.contains(LINK_MARKER),
        "the token must not be audited"
    );
}

#[tokio::test]
async fn new_page_asks_for_a_new_item() {
    let mut world = world().await;
    world.script.link_token();

    let page = world.admin.get(NEW_PAGE).await;

    assert!(page.body.contains("Connect with Plaid"));
    assert!(page.body.contains("data-csrf=\""));
    assert!(
        page.body
            .contains(&format!("data-submit=\"{EXCHANGE_URL}\""))
    );
    assert!(
        tags(&page.body, "input")
            .iter()
            .any(|input| input.contains("plaid-label"))
    );
    let seen = world.script.seen(LINK_TOKEN_CREATE);
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0]["products"], json!(["transactions"]));
    assert!(seen[0].get("access_token").is_none());
    assert_eq!(
        seen[0]["user"]["client_user_id"],
        world.alice.member.to_string()
    );
}

#[tokio::test]
async fn connections_page_offers_plaid_when_configured() {
    let mut world = world().await;
    let needing = world.needing_login().await;
    let healthy = world.seeded().await;

    let page = world.admin.get("/connections").await;

    assert_eq!(csp(&page), DEFAULT_CSP);
    assert!(page.body.contains(&format!("href=\"{NEW_PAGE}\"")));
    assert!(
        page.body
            .contains(&format!("href=\"{}\"", reconnect_path(needing)))
    );
    assert!(
        !page.body.contains(&reconnect_path(healthy)),
        "only a connection that needs it"
    );
    assert!(
        !page
            .body
            .contains(&format!("/connections/{needing}/simplefin/reconnect")),
        "a Plaid connection is not offered the SimpleFin form"
    );
    assert!(!page.body.contains("cdn.plaid.com"));
}
