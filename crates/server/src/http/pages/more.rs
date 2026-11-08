//! The More page (spec §5): everything that isn't Home, Money or Activity, as links. It shows
//! no amount and no name but the viewer's, and links to the household's pages only for
//! whoever may open them.

use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use maud::{Markup, html};

use super::PageSession;
use super::account::viewer;
use super::layout::{Frame, Nav, Viewer, page};
use crate::http::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/more", get(more_page))
}

/// A destination: where it goes, what it is called and what it holds.
type Link = (&'static str, &'static str, &'static str);

const YOUR_MONEY: [Link; 3] = [
    (
        "/accounts",
        "Accounts",
        "Every account, its balance and its history",
    ),
    (
        "/net-worth",
        "Net worth history",
        "What you own minus what you owe, over time",
    ),
    (
        "/forecast",
        "Cash forecast",
        "The cash you will have, month by month",
    ),
];

const SETUP: [Link; 3] = [
    (
        "/categories",
        "Categories",
        "The groups spending is filed under",
    ),
    ("/rules", "Rules", "What fills in new transactions for you"),
    ("/connections", "Connections", "The banks coffer reads from"),
];

const HOUSEHOLD: Link = (
    "/admin/household",
    "Household",
    "Currency, time zone and exchange rates",
);

const PEOPLE: [Link; 2] = [
    ("/admin/members", "Members", "Who belongs to the household"),
    (
        "/admin/invites",
        "Invites",
        "Links to join that haven't been used yet",
    ),
];

const YOU: Link = ("/account", "You", "Passkeys, tokens and appearance");

async fn more_page(State(state): State<AppState>, session: PageSession) -> Response {
    let viewer = viewer(&state, &session).await;
    let frame = Frame {
        look: session.look(&state).await,
        viewer: Some(&viewer),
        current: Some(Nav::More),
        narrow: false,
    };
    page(&frame, "More", &content(&viewer)).into_response()
}

fn content(viewer: &Viewer) -> Markup {
    let household: Vec<Link> = viewer
        .can_manage_household
        .then_some(HOUSEHOLD)
        .into_iter()
        .chain(
            viewer
                .can_manage_members
                .then_some(PEOPLE)
                .into_iter()
                .flatten(),
        )
        .collect();
    html! {
        h1 { "More" }
        (section("Your money", &YOUR_MONEY))
        (section("Setup", &SETUP))
        @if !household.is_empty() {
            (section("Household", &household))
        }
        (section("You", &[YOU]))
    }
}

fn section(heading: &str, links: &[Link]) -> Markup {
    html! {
        section.card {
            h2 { (heading) }
            ul.items {
                @for (href, label, about) in links {
                    li.item {
                        div.item-main {
                            a.item-title href=(href) { (label) }
                            span.item-meta { (about) }
                        }
                    }
                }
            }
        }
    }
}
