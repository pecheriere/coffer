//! What may be in a report's log event. Each field is either one of a fixed
//! set of values, a small number, or the one bit of client text, which
//! [`clean_message`] cleans and cuts.

use serde::Deserialize;
use url::Url;

use crate::assets;
use crate::sync::provider::is_format_char;

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

const LINK_NEW: &str = "/connections/plaid/new";
const LINK_RECONNECT: &str = "/connections/{id}/plaid/reconnect";

/// The pages that run coffer's scripts, by route. A page is logged only as
/// one of these, so a link's token or an id in its path never reaches the log.
///
/// **A message is logged only from a page listed here, so list only pages that
/// show no financial data.** A page showing amounts or names, such as the
/// coming forecast page, stays out, so that a report from it says only that
/// something failed.
const PAGES: [&str; 10] = [
    "/login",
    "/setup/{token}",
    "/invite/{token}",
    "/account",
    "/connections",
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
    #[serde(default)]
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
    pub(super) fn into_report(self, public_url: &Url) -> Report {
        let page = page_template(public_url, &self.page);
        Report {
            kind: self.kind,
            message: if page == OTHER {
                WITHHELD.to_owned()
            } else {
                clean_message(&self.message)
            },
            script: script_path(public_url, &self.script),
            line: self.line,
            column: self.column,
            page,
        }
    }
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
/// - control characters and Unicode format characters go, so nothing can start
///   a new line, reach a terminal or hide what a reader sees;
/// - a word that looks like an address becomes [`URL_WORD`], since a link's
///   token could be in it;
/// - the result is cut to [`MESSAGE_CHARS`] characters.
fn clean_message(raw: &str) -> String {
    let words: Vec<String> = raw
        .split_whitespace()
        .map(|word| {
            word.chars()
                .filter(|&c| !c.is_control() && !is_format_char(c))
                .collect::<String>()
        })
        .map(|word| {
            if looks_like_address(&word) {
                URL_WORD.to_owned()
            } else {
                word
            }
        })
        .collect();
    words.join(" ").chars().take(MESSAGE_CHARS).collect()
}

fn looks_like_address(word: &str) -> bool {
    word.contains('/')
        || word.to_ascii_lowercase().contains("%2f")
        || (word.contains('?') && word.contains('='))
}

#[cfg(test)]
mod tests {
    use proptest::prelude::{ProptestConfig, any, proptest};
    use url::Url;

    use super::{MESSAGE_CHARS, clean_message, is_format_char, matches_template, page_template};

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

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        /// Whatever is sent, nothing that can break a log line is left.
        #[test]
        fn a_cleaned_message_is_always_safe(raw in any::<String>()) {
            let cleaned = clean_message(&raw);
            assert!(cleaned.chars().count() <= MESSAGE_CHARS);
            assert!(cleaned.chars().all(|c| !c.is_control() && !is_format_char(c)));
            assert!(cleaned.chars().all(|c| c == ' ' || !c.is_whitespace()));
        }
    }
}
