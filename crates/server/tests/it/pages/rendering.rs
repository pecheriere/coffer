//! Theming, accessibility and error pages across every page.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions unwrap to fail the test"
)]

use super::get_and_head;
use axum::http::{StatusCode, header};
use coffer_domain::identity::Role;
use coffer_server::assets::CSS_BASE_CSS;
use coffer_server::testkit::TestServer;
use coffer_server::testkit::browser::{Browser, Reply, with_admin};
use coffer_server::themes;

/// Every page a household sees, as (page, reply): sign-in, a bootstrap
/// setup link, a new-member invite link, and the signed-in admin's account,
/// connections, members, invites and household pages. Bob is a member, so the members page lists
/// someone the admin manages.
async fn every_page() -> Vec<(&'static str, Reply)> {
    let server = TestServer::new().await;
    let setup = server.identity.bootstrap_link().await.unwrap().unwrap();
    let mut visitor = Browser::new(&server);
    let mut pages = vec![
        ("/login", visitor.get("/login").await),
        ("/setup/<token>", visitor.get(setup.path()).await),
    ];

    let mut admin = Browser::new(&server);
    let mut alice_key = server.authenticator();
    let done = admin.enroll(&mut alice_key, &setup, "alice", "Alice").await;
    assert_eq!(done.status, StatusCode::OK, "`done` status");
    let actor = server
        .sessions
        .resolve(admin.cookie.as_deref().unwrap())
        .await
        .unwrap();
    let for_bob = server
        .identity
        .create_invite(&actor, Role::Member, None)
        .await
        .unwrap();
    let mut bob = Browser::new(&server);
    let mut bob_key = server.authenticator();
    let done = bob.enroll(&mut bob_key, &for_bob, "bob", "Bob").await;
    assert_eq!(done.status, StatusCode::OK, "`done` status");
    let pending = server
        .identity
        .create_invite(&actor, Role::Member, None)
        .await
        .unwrap();
    pages.push(("/invite/<token>", visitor.get(pending.path()).await));

    for path in [
        "/account",
        "/admin/members",
        "/admin/invites",
        "/admin/household",
    ] {
        pages.push((path, admin.get(path).await));
    }
    pages
}

/// The opening tags `<name …>` in `html`.
fn tags<'a>(html: &'a str, name: &str) -> Vec<(usize, &'a str)> {
    let open = format!("<{name}");
    html.match_indices(&open)
        .filter(|(at, _)| {
            html[at + open.len()..].starts_with(|c: char| c.is_whitespace() || c == '>' || c == '/')
        })
        .map(|(at, _)| {
            let end = html[at..].find('>').map_or(html.len(), |len| at + len + 1);
            (at, &html[at..end])
        })
        .collect()
}

/// The value of attribute `name` in the opening tag `tag`.
fn attribute<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let key = format!(" {name}=\"");
    let from = tag.find(&key)? + key.len();
    let len = tag[from..].find('"')?;
    Some(&tag[from..from + len])
}

#[tokio::test]
async fn every_page_is_themed_and_inline_free() {
    for (_, reply) in every_page().await {
        assert_eq!(reply.status, StatusCode::OK);
        let body = &reply.body;
        let theme = format!("href=\"{}\"", themes::DEFAULT.stylesheet.url);
        let base = format!("href=\"{}\"", CSS_BASE_CSS.url);
        let theme_at = body
            .find(&theme)
            .unwrap_or_else(|| panic!("the page lacks the theme stylesheet"));
        let base_at = body
            .find(&base)
            .unwrap_or_else(|| panic!("the page lacks the base stylesheet"));
        assert!(theme_at < base_at, "tokens must load before components");
        for expected in [
            "<meta name=\"color-scheme\" content=\"light dark\">",
            "href=\"#main\"",
            "<main id=\"main\"",
        ] {
            assert!(body.contains(expected), "missing {expected}");
        }
        assert!(
            !body.contains("style="),
            "the page has an inline style attribute"
        );
        assert!(
            tags(body, "style").is_empty(),
            "the page has a <style> element"
        );
        for (_, script) in tags(body, "script") {
            assert!(
                attribute(script, "src").is_some(),
                "an inline script is present"
            );
        }
    }
}

#[tokio::test]
async fn sign_in_page_greets_with_the_mascot() {
    let server = TestServer::new().await;
    let page = Browser::new(&server).get("/login").await;
    assert_eq!(page.status, StatusCode::OK);
    for expected in [
        "illustration-welcome",
        "id=\"login\"",
        "Sign in with a passkey",
    ] {
        assert!(page.body.contains(expected), "missing {expected}");
    }
}

#[tokio::test]
async fn empty_states_show_the_resting_mascot() {
    let (_server, mut admin, _) = with_admin().await;
    let account = admin.get("/account").await;
    assert!(account.body.contains("No agent tokens yet"));
    assert!(account.body.contains("illustration-empty"));
    let invites = admin.get("/admin/invites").await;
    assert!(invites.body.contains("No pending invites."));
}

#[tokio::test]
async fn every_form_control_has_an_accessible_name() {
    for (page, reply) in every_page().await {
        let body = &reply.body;
        let labelled: Vec<&str> = tags(body, "label")
            .into_iter()
            .filter_map(|(_, tag)| attribute(tag, "for"))
            .collect();
        let controls = tags(body, "select")
            .into_iter()
            .chain(tags(body, "input"))
            .filter(|(_, tag)| attribute(tag, "type") != Some("hidden"));
        for (at, tag) in controls {
            let id = attribute(tag, "id");
            let enclosed = body[..at]
                .rfind("<label")
                .is_some_and(|label| !body[label..at].contains("</label>"));
            let named = attribute(tag, "aria-label").is_some()
                || id.is_some_and(|id| labelled.contains(&id))
                || enclosed;
            assert!(named, "a control has no accessible name");
        }
        if page == "/admin/members" {
            assert!(
                body.contains("aria-label=\"Role for bob\""),
                "the role select names its member"
            );
        }
    }
}

/// The text of every main-navigation link that marks the current page.
fn current_nav_links(body: &str) -> Vec<String> {
    let start = body.find("<nav").unwrap();
    let nav = &body[start..start + body[start..].find("</nav>").unwrap()];
    tags(nav, "a")
        .into_iter()
        .filter(|(_, tag)| attribute(tag, "aria-current") == Some("page"))
        .map(|(at, tag)| {
            let text = &nav[at + tag.len()..];
            text[..text.find("</a>").unwrap()].to_owned()
        })
        .collect()
}

#[tokio::test]
async fn nav_marks_the_current_page() {
    let (_server, mut admin, _) = with_admin().await;
    for (path, current) in [("/account", "Account"), ("/admin/invites", "Invites")] {
        let page = admin.get(path).await;
        assert_eq!(
            current_nav_links(&page.body),
            vec![current.to_owned()],
            "{path}"
        );
        assert_eq!(page.body.matches("aria-current=").count(), 1, "{path}");
    }
}

/// The security headers and the themed body every error page carries.
fn assert_themed_error(reply: &Reply, status: StatusCode) {
    assert_eq!(reply.status, status, "`reply` status");
    assert!(reply.header(header::CONTENT_TYPE).starts_with("text/html"));
    assert_eq!(reply.header(header::CACHE_CONTROL), "no-store");
    assert_eq!(
        reply.header(header::CONTENT_SECURITY_POLICY),
        "default-src 'self'; script-src 'self'; style-src 'self'; frame-ancestors 'none'; \
         form-action 'self'; base-uri 'none'"
    );
    assert!(
        reply
            .body
            .contains(&format!("href=\"{}\"", themes::DEFAULT.stylesheet.url))
    );
    assert!(reply.body.contains("illustration-error"));
    assert!(reply.body.contains("href=\"/account\""));
}

#[tokio::test]
async fn unknown_page_renders_a_themed_not_found() {
    let server = TestServer::new().await;
    let mut browser = Browser::new(&server);

    let [get, head] = get_and_head(&mut browser, "/nope").await;

    assert_themed_error(&get, StatusCode::NOT_FOUND);
    assert!(get.body.contains("Not found"));
    assert_eq!(head.status, StatusCode::NOT_FOUND);
}

/// A page URL revisited with the wrong method (the token-created page is
/// drawn at `/account/tokens`, so history or a bookmark sends a GET) gets
/// the themed error page, not a blank one.
#[tokio::test]
async fn wrong_method_on_a_page_renders_a_themed_error() {
    let (_server, mut admin, _) = with_admin().await;

    for path in ["/account/tokens", "/logout"] {
        let reply = admin.get(path).await;

        assert_themed_error(&reply, StatusCode::METHOD_NOT_ALLOWED);
        assert!(reply.body.contains("Method not allowed"), "{path}");
        assert_eq!(reply.header(header::ALLOW), "POST", "{path}");
    }
}

#[tokio::test]
async fn error_page_with_a_stale_cookie_still_renders() {
    let (server, mut admin, _) = with_admin().await;
    let invite = server
        .identity
        .create_invite(
            &server
                .sessions
                .resolve(admin.cookie.as_deref().unwrap())
                .await
                .unwrap(),
            Role::Member,
            None,
        )
        .await
        .unwrap();
    let mut bob = Browser::new(&server);
    let mut bob_key = server.authenticator();
    assert_eq!(
        bob.enroll(&mut bob_key, &invite, "bob", "Bob").await.status,
        StatusCode::OK
    );
    let members = admin.get("/admin/members").await;
    let csrf = members.csrf();
    let bob_row = Reply {
        body: members.body[members.body.find("class=\"item-title\">bob<").unwrap()..].to_owned(),
        ..members
    };
    let bob_id = bob_row.between("action=\"/admin/members/", "/role\"");
    let disabled = admin
        .post_form(
            &format!("/admin/members/{bob_id}/disable"),
            &[("csrf", &csrf)],
        )
        .await;
    assert_eq!(disabled.status, StatusCode::SEE_OTHER);
    let mut garbage = Browser::new(&server);
    garbage.cookie = Some("garbage".to_owned());

    for visitor in [&mut garbage, &mut bob] {
        let reply = visitor.get("/nope").await;
        assert_themed_error(&reply, StatusCode::NOT_FOUND);
        let account = visitor.get("/account").await;
        assert_eq!(account.status, StatusCode::SEE_OTHER);
        assert_eq!(account.header(header::LOCATION), "/login");
    }
}

#[tokio::test]
async fn conflict_pages_are_themed() {
    let (_server, mut admin, _) = with_admin().await;
    let account = admin.get("/account").await;
    let id = account.between("action=\"/account/passkeys/", "/remove\"");

    let reply = admin
        .post_form(
            &format!("/account/passkeys/{id}/remove"),
            &[("csrf", &account.csrf())],
        )
        .await;

    assert_themed_error(&reply, StatusCode::CONFLICT);
    assert!(reply.body.contains("last passkey"));
}
