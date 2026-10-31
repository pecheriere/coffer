//! The shared page frame. Pages carry no inline script or style: behaviour
//! lives in the script asset and looks in the stylesheet asset.

use maud::{DOCTYPE, Markup, html};

use crate::assets;

/// The signed-in visitor, for the navigation bar.
pub struct Viewer {
    pub display_name: String,
    pub is_admin: bool,
    pub csrf: String,
}

/// A hidden anti-forgery field for a form.
pub fn csrf_field(csrf: &str) -> Markup {
    html! { input type="hidden" name="csrf" value=(csrf); }
}

/// A full page.
pub fn page(title: &str, viewer: Option<&Viewer>, content: &Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (title) " · coffer" }
                link rel="stylesheet" href=(assets::CSS_BASE_CSS.url);
                script src=(assets::JS_COFFER_JS.url) defer {}
            }
            body {
                @if let Some(viewer) = viewer {
                    nav {
                        a href="/account" { "Account" }
                        @if viewer.is_admin {
                            a href="/admin/members" { "Members" }
                            a href="/admin/invites" { "Invites" }
                        }
                        span.spacer {}
                        span { (viewer.display_name) }
                        form.inline method="post" action="/logout" {
                            (csrf_field(&viewer.csrf))
                            button type="submit" { "Sign out" }
                        }
                    }
                }
                main { (content) }
            }
        }
    }
}

/// A generic error page. `detail` is a message safe to show, never input.
pub fn error_page(title: &str, detail: Option<&str>) -> Markup {
    let content = html! {
        h1 { (title) }
        @if let Some(detail) = detail {
            p { (detail) }
        }
        p { a href="/account" { "Back to coffer" } }
    };
    page(title, None, &content)
}

/// A secret shown once, with a copy button.
pub fn once(label: &str, id: &str, secret: &str) -> Markup {
    html! {
        section.once {
            p { strong { (label) } " Copy it now: it is shown only once." }
            p { code id=(id) { (secret) } }
            button type="button" data-copy=(id) { "Copy" }
        }
    }
}
