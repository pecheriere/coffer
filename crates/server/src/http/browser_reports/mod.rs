//! The one endpoint browsers report errors to: script errors, failed passkey
//! ceremonies and CSP violations become log events, with nothing stored and
//! nothing sent anywhere else.
//!
//! It has to work before sign-in, so anyone on the network can call it. Each
//! request therefore goes through, in this order: an exact content type, the
//! origin rule below, the declared size, the real size, the parser, and only
//! then the [`limiter`]. So a refused request spends nothing of the limits, and
//! neither another site's page nor junk can silence a real browser; the size
//! limit bounds what that costs. A report kept writes one event, whose fields
//! [`fields`] allow-lists.
//!
//! The answer is always the same empty 204, so its content says nothing, not
//! even whether the report was kept. Its timing can: a request refused before
//! the body is read is answered at once, so on a direct, unbuffered connection
//! a client that sends only part of a body can tell which it was. That reveals
//! only the rules this public source already states.
//!
//! This route is exempt from the router's origin rule, because a browser may
//! send a CSP report with no `Origin`, and applies its own:
//! - A script report must carry the public origin.
//! - A CSP report may carry that, `null` or none, but must be about a document
//!   of the public origin.
//!
//! No anti-forgery token is needed: the route uses no session and changes
//! nothing. A cross-site script cannot post either content type without a
//! preflight, which this route refuses, and another site's `report-uri` names
//! its own document. A program on the network can forge all of this, and is
//! what the limits and allow-lists are for.

mod fields;
mod limiter;

use std::net::IpAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::to_bytes;
use axum::extract::Request;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::routing::post;
use coffer_domain::time::Clock;
use jiff::Timestamp;
use url::Url;

use self::fields::{CspEnvelope, Report, ScriptReport};
use self::limiter::Limiter;
use super::AppState;

/// Where reports go. The policies' `report-uri` and `coffer.js` name it too.
pub(super) const PATH: &str = "/browser-reports";

/// The largest body read: a report's allow-listed fields, or a browser's CSP
/// report, fit well within it.
const BODY_LIMIT: usize = 8 * 1024;

/// The route, with limits of its own: they last as long as the router.
pub(super) fn routes(state: &AppState) -> Router<AppState> {
    let intake = Arc::new(Intake {
        clock: Arc::clone(&state.clock),
        public_url: state.config.public_url.clone(),
        limiter: Limiter::default(),
    });
    Router::new().route(
        PATH,
        post(move |request: Request| {
            let intake = Arc::clone(&intake);
            async move {
                intake.receive(request).await;
                StatusCode::NO_CONTENT
            }
        }),
    )
}

/// The two kinds of body, each with one exact content type.
#[derive(Clone, Copy)]
enum Format {
    Script,
    Csp,
}

impl Format {
    fn of(headers: &HeaderMap) -> Option<Self> {
        match headers.get(header::CONTENT_TYPE)?.as_bytes() {
            b"application/json" => Some(Self::Script),
            b"application/csp-report" => Some(Self::Csp),
            _ => None,
        }
    }
}

struct Intake {
    clock: Arc<dyn Clock>,
    public_url: Url,
    limiter: Limiter,
}

impl Intake {
    async fn receive(&self, request: Request) {
        let now = self.clock.now();
        if let Some(dropped) = self.limiter.due_summary(now) {
            tracing::warn!(
                source = "browser",
                over_limit = dropped.over_limit,
                refused = dropped.refused,
                "browser reports dropped"
            );
        }
        match self.read(request, now).await {
            Outcome::Log(report) => report.log(),
            Outcome::Expected | Outcome::OverLimit => {}
            Outcome::Refused => self.limiter.note_refused(now),
        }
    }

    async fn read(&self, request: Request, now: Timestamp) -> Outcome {
        let (parts, body) = request.into_parts();
        let Some(format) = Format::of(&parts.headers) else {
            return Outcome::Refused;
        };
        if !self.origin_allowed(format, &parts.headers) || declares_too_much(&parts.headers) {
            return Outcome::Refused;
        }
        let Ok(bytes) = to_bytes(body, BODY_LIMIT).await else {
            return Outcome::Refused;
        };
        let report = match format {
            Format::Script => serde_json::from_slice::<ScriptReport>(&bytes)
                .ok()
                .map(|report| report.into_report(&self.public_url)),
            Format::Csp => serde_json::from_slice::<CspEnvelope>(&bytes)
                .ok()
                .and_then(|envelope| envelope.into_report(&self.public_url)),
        };
        let Some(report) = report else {
            return Outcome::Refused;
        };
        if report.is_expected() {
            return Outcome::Expected;
        }
        if self.limiter.admit(client_of(&parts.headers), now) {
            Outcome::Log(report)
        } else {
            Outcome::OverLimit
        }
    }

    fn origin_allowed(&self, format: Format, headers: &HeaderMap) -> bool {
        let origin = headers.get(header::ORIGIN).map(HeaderValue::as_bytes);
        let public = self.public_url.origin().ascii_serialization();
        let from_public = origin == Some(public.as_bytes());
        match format {
            Format::Script => from_public,
            Format::Csp => from_public || origin.is_none() || origin == Some(b"null"),
        }
    }
}

/// What became of a request.
enum Outcome {
    Log(Report),
    /// The violation Plaid's script always causes: not worth a line or a count.
    Expected,
    OverLimit,
    Refused,
}

fn declares_too_much(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| usize::try_from(length).map_or(true, |length| length > BODY_LIMIT))
}

/// Who a request counts against: the last `X-Forwarded-For` entry, which the
/// reverse proxy adds and a client cannot choose. Without a usable one, all
/// such requests share one limit. With no proxy in front a client could choose
/// it, which is why the limiter has a global bound as well.
fn client_of(headers: &HeaderMap) -> Option<IpAddr> {
    headers
        .get_all("x-forwarded-for")
        .iter()
        .next_back()?
        .to_str()
        .ok()?
        .rsplit(',')
        .next()?
        .trim()
        .parse()
        .ok()
}
