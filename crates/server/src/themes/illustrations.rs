//! Theme illustrations: inner SVG markup, wrapped in `<svg>` by the layout.
//!
//! A fragment holds shapes only (no script, style, link or event handler),
//! so it renders inline under the page's strict content security policy.

/// One SVG fragment and the `viewBox` it is drawn in.
#[derive(Debug)]
pub struct Illustration {
    pub view_box: &'static str,
    pub body: &'static str,
}

/// The illustrations a theme shows. A theme overrides the mascot by pointing
/// at its own set.
#[derive(Debug)]
pub struct Illustrations {
    /// Sign-in and first-run pages.
    pub welcome: Illustration,
    /// Empty states.
    pub empty: Illustration,
    /// Error pages.
    pub error: Illustration,
    /// The brand mark in the top bar (and the favicon).
    pub glyph: Illustration,
}

const NUGGET_RESTING: Illustration = Illustration {
    view_box: "0 0 160 160",
    body: include_str!("nugget/resting.svg"),
};

/// Nugget, the hamster mascot.
pub const NUGGET: Illustrations = Illustrations {
    welcome: Illustration {
        view_box: "0 0 160 160",
        body: include_str!("nugget/welcome.svg"),
    },
    empty: NUGGET_RESTING,
    error: NUGGET_RESTING,
    glyph: Illustration {
        view_box: "0 0 32 32",
        body: include_str!("nugget/glyph.svg"),
    },
};
