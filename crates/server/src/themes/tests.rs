use std::collections::BTreeSet;

use coffer_domain::identity::ThemeName;

use super::contract::{
    COLOUR_TOKENS, NEGATIVE_ACCENT_MIN_DISTANCE, OTHER_TOKENS, PAIRS, Rgb, Scheme, Tokens,
    contrast_ratio, oklab_distance, parse_theme,
};
use super::{DEFAULT, THEMES, Theme};
use crate::assets::ALL;

const SCHEMES: [Scheme; 2] = [Scheme::Light, Scheme::Dark];
const LICENSED_UNDER: &str =
    "This Font Software is licensed under the SIL Open Font License, Version 1.1.";

fn rgb(hex: &str) -> Rgb {
    Rgb::parse(hex).expect("a #RRGGBB colour")
}

fn tokens_of(theme: &Theme) -> Tokens {
    let css = std::str::from_utf8(theme.stylesheet.body).expect("UTF-8 stylesheet");
    parse_theme(css).unwrap_or_else(|err| panic!("{}: {err}", theme.name))
}

/// `#RRGGBB`, `rgba(r, g, b, a)` or `transparent`.
fn is_colour(value: &str) -> bool {
    if value == "transparent" || Rgb::parse(value).is_some() {
        return true;
    }
    let Some(args) = value
        .strip_prefix("rgba(")
        .and_then(|rest| rest.strip_suffix(')'))
    else {
        return false;
    };
    let parts: Vec<&str> = args.split(',').map(str::trim).collect();
    parts.len() == 4
        && parts[..3].iter().all(|part| part.parse::<u8>().is_ok())
        && parts[3]
            .parse::<f64>()
            .is_ok_and(|alpha| (0.0..=1.0).contains(&alpha))
}

/// Every declared pair below its minimum contrast, as `theme scheme fg/bg = ratio`.
fn wcag_failures(name: &str, tokens: &Tokens) -> Vec<String> {
    let mut failures = Vec::new();
    for scheme in SCHEMES {
        for (fg, bg, min) in PAIRS {
            let pair = (tokens.resolve(fg, scheme), tokens.resolve(bg, scheme));
            let (Ok(fg_value), Ok(bg_value)) = pair else {
                failures.push(format!("{name} {scheme:?} {fg}/{bg} = unresolved"));
                continue;
            };
            let (Some(fg_rgb), Some(bg_rgb)) = (Rgb::parse(&fg_value), Rgb::parse(&bg_value))
            else {
                failures.push(format!(
                    "{name} {scheme:?} {fg}/{bg} = not #RRGGBB ({fg_value} on {bg_value})"
                ));
                continue;
            };
            let ratio = contrast_ratio(fg_rgb, bg_rgb);
            if ratio < min.ratio() {
                failures.push(format!("{name} {scheme:?} {fg}/{bg} = {ratio:.2}"));
            }
        }
    }
    failures
}

fn has_extension(url: &str, extension: &str) -> bool {
    std::path::Path::new(url)
        .extension()
        .is_some_and(|found| found.eq_ignore_ascii_case(extension))
}

/// The lines before the OFL's "licensed under" line, or `None` without it.
fn copyright_header(licence: &str) -> Option<Vec<&str>> {
    let mut header = Vec::new();
    for line in licence.lines() {
        if line.trim() == LICENSED_UNDER {
            return Some(header);
        }
        header.push(line);
    }
    None
}

#[test]
fn contrast_maths_matches_wcag_references() {
    let white = rgb("#FFFFFF");
    assert!((contrast_ratio(rgb("#000000"), white) - 21.0).abs() < 0.01);
    assert!((contrast_ratio(rgb("#767676"), white) - 4.54).abs() < 0.01);
    assert!((contrast_ratio(white, rgb("#767676")) - 4.54).abs() < 0.01);
    assert!((contrast_ratio(rgb("#C2386A"), rgb("#C2386A")) - 1.0).abs() < 1e-9);
}

#[test]
fn oklab_matches_the_reference() {
    assert!((oklab_distance(rgb("#FFFFFF"), rgb("#000000")) - 1.0).abs() < 1e-3);
    let red = rgb("#FF0000").oklab();
    for (got, want) in red.iter().zip([0.62796, 0.22486, 0.12585]) {
        assert!((got - want).abs() < 1e-3, "{red:?}");
    }
}

#[test]
fn resolver_follows_var_and_light_dark() {
    let tokens = parse_theme(
        ":root{--a: light-dark(#111111, #EEEEEE); --b: var(--a); --c: light-dark(rgba(0, 0, 0, .5), #000000)}",
    )
    .expect("a valid theme");
    assert_eq!(tokens.resolve("b", Scheme::Light).as_deref(), Ok("#111111"));
    assert_eq!(tokens.resolve("b", Scheme::Dark).as_deref(), Ok("#EEEEEE"));
    assert_eq!(
        tokens.resolve("c", Scheme::Light).as_deref(),
        Ok("rgba(0, 0, 0, .5)")
    );
    assert_eq!(tokens.resolve("c", Scheme::Dark).as_deref(), Ok("#000000"));
    assert_eq!(tokens.references(), vec!["a".to_owned()]);
    assert!(tokens.resolve("missing", Scheme::Light).is_err());

    let cycle = parse_theme(":root{--x: var(--y); --y: var(--x)}").expect("parses");
    assert!(cycle.resolve("x", Scheme::Light).is_err());
}

#[test]
fn a_theme_file_holds_only_tokens_and_fonts() {
    for bad in [
        ":root{--a:#000000} .btn{color:red}",
        ":root{--a:#000000} :root{--b:#000000}",
        ":root{--a:#000000; --a:#FFFFFF}",
        ":root{--a:#000000",
        ":root{--a:#000000}}",
        ":root{color:red}",
        "/* unterminated :root{--a:#000000}",
    ] {
        assert!(parse_theme(bad).is_err(), "{bad}");
    }
    let tokens = parse_theme(
        "/* a { } */ @font-face { font-family: \"X\"; src: url(\"/x.woff2\"); } :root { --a: #000000; }",
    )
    .expect("fonts and one :root are a theme");
    assert_eq!(tokens.resolve("a", Scheme::Dark).as_deref(), Ok("#000000"));
}

#[test]
fn every_theme_defines_every_token_in_both_schemes() {
    for theme in THEMES {
        let tokens = tokens_of(theme);
        for scheme in SCHEMES {
            for token in COLOUR_TOKENS {
                let value = tokens
                    .resolve(token, scheme)
                    .unwrap_or_else(|err| panic!("{} {token} {scheme:?}: {err}", theme.name));
                assert!(
                    is_colour(&value),
                    "{} {token} {scheme:?}: {value:?} is not a colour",
                    theme.name
                );
            }
            for token in OTHER_TOKENS {
                let value = tokens
                    .resolve(token, scheme)
                    .unwrap_or_else(|err| panic!("{} {token} {scheme:?}: {err}", theme.name));
                assert!(
                    !value.is_empty(),
                    "{} {token} {scheme:?} is empty",
                    theme.name
                );
            }
        }
        for name in tokens.references() {
            assert!(
                tokens.resolve(&name, Scheme::Light).is_ok(),
                "{}: var(--{name}) is not defined",
                theme.name
            );
        }
    }
}

#[test]
fn every_theme_meets_wcag_aa() {
    let failures: Vec<String> = THEMES
        .iter()
        .flat_map(|theme| wcag_failures(theme.name, &tokens_of(theme)))
        .collect();
    assert!(
        failures.is_empty(),
        "below WCAG AA:\n{}",
        failures.join("\n")
    );
}

#[test]
fn wcag_check_reports_a_broken_theme() {
    let css = std::str::from_utf8(DEFAULT.stylesheet.body).expect("UTF-8 stylesheet");
    let broken = css.replace("--text: light-dark(#2A2833", "--text: light-dark(#F3F6F1");
    assert_ne!(broken, css, "the replacement applies");
    let failures = wcag_failures("broken", &parse_theme(&broken).expect("parses"));
    assert!(
        failures
            .iter()
            .any(|failure| failure.starts_with("broken Light text/bg = ")),
        "{failures:?}"
    );
}

#[test]
fn negative_amounts_stand_apart_from_the_accent() {
    for theme in THEMES {
        let tokens = tokens_of(theme);
        for scheme in SCHEMES {
            let colour = |token: &str| {
                tokens
                    .resolve(token, scheme)
                    .ok()
                    .and_then(|value| Rgb::parse(&value))
                    .unwrap_or_else(|| panic!("{} {token} {scheme:?} is not #RRGGBB", theme.name))
            };
            let distance = oklab_distance(colour("negative"), colour("accent"));
            assert!(
                distance >= NEGATIVE_ACCENT_MIN_DISTANCE,
                "{} {scheme:?}: negative is {distance:.3} from the accent",
                theme.name
            );
        }
    }
    // The mockup's terracotta negative sits too close to the raspberry accent.
    assert!(oklab_distance(rgb("#B3410C"), rgb("#C2386A")) < NEGATIVE_ACCENT_MIN_DISTANCE);
}

#[test]
fn every_theme_file_is_registered() {
    let served: BTreeSet<&str> = ALL
        .iter()
        .map(|asset| asset.url)
        .filter(|url| url.starts_with("/static/themes/"))
        .collect();
    let registered: BTreeSet<&str> = THEMES.iter().map(|theme| theme.stylesheet.url).collect();
    assert_eq!(served, registered);

    let names: BTreeSet<&str> = THEMES.iter().map(|theme| theme.name).collect();
    assert_eq!(names.len(), THEMES.len(), "theme names are unique");
    for theme in THEMES {
        assert!(ThemeName::parse(theme.name).is_ok(), "{}", theme.name);
        assert!(!theme.label.is_empty(), "{}", theme.name);
        assert_eq!(
            super::find(theme.name).map(|found| found.name),
            Some(theme.name)
        );
    }
    assert!(super::find("retired").is_none());
    assert_eq!(DEFAULT.name, "mochi");
}

#[test]
fn look_defaults_to_the_default_theme_following_the_system() {
    let look = super::Look::default();
    assert_eq!(look.theme.name, DEFAULT.name);
    assert_eq!(look.scheme, coffer_domain::identity::ColorScheme::System);
}

#[test]
fn illustrations_are_inert_svg_fragments() {
    for theme in THEMES {
        let set = theme.illustrations;
        for (role, illustration) in [
            ("welcome", &set.welcome),
            ("empty", &set.empty),
            ("error", &set.error),
            ("glyph", &set.glyph),
        ] {
            let body = illustration.body;
            assert!(!body.trim().is_empty(), "{} {role} is empty", theme.name);
            for forbidden in [
                "<script",
                "style",
                "href",
                "javascript:",
                "<foreignObject",
                "<svg",
            ] {
                assert!(
                    !body.contains(forbidden),
                    "{} {role} contains {forbidden}",
                    theme.name
                );
            }
            assert!(
                !has_event_handler(body),
                "{} {role} has an on*=",
                theme.name
            );
        }
    }
}

/// Whether `body` holds ` on<letters>=`, an inline event handler attribute.
fn has_event_handler(body: &str) -> bool {
    body.match_indices(" on").any(|(at, _)| {
        let rest = &body[at + 3..];
        let letters = rest.bytes().take_while(u8::is_ascii_alphabetic).count();
        letters > 0 && rest[letters..].trim_start().starts_with('=')
    })
}

#[test]
fn event_handler_detection_finds_inline_handlers() {
    assert!(has_event_handler(r#"<circle onload="x()"/>"#));
    assert!(has_event_handler(r#"<g onclick = "x()"/>"#));
    assert!(!has_event_handler("<text>on a hill</text>"));
}

#[test]
fn every_font_ships_with_its_licence() {
    let fonts: Vec<&str> = ALL
        .iter()
        .map(|asset| asset.url)
        .filter(|url| url.starts_with("/static/fonts/") && has_extension(url, "woff2"))
        .collect();
    assert!(!fonts.is_empty(), "the themes bundle fonts");
    for url in fonts {
        let file = &url["/static/fonts/".len()..];
        let family = file.split_once('-').map_or(file, |(family, _)| family);
        let prefix = format!("/static/fonts/ofl-{family}.");
        let licence = ALL
            .iter()
            .find(|asset| asset.url.starts_with(&prefix) && has_extension(asset.url, "txt"))
            .unwrap_or_else(|| panic!("{url} has no ofl-{family}.txt"));
        let text = std::str::from_utf8(licence.body).expect("UTF-8 licence");
        assert!(text.starts_with("Copyright"), "{}", licence.url);
        assert!(
            text.contains("SIL Open Font License, Version 1.1"),
            "{}",
            licence.url
        );
        let header = copyright_header(text)
            .unwrap_or_else(|| panic!("{}: no \"licensed under\" line", licence.url));
        assert!(
            !header
                .iter()
                .any(|line| line.contains("Reserved Font Name")),
            "{}: the copyright header declares a Reserved Font Name",
            licence.url
        );
    }
}

#[test]
fn only_the_copyright_header_declares_a_reserved_font_name() {
    let plain = format!(
        "Copyright 2016 The Example Project Authors\n\n{LICENSED_UNDER}\n\"Reserved Font Name\" refers to any names"
    );
    assert_eq!(
        copyright_header(&plain),
        Some(vec!["Copyright 2016 The Example Project Authors", ""])
    );
    let reserved = format!(
        "Copyright \u{a9} 2017 IBM Corp. with Reserved Font Name \"Plex\"\n\n{LICENSED_UNDER}"
    );
    let header = copyright_header(&reserved).expect("has a header");
    assert!(
        header
            .iter()
            .any(|line| line.contains("Reserved Font Name"))
    );
    assert_eq!(
        copyright_header("Copyright 2016 Someone\nOther licence."),
        None
    );
}
