//! The theme contract, checked by the tests: the tokens every theme defines,
//! the colour pairs that must meet WCAG AA, and a small reader for theme files
//! (`@font-face` rules and one `:root` block of custom properties).

use std::collections::BTreeMap;

/// Which arm of `light-dark(<light>, <dark>)` applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scheme {
    Light,
    Dark,
}

/// A theme's tokens: name without `--` mapped to its raw value.
#[derive(Debug)]
pub(crate) struct Tokens(BTreeMap<String, String>);

/// Tokens that must resolve to a colour (`#RRGGBB`, `rgba(…)` or
/// `transparent`) in both schemes.
pub(crate) const COLOUR_TOKENS: &[&str] = &[
    "bg",
    "surface",
    "text",
    "muted",
    "accent",
    "on-accent",
    "accent-soft",
    "positive",
    "negative",
    "warning",
    "warn-soft",
    "border",
    "control-border",
    "shadow",
    "card-bg",
    "card-border-color",
    "input-bg",
    "input-border-color",
    "button-primary-bg",
    "button-primary-fg",
    "button-secondary-bg",
    "button-secondary-fg",
    "button-secondary-border",
    "nav-bg",
    "nav-border-color",
    "nav-active-bg",
    "nav-active-fg",
];

/// Tokens that must be present with a non-empty value.
pub(crate) const OTHER_TOKENS: &[&str] = &[
    "font-head",
    "font-body",
    "font-num",
    "font-mono",
    "weight-head",
    "weight-button",
    "tracking-head",
    "style-display",
    "radius",
    "radius-sm",
    "radius-btn",
    "radius-chip",
    "icon-stroke",
    "card-border-width",
    "card-shadow",
    "input-border-width",
    "button-primary-shadow",
    "nav-active-shadow",
    "illustration-animation",
];

/// The WCAG AA minimum contrast a pair must reach.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Min {
    /// Text, at 4.5:1 (large text is held to it too).
    Body,
    /// UI boundaries, at 3:1.
    Ui,
}

impl Min {
    pub(crate) fn ratio(self) -> f64 {
        match self {
            Min::Body => 4.5,
            Min::Ui => 3.0,
        }
    }
}

/// Every (foreground, background) pair the components draw.
pub(crate) const PAIRS: &[(&str, &str, Min)] = &[
    ("text", "bg", Min::Body),
    ("text", "surface", Min::Body),
    ("muted", "bg", Min::Body),
    ("muted", "surface", Min::Body),
    ("accent", "bg", Min::Body),
    ("accent", "surface", Min::Body),
    ("on-accent", "accent", Min::Body),
    ("positive", "bg", Min::Body),
    ("positive", "surface", Min::Body),
    ("positive", "card-bg", Min::Body),
    ("negative", "bg", Min::Body),
    ("negative", "surface", Min::Body),
    ("warning", "surface", Min::Body),
    ("warning", "warn-soft", Min::Body),
    ("text", "warn-soft", Min::Body),
    ("text", "accent-soft", Min::Body),
    ("muted", "accent-soft", Min::Body),
    ("text", "card-bg", Min::Body),
    ("muted", "card-bg", Min::Body),
    ("accent", "card-bg", Min::Body),
    ("text", "input-bg", Min::Body),
    ("button-primary-fg", "button-primary-bg", Min::Body),
    ("button-secondary-fg", "button-secondary-bg", Min::Body),
    ("text", "nav-bg", Min::Body),
    ("muted", "nav-bg", Min::Body),
    ("nav-active-fg", "nav-active-bg", Min::Body),
    ("control-border", "surface", Min::Ui),
    ("control-border", "bg", Min::Ui),
    ("input-border-color", "card-bg", Min::Ui),
    ("accent", "nav-bg", Min::Ui),
];

/// How far apart, in Oklab, negative amounts and the accent must sit.
pub(crate) const NEGATIVE_ACCENT_MIN_DISTANCE: f64 = 0.15;

/// Reads a theme file.
///
/// # Errors
/// On an unterminated comment, unbalanced braces, a top-level rule other than
/// `:root` and `@font-face`, a second `:root`, a missing `:root`, a
/// declaration in `:root` that is not a custom property, or a duplicate token.
pub(crate) fn parse_theme(css: &str) -> Result<Tokens, String> {
    let css = strip_comments(css)?;
    let mut tokens = None;
    let mut rest = css.as_str();
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        let open = rest
            .find('{')
            .ok_or_else(|| format!("text outside any rule: {:?}", rest.trim()))?;
        let close = matching_brace(rest, open)?;
        let body = &rest[open + 1..close];
        match rest[..open].trim() {
            ":root" if tokens.is_some() => return Err("a second :root block".to_owned()),
            ":root" => tokens = Some(parse_declarations(body)?),
            "@font-face" => {}
            other => {
                return Err(format!(
                    "a theme holds only :root and @font-face, found {other:?}"
                ));
            }
        }
        rest = &rest[close + 1..];
    }
    tokens
        .map(Tokens)
        .ok_or_else(|| "no :root block".to_owned())
}

impl Tokens {
    /// The value of `name` in `scheme`, with every `var()` substituted and
    /// every `light-dark()` reduced to the matching arm.
    ///
    /// # Errors
    /// When a token is undefined, a reference is cyclic or a function is
    /// malformed.
    pub(crate) fn resolve(&self, name: &str, scheme: Scheme) -> Result<String, String> {
        self.resolve_within(name, scheme, &mut Vec::new())
    }

    /// Every token named in a `var()`, sorted and without repeats.
    pub(crate) fn references(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .0
            .values()
            .flat_map(|value| {
                value.match_indices("var(--").map(|(at, call)| {
                    value[at + call.len()..]
                        .chars()
                        .take_while(|c| !matches!(c, ',' | ')') && !c.is_whitespace())
                        .collect()
                })
            })
            .collect();
        names.sort();
        names.dedup();
        names
    }

    fn resolve_within(
        &self,
        name: &str,
        scheme: Scheme,
        stack: &mut Vec<String>,
    ) -> Result<String, String> {
        if stack.iter().any(|seen| seen == name) {
            return Err(format!(
                "cyclic reference: {} -> {name}",
                stack.join(" -> ")
            ));
        }
        let raw = self
            .0
            .get(name)
            .ok_or_else(|| format!("--{name} is not defined"))?;
        stack.push(name.to_owned());
        let value = self.expand(raw, scheme, stack);
        stack.pop();
        value
    }

    /// Substitutes `var()` and `light-dark()` calls in `value`, left to right.
    fn expand(
        &self,
        value: &str,
        scheme: Scheme,
        stack: &mut Vec<String>,
    ) -> Result<String, String> {
        let mut out = String::new();
        let mut rest = value;
        loop {
            let next = ["var(", "light-dark("]
                .into_iter()
                .filter_map(|call| rest.find(call).map(|at| (at, call)))
                .min();
            let Some((at, call)) = next else {
                out.push_str(rest);
                break;
            };
            out.push_str(&rest[..at]);
            let open = at + call.len() - 1;
            let close = matching_paren(rest, open)?;
            let args = split_top_level(&rest[open + 1..close], ',');
            let replacement = match (call, args.as_slice()) {
                ("var(", [token]) => {
                    let token = token.trim();
                    let token = token
                        .strip_prefix("--")
                        .ok_or_else(|| format!("var({token}) does not name a token"))?;
                    self.resolve_within(token, scheme, stack)?
                }
                ("light-dark(", [light, dark]) => {
                    let arm = match scheme {
                        Scheme::Light => light,
                        Scheme::Dark => dark,
                    };
                    self.expand(arm.trim(), scheme, stack)?
                }
                _ => return Err(format!("unsupported call: {}", &rest[at..=close])),
            };
            out.push_str(&replacement);
            rest = &rest[close + 1..];
        }
        Ok(out.trim().to_owned())
    }
}

fn strip_comments(css: &str) -> Result<String, String> {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        let end = rest[start + 2..]
            .find("*/")
            .ok_or_else(|| "unterminated comment".to_owned())?;
        out.push(' ');
        rest = &rest[start + 2 + end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// The index of the delimiter closing the one at `open`, skipping strings.
fn matching(text: &str, open: usize, opening: u8, closing: u8) -> Result<usize, String> {
    let mut depth = 0_usize;
    let mut quote = None;
    for (index, byte) in text.bytes().enumerate().skip(open) {
        if let Some(q) = quote {
            if byte == q {
                quote = None;
            }
            continue;
        }
        match byte {
            b'"' | b'\'' => quote = Some(byte),
            b if b == opening => depth += 1,
            b if b == closing => {
                depth -= 1;
                if depth == 0 {
                    return Ok(index);
                }
            }
            _ => {}
        }
    }
    Err(format!("unbalanced {}", char::from(opening)))
}

fn matching_brace(text: &str, open: usize) -> Result<usize, String> {
    matching(text, open, b'{', b'}')
}

fn matching_paren(text: &str, open: usize) -> Result<usize, String> {
    matching(text, open, b'(', b')')
}

/// Splits at `separator` outside parentheses and strings.
fn split_top_level(text: &str, separator: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0_usize;
    let mut quote = None;
    let mut start = 0;
    for (index, c) in text.char_indices() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            c if c == separator && depth == 0 => {
                parts.push(&text[start..index]);
                start = index + c.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

fn parse_declarations(body: &str) -> Result<BTreeMap<String, String>, String> {
    if body.contains(['{', '}']) {
        return Err("a nested block in :root".to_owned());
    }
    let mut tokens = BTreeMap::new();
    for declaration in split_top_level(body, ';') {
        let declaration = declaration.trim();
        if declaration.is_empty() {
            continue;
        }
        let (property, value) = declaration
            .split_once(':')
            .ok_or_else(|| format!("not a declaration: {declaration:?}"))?;
        let name = property
            .trim()
            .strip_prefix("--")
            .ok_or_else(|| format!(":root holds only tokens, found {:?}", property.trim()))?;
        if tokens
            .insert(name.to_owned(), value.trim().to_owned())
            .is_some()
        {
            return Err(format!("--{name} is defined twice"));
        }
    }
    Ok(tokens)
}

/// An opaque sRGB colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Rgb([u8; 3]);

impl Rgb {
    /// Reads `#RRGGBB` (either case).
    pub(crate) fn parse(text: &str) -> Option<Rgb> {
        let hex = text.strip_prefix('#')?;
        if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let channel = |at: usize| u8::from_str_radix(&hex[at..at + 2], 16).ok();
        Some(Rgb([channel(0)?, channel(2)?, channel(4)?]))
    }

    /// Linear-light channels, with the sRGB threshold 0.04045.
    fn linear(self) -> [f64; 3] {
        self.0.map(|channel| {
            let c = f64::from(channel) / 255.0;
            if c <= 0.040_45 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        })
    }

    /// WCAG 2.x relative luminance.
    fn luminance(self) -> f64 {
        let [r, g, b] = self.linear();
        0.2126 * r + 0.7152 * g + 0.0722 * b
    }

    /// Björn Ottosson's Oklab coordinates `[L, a, b]`.
    pub(crate) fn oklab(self) -> [f64; 3] {
        let [red, green, blue] = self.linear();
        // Cone responses (long, medium, short), then their cube roots.
        let long =
            (0.412_221_470_8 * red + 0.536_332_536_3 * green + 0.051_445_992_9 * blue).cbrt();
        let medium =
            (0.211_903_498_2 * red + 0.680_699_545_1 * green + 0.107_396_956_6 * blue).cbrt();
        let short =
            (0.088_302_461_9 * red + 0.281_718_837_6 * green + 0.629_978_700_5 * blue).cbrt();
        [
            0.210_454_255_3 * long + 0.793_617_785_0 * medium - 0.004_072_046_8 * short,
            1.977_998_495_1 * long - 2.428_592_205_0 * medium + 0.450_593_709_9 * short,
            0.025_904_037_1 * long + 0.782_771_766_2 * medium - 0.808_675_766_0 * short,
        ]
    }
}

/// The WCAG 2.x contrast ratio between two colours, from 1 to 21.
pub(crate) fn contrast_ratio(a: Rgb, b: Rgb) -> f64 {
    let (la, lb) = (a.luminance(), b.luminance());
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// The Euclidean distance between two colours in Oklab.
pub(crate) fn oklab_distance(a: Rgb, b: Rgb) -> f64 {
    let (a, b) = (a.oklab(), b.oklab());
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - y).powi(2))
        .sum::<f64>()
        .sqrt()
}
