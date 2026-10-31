//! Themes: a registry of token stylesheets and illustrations.
//!
//! A theme is data. Its stylesheet (`assets/themes/<name>.css`) holds only
//! `@font-face` rules and one `:root` block of tokens; the component
//! stylesheet reads those tokens and nothing else. Adding a theme is one token
//! file and one entry in [`THEMES`]. See AGENTS.md, "Themes and assets".

pub mod illustrations;

use coffer_domain::identity::ColorScheme;

use crate::assets::{self, Asset};
use illustrations::{Illustrations, NUGGET};

/// A registered theme. Themes are told apart by `name`.
#[derive(Debug)]
pub struct Theme {
    /// The stored identifier, a valid `ThemeName`.
    pub name: &'static str,
    /// The name shown in the theme picker.
    pub label: &'static str,
    /// The token stylesheet, linked before the component stylesheet.
    pub stylesheet: Asset,
    pub illustrations: &'static Illustrations,
}

/// Soft and friendly: pistachio paper, raspberry accent, round shapes.
const MOCHI: Theme = Theme {
    name: "mochi",
    label: "Mochi",
    stylesheet: assets::THEMES_MOCHI_CSS,
    illustrations: &NUGGET,
};

/// Every theme a household or member may choose.
pub const THEMES: &[Theme] = &[MOCHI];

/// The theme used when none is chosen or the chosen one is not registered.
pub const DEFAULT: &Theme = &THEMES[0];

/// The registered theme called `name`.
#[must_use]
pub fn find(name: &str) -> Option<&'static Theme> {
    THEMES.iter().find(|theme| theme.name == name)
}

/// How a page is drawn: a theme and a colour scheme.
#[derive(Debug, Clone, Copy)]
pub struct Look {
    pub theme: &'static Theme,
    pub scheme: ColorScheme,
}

impl Default for Look {
    fn default() -> Self {
        Self {
            theme: DEFAULT,
            scheme: ColorScheme::System,
        }
    }
}

#[cfg(test)]
mod contract;
#[cfg(test)]
mod tests;
