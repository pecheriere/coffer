//! The four tabs every signed-in page shows, and the More page that holds the rest.

use axum::http::StatusCode;
use coffer_domain::identity::Role;
use coffer_server::testkit::TestServer;
use coffer_server::testkit::browser::{Browser, with_admin};

/// The tabs, in order, as each is written in the page.
const TABS: [&str; 4] = [
    "href=\"/\">Home<",
    "href=\"/plan\">Money<",
    "href=\"/transactions\">Activity<",
    "href=\"/more\">More<",
];

/// Whatever More links to for every member.
const EVERY_MEMBERS: [&str; 7] = [
    "/accounts",
    "/net-worth",
    "/forecast",
    "/categories",
    "/rules",
    "/connections",
    "/account",
];

/// What More links to only for whoever runs the household.
const ADMINS_ONLY: [&str; 3] = ["/admin/household", "/admin/members", "/admin/invites"];

/// The main navigation of `body`.
fn main_nav(body: &str) -> &str {
    let start = body.find("<nav class=\"nav\" aria-label=\"Main\"").unwrap();
    let len = body[start..].find("</nav>").unwrap();
    &body[start..start + len]
}

/// A member "bob", enrolled by invite from the signed-in admin and signed in.
async fn member(server: &TestServer, admin: &Browser) -> Browser {
    let alice = server
        .sessions
        .resolve(admin.cookie.as_deref().unwrap())
        .await
        .unwrap();
    let invite = server
        .identity
        .create_invite(&alice, Role::Member, None)
        .await
        .unwrap();
    let mut bob = Browser::new(server);
    let mut key = server.authenticator();
    let done = bob.enroll(&mut key, &invite, "bob", "Bob").await;
    assert_eq!(done.status, StatusCode::OK);
    bob
}

#[tokio::test]
async fn every_page_shows_the_four_tabs() {
    let (_server, mut admin, _) = with_admin().await;
    for (path, current) in [
        ("/", "Home"),
        ("/plan", "Money"),
        ("/transactions", "Activity"),
        ("/more", "More"),
        ("/accounts", "More"),
        ("/account", "More"),
        ("/forecast", "More"),
        ("/admin/household", "More"),
    ] {
        let page = admin.get(path).await;
        assert_eq!(page.status, StatusCode::OK, "{path}");
        let nav = main_nav(&page.body);
        let tabs = nav.replace(" aria-current=\"page\"", "");
        let mut from = 0;
        for tab in TABS {
            let at = tabs[from..]
                .find(tab)
                .unwrap_or_else(|| panic!("{path}: {tab} missing or out of order"));
            from += at + tab.len();
        }
        assert_eq!(nav.matches("<a ").count(), TABS.len(), "{path}");
        assert_eq!(nav.matches("aria-current=\"page\"").count(), 1, "{path}");
        assert!(
            nav.contains(&format!("aria-current=\"page\">{current}<")),
            "{path}: {current} is not current"
        );
    }
}

#[tokio::test]
async fn more_lists_the_rest() {
    let (server, mut admin, _) = with_admin().await;
    let page = admin.get("/more").await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("<h1>More</h1>"));
    for path in EVERY_MEMBERS.iter().chain(&ADMINS_ONLY) {
        assert!(page.body.contains(&format!("href=\"{path}\"")), "{path}");
    }

    let mut bob = member(&server, &admin).await;
    let page = bob.get("/more").await;
    assert_eq!(page.status, StatusCode::OK);
    for path in EVERY_MEMBERS {
        assert!(page.body.contains(&format!("href=\"{path}\"")), "{path}");
    }
    for path in ADMINS_ONLY {
        assert!(!page.body.contains(&format!("href=\"{path}\"")), "{path}");
    }
}

#[tokio::test]
async fn the_brand_goes_home() {
    let (_server, mut admin, _) = with_admin().await;
    let page = admin.get("/more").await;
    assert!(page.body.contains("<a class=\"brand\" href=\"/\""));
}
