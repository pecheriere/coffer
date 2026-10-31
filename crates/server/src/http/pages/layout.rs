//! The shared page frame and components. Pages carry no inline script or
//! style: behaviour lives in the script asset and looks in the theme and
//! component stylesheets.

use coffer_domain::identity::ColorScheme;
use maud::{DOCTYPE, Markup, PreEscaped, html};

use crate::assets;
use crate::themes::Look;
use crate::themes::illustrations::Illustration;

/// The signed-in visitor, for the navigation bar.
pub struct Viewer {
    pub display_name: String,
    pub is_admin: bool,
    pub csrf: String,
}

/// A main-navigation destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    Account,
    Members,
    Invites,
    Household,
}

/// How a page is framed: its look, who is signed in, which navigation item
/// is current, and whether the content column is narrow (sign-in, setup and
/// error pages).
pub struct Frame<'a> {
    pub look: Look,
    pub viewer: Option<&'a Viewer>,
    pub current: Option<Nav>,
    pub narrow: bool,
}

/// A hidden anti-forgery field for a form.
pub fn csrf_field(csrf: &str) -> Markup {
    html! { input type="hidden" name="csrf" value=(csrf); }
}

/// A full page.
pub fn page(frame: &Frame<'_>, title: &str, content: &Markup) -> Markup {
    let look = frame.look;
    let (color_scheme, data_scheme) = match look.scheme {
        ColorScheme::System => ("light dark", None),
        ColorScheme::Light => ("light", Some("light")),
        ColorScheme::Dark => ("dark", Some("dark")),
    };
    let main_class = if frame.narrow {
        "page page-narrow"
    } else {
        "page"
    };
    html! {
        (DOCTYPE)
        html lang="en" data-scheme=[data_scheme] {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                meta name="color-scheme" content=(color_scheme);
                title { (title) " · coffer" }
                link rel="icon" type="image/svg+xml" href=(assets::ICONS_FAVICON_SVG.url);
                link rel="icon" href="/favicon.ico" sizes="48x48";
                link rel="apple-touch-icon" href=(assets::ICONS_APPLE_TOUCH_ICON_PNG.url);
                link rel="manifest" href=(assets::MANIFEST_WEBMANIFEST.url);
                link rel="stylesheet" href=(look.theme.stylesheet.url);
                link rel="stylesheet" href=(assets::CSS_BASE_CSS.url);
                script src=(assets::JS_COFFER_JS.url) defer {}
            }
            body {
                a.skip href="#main" { "Skip to content" }
                @if let Some(viewer) = frame.viewer {
                    (topbar(look, viewer, frame.current))
                }
                main id="main" class=(main_class) { (content) }
            }
        }
    }
}

fn topbar(look: Look, viewer: &Viewer, current: Option<Nav>) -> Markup {
    let link = |nav: Nav, href: &str, label: &str| {
        html! {
            a href=(href) aria-current=[(current == Some(nav)).then_some("page")] { (label) }
        }
    };
    html! {
        header.topbar {
            a.brand href="/account" {
                (illustration(&look.theme.illustrations.glyph, 28, "glyph"))
                "coffer"
            }
            nav.nav aria-label="Main" {
                (link(Nav::Account, "/account", "Account"))
                @if viewer.is_admin {
                    (link(Nav::Members, "/admin/members", "Members"))
                    (link(Nav::Invites, "/admin/invites", "Invites"))
                    (link(Nav::Household, "/admin/household", "Household"))
                }
            }
            span.who {
                (viewer.display_name)
                form method="post" action="/logout" {
                    (csrf_field(&viewer.csrf))
                    button.btn type="submit" { "Sign out" }
                }
            }
        }
    }
}

/// The card that greets a visitor on the sign-in and setup pages.
pub fn auth_card(look: Look, heading: &str, content: &Markup) -> Markup {
    html! {
        section.card.welcome {
            (illustration(&look.theme.illustrations.welcome, 96, "welcome"))
            p.wordmark { "coffer" }
            h1 { (heading) }
            (content)
        }
    }
}

/// A list with nothing in it yet: the resting mascot and a message.
pub fn empty_state(look: Look, message: &str) -> Markup {
    html! {
        div.empty {
            (illustration(&look.theme.illustrations.empty, 72, "empty"))
            p { (message) }
        }
    }
}

/// A theme illustration, decorative and hidden from assistive technology.
pub fn illustration(art: &Illustration, size: u32, slot: &str) -> Markup {
    html! {
        svg class={ "illustration illustration-" (slot) } viewBox=(art.view_box)
            width=(size) height=(size) aria-hidden="true" focusable="false" {
            // A compile-time constant, checked inert by the theme tests.
            (PreEscaped(art.body))
        }
    }
}

/// The passkey key, drawn in the text colour.
pub fn key_icon() -> Markup {
    html! {
        svg.icon viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor"
            stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" {
            circle cx="8" cy="15" r="4.5" {}
            path d="M11.2 11.8L20 3M16.5 6.5l2.5 2.5M14 9l2 2" {}
        }
    }
}

/// The card of an error page. `detail` is a message safe to show, never
/// input.
pub fn error_body(look: Look, title: &str, detail: Option<&str>) -> Markup {
    html! {
        section.card.welcome {
            (illustration(&look.theme.illustrations.error, 120, "error"))
            h1 { (title) }
            @if let Some(detail) = detail {
                p { (detail) }
            }
            a.btn.btn-primary href="/account" { "Back to coffer" }
        }
    }
}

/// A secret shown once, with a copy button.
pub fn once(label: &str, id: &str, secret: &str) -> Markup {
    html! {
        section.once {
            p { strong { (label) } " Copy it now: it is shown only once." }
            code id=(id) { (secret) }
            button.btn type="button" data-copy=(id) { "Copy" }
        }
    }
}
