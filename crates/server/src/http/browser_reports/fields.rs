//! What may be in a report's log event. Each field is either one of a fixed
//! set of values, a small number, or the one bit of client text, which
//! [`clean_message`] cleans and cuts.

use std::fmt;

use serde::Deserialize;
use serde::de::{Deserializer, Error, Visitor};
use unicode_properties::{GeneralCategoryGroup, UnicodeGeneralCategory as _};
use url::Url;

use crate::assets;

/// The most characters logged of a client's message.
const MESSAGE_CHARS: usize = 200;

/// Stands for any page or script outside the tables below.
const OTHER: &str = "other";
/// Stands for a script a report does not name.
const NONE: &str = "none";
/// Stands for a message left out because of its page.
const WITHHELD: &str = "[withheld]";
/// Stands for a word that looks like an address, which may hold a link's token.
const URL_WORD: &str = "[url]";
/// Stands for a word shaped like a credential, which log alerts look for.
const TOKEN_WORD: &str = "[token]";

/// Letters and symbols that show nothing: the Hangul fillers and the blank
/// Braille pattern.
const BLANK_LOOKING: [char; 5] = ['\u{115f}', '\u{1160}', '\u{2800}', '\u{3164}', '\u{ffa0}'];

const CONNECTIONS: &str = "/connections";
const LINK_NEW: &str = "/connections/plaid/new";
const LINK_RECONNECT: &str = "/connections/{id}/plaid/reconnect";

/// The pages that run coffer's scripts, by route. A page is logged only as
/// one of these, so a link's token or an id in its path never reaches the log.
///
/// Some of them show names or provider accounts. Listing them is safe because
/// a message is logged only from coffer's own code (see
/// [`ScriptReport::into_report`]) and that code has no path to that data: it
/// never reads what a page shows into an error. **A page whose scripts handle
/// amounts or names breaks that, so it stays out**, as the forecast and net worth
/// pages do, and a report from it says only that something failed.
///
/// A rejection names no script, so nothing shows it is coffer's. Its text is kept, except from
/// [`CONNECTIONS`], whose forms hold a label that may be a child's name, which another script on
/// the page, a browser extension's say, could quote.
const PAGES: [&str; 10] = [
    "/login",
    "/setup/{token}",
    "/invite/{token}",
    "/account",
    CONNECTIONS,
    LINK_NEW,
    LINK_RECONNECT,
    "/admin/members",
    "/admin/invites",
    "/admin/household",
];

/// CSP directives by name. Anything else is logged as [`OTHER`].
const DIRECTIVES: [&str; 19] = [
    "default-src",
    "script-src",
    "script-src-elem",
    "script-src-attr",
    "style-src",
    "style-src-elem",
    "style-src-attr",
    "img-src",
    "font-src",
    "connect-src",
    "media-src",
    "object-src",
    "frame-src",
    "child-src",
    "worker-src",
    "manifest-src",
    "frame-ancestors",
    "form-action",
    "base-uri",
];

/// The kinds of report. Only a CSP report's body can make a [`Kind::Csp`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Kind {
    Error,
    UnhandledRejection,
    Passkey,
    Plaid,
    #[serde(skip_deserializing)]
    Csp,
}

impl Kind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::UnhandledRejection => "unhandled_rejection",
            Self::Passkey => "passkey",
            Self::Plaid => "plaid",
            Self::Csp => "csp",
        }
    }
}

/// What `coffer.js` sends. It sends exactly these fields, so any other means
/// the report is not from it: unknown fields drop the report.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ScriptReport {
    kind: Kind,
    #[serde(default, deserialize_with = "lossy_text")]
    message: String,
    #[serde(default)]
    script: String,
    #[serde(default)]
    line: u32,
    #[serde(default)]
    column: u32,
    #[serde(default)]
    page: String,
}

impl ScriptReport {
    /// The message is kept only when it is known to come from coffer's own code:
    /// from a page in [`PAGES`] and, for an error, which names its script, from
    /// one of coffer's scripts. A password manager's script, say, can throw text
    /// holding the page's content. A rejection from `/connections` is withheld too: see [`PAGES`].
    pub(super) fn into_report(self, public_url: &Url) -> Report {
        let page = page_template(public_url, &self.page);
        let script = script_path(public_url, &self.script);
        let foreign_error = self.kind == Kind::Error && (script == OTHER || script == NONE);
        let rejection_by_a_form = self.kind == Kind::UnhandledRejection && page == CONNECTIONS;
        Report {
            kind: self.kind,
            message: if page == OTHER || foreign_error || rejection_by_a_form {
                WITHHELD.to_owned()
            } else {
                clean_message(&self.message)
            },
            script,
            line: self.line,
            column: self.column,
            page,
        }
    }
}

/// A JSON string as text, whatever it holds: half a character, which JSON can
/// carry and a Rust string cannot, becomes U+FFFD rather than losing the report.
fn lossy_text<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    struct LossyText;

    impl Visitor<'_> for LossyText {
        type Value = String;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a string")
        }

        fn visit_bytes<E: Error>(self, bytes: &[u8]) -> Result<String, E> {
            Ok(String::from_utf8_lossy(bytes).into_owned())
        }
    }

    deserializer.deserialize_bytes(LossyText)
}

/// The body of a browser's `report-uri` report. Browsers add fields of their
/// own, so unknown ones are ignored. Among them are the sample of the blocked
/// code and the referrer, which are never read.
#[derive(Debug, Deserialize)]
pub(super) struct CspEnvelope {
    #[serde(rename = "csp-report")]
    report: CspReport,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
struct CspReport {
    document_uri: String,
    effective_directive: Option<String>,
    violated_directive: String,
    blocked_uri: String,
    source_file: String,
    line_number: u32,
    column_number: u32,
}

impl CspEnvelope {
    /// The report, unless it is about a document of another origin: another
    /// site's policy can name this endpoint.
    pub(super) fn into_report(self, public_url: &Url) -> Option<Report> {
        let CspReport {
            document_uri,
            effective_directive,
            violated_directive,
            blocked_uri,
            source_file,
            line_number,
            column_number,
        } = self.report;
        same_origin(public_url, &document_uri)?;
        let directive = effective_directive.unwrap_or(violated_directive);
        Some(Report {
            kind: Kind::Csp,
            message: format!(
                "{} blocked {}",
                directive_name(&directive),
                blocked_class(public_url, &blocked_uri)
            ),
            script: script_path(public_url, &source_file),
            line: line_number,
            column: column_number,
            page: page_template(public_url, &document_uri),
        })
    }
}

/// One report, ready for the log: every field already allow-listed or cleaned.
#[derive(Debug)]
pub(super) struct Report {
    kind: Kind,
    message: String,
    script: &'static str,
    line: u32,
    column: u32,
    page: &'static str,
}

impl Report {
    /// The one violation the threat model expects: Plaid's script checks at
    /// start-up whether inline styles work, once on each Link page.
    pub(super) fn is_expected(&self) -> bool {
        self.kind == Kind::Csp
            && (self.page == LINK_NEW || self.page == LINK_RECONNECT)
            && self.message == "style-src-elem blocked inline"
    }

    /// Writes the event. The text goes in as a plain `&str`, which the text
    /// format quotes and escapes and the JSON format encodes.
    pub(super) fn log(&self) {
        tracing::warn!(
            source = "browser",
            trust = "untrusted",
            kind = self.kind.as_str(),
            page = self.page,
            script = self.script,
            line = self.line,
            column = self.column,
            client_message = self.message.as_str(),
            "browser report (client-reported, untrusted)"
        );
    }
}

/// `raw` as an address of the public origin. A relative one is taken as such,
/// so a query or fragment in it never matters: only the path is used.
fn same_origin(public_url: &Url, raw: &str) -> Option<Url> {
    let url = public_url.join(raw).ok()?;
    (url.origin() == public_url.origin()).then_some(url)
}

fn page_template(public_url: &Url, raw: &str) -> &'static str {
    if raw.is_empty() {
        return OTHER;
    }
    same_origin(public_url, raw)
        .and_then(|url| {
            PAGES
                .into_iter()
                .find(|template| matches_template(template, url.path()))
        })
        .unwrap_or(OTHER)
}

/// Whether `path` is an instance of `template`, in which a `{name}` segment
/// stands for any one non-empty segment.
fn matches_template(template: &str, path: &str) -> bool {
    let mut wanted = template.split('/');
    let mut given = path.split('/');
    loop {
        match (wanted.next(), given.next()) {
            (None, None) => return true,
            (Some(want), Some(have)) => {
                let any = want.starts_with('{') && !have.is_empty();
                if !any && want != have {
                    return false;
                }
            }
            _ => return false,
        }
    }
}

/// A first-party script's own path, without what follows it. Everything else
/// is [`OTHER`], so this logs no text a client chose.
fn script_path(public_url: &Url, raw: &str) -> &'static str {
    if raw.is_empty() {
        return NONE;
    }
    same_origin(public_url, raw)
        .and_then(|url| {
            [assets::JS_COFFER_JS.url, assets::JS_PLAID_LINK_JS.url]
                .into_iter()
                .find(|script| *script == url.path())
        })
        .unwrap_or(OTHER)
}

/// A CSP directive's name; a violated directive may be followed by its value.
fn directive_name(raw: &str) -> &'static str {
    let name = raw.split_whitespace().next().unwrap_or_default();
    DIRECTIVES
        .into_iter()
        .find(|directive| *directive == name)
        .unwrap_or(OTHER)
}

/// What was blocked, as a class: the address itself may hold a token.
fn blocked_class(public_url: &Url, raw: &str) -> &'static str {
    match raw {
        "inline" => "inline",
        "eval" => "eval",
        "wasm-eval" => "wasm-eval",
        "self" => "same-origin",
        "data" => "data",
        "blob" => "blob",
        _ if raw.starts_with("data:") => "data",
        _ if raw.starts_with("blob:") => "blob",
        _ if raw.is_empty() => NONE,
        _ if same_origin(public_url, raw).is_some() && raw.contains("://") => "same-origin",
        _ => OTHER,
    }
}

/// A client's message, safe to log:
/// - every run of whitespace, line separators included, becomes one space;
/// - only letters, numbers, punctuation and symbols stay, so nothing can start a
///   new line, reach a terminal or hide text from a reader. That leaves out
///   combining marks, so some scripts lose their vowels: this is for diagnosis;
/// - a word shaped like a credential becomes [`TOKEN_WORD`], so that a report
///   cannot set off a log alert on secrets, and one that looks like an address
///   becomes [`URL_WORD`], since a link's token could be in it;
/// - the result is cut to [`MESSAGE_CHARS`] characters.
fn clean_message(raw: &str) -> String {
    let words: Vec<String> = raw
        .split_whitespace()
        .map(|word| {
            word.chars()
                .filter(|&c| is_printable(c))
                .collect::<String>()
        })
        .filter(|word| !word.is_empty())
        .map(|word| {
            if looks_like_credential(&word) {
                TOKEN_WORD.to_owned()
            } else if looks_like_address(&word) {
                URL_WORD.to_owned()
            } else {
                word
            }
        })
        .collect();
    words.join(" ").chars().take(MESSAGE_CHARS).collect()
}

fn is_printable(c: char) -> bool {
    matches!(
        c.general_category_group(),
        GeneralCategoryGroup::Letter
            | GeneralCategoryGroup::Number
            | GeneralCategoryGroup::Punctuation
            | GeneralCategoryGroup::Symbol
    ) && !BLANK_LOOKING.contains(&c)
}

/// A JSON Web Token's start and dots, or a key file's header.
fn looks_like_credential(word: &str) -> bool {
    word.contains("-----BEGIN") || (word.contains("eyJ") && word.contains('.'))
}

fn looks_like_address(word: &str) -> bool {
    word.contains('/')
        || word.to_ascii_lowercase().contains("%2f")
        || (word.contains('?') && word.contains('='))
}

#[cfg(test)]
mod tests {
    use proptest::prelude::{ProptestConfig, any, proptest};
    use unicode_properties::{GeneralCategoryGroup, UnicodeGeneralCategory as _};
    use url::Url;

    use super::{MESSAGE_CHARS, clean_message, matches_template, page_template};

    fn public_url() -> Url {
        Url::parse("https://coffer.test").unwrap()
    }

    #[test]
    fn a_template_matches_one_segment_per_placeholder() {
        assert!(matches_template("/invite/{token}", "/invite/abc"));
        assert!(!matches_template("/invite/{token}", "/invite/"));
        assert!(!matches_template("/invite/{token}", "/invite"));
        assert!(!matches_template("/invite/{token}", "/invite/abc/def"));
        assert!(!matches_template("/login", "/login/"));
    }

    #[test]
    fn a_path_that_climbs_back_is_read_as_the_browser_would() {
        assert_eq!(
            page_template(&public_url(), "/invite/abc/../../login"),
            "/login"
        );
    }

    #[test]
    fn an_empty_message_stays_empty() {
        assert_eq!(clean_message(""), "");
        assert_eq!(clean_message(" \r\n\t "), "");
    }

    /// Nothing that can break a log line or hide from a reader, in the
    /// categories themselves and not through the function under test.
    fn assert_safe(cleaned: &str) {
        assert!(cleaned.chars().count() <= MESSAGE_CHARS);
        assert!(!cleaned.contains("  ") && cleaned.trim() == cleaned);
        for c in cleaned.chars() {
            let group = c.general_category_group();
            assert!(
                c == ' '
                    || matches!(
                        group,
                        GeneralCategoryGroup::Letter
                            | GeneralCategoryGroup::Number
                            | GeneralCategoryGroup::Punctuation
                            | GeneralCategoryGroup::Symbol
                    ),
                "U+{:X}",
                u32::from(c)
            );
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn a_cleaned_message_is_always_safe(raw in any::<String>()) {
            assert_safe(&clean_message(&raw));
        }

        /// Random text rarely holds these, so they get a run of their own.
        #[test]
        fn a_message_of_invisible_characters_is_always_safe(
            raw in "[a-z \\x{e0100}-\\x{e01ef}\\x{fe00}-\\x{fe0f}\\x{300}-\\x{36f}\\x{115f}\\x{1160}\\x{3164}\\x{ffa0}\\x{2800}\\x{e000}-\\x{f8ff}\\x{202a}-\\x{202e}]{0,300}",
        ) {
            let cleaned = clean_message(&raw);
            assert_safe(&cleaned);
            assert!(cleaned.chars().all(|c| c == ' ' || c.is_ascii_lowercase()), "{cleaned}");
        }
    }
}
