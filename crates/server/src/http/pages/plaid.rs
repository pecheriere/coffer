//! The Plaid Link pages (spec §8.6): connecting a new Item and logging an
//! existing one in again (update mode), and the endpoints their script calls.
//! They exist only on a server configured for Plaid.
//!
//! These are the only pages that load a third-party script, so only they have
//! a policy of their own, with a nonce minted for each response and set on the
//! successful response alone: a failure shows a plain error page under the
//! default policy. The link token goes
//! into the page and into no log; HTML is never stored by the browser's
//! caches ([`super::security_headers`]). A public token is passed on to Plaid
//! by the sync service and never comes back: its type hides it, and errors
//! never quote what was sent.

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use coffer_domain::accounts::ConnectionLabel;
use coffer_domain::ids::ConnectionId;
use maud::{Markup, html};
use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;
use serde_json::{Value, json};

use super::account::{json_session, viewer};
use super::layout::{Frame, Nav, Script, page_with_scripts};
use super::{ApiJson, PageError, PageSession};
use crate::assets;
use crate::config::PlaidEnvironment;
use crate::http::AppState;
use crate::http::problem::{Problem, ProblemCode};
use crate::secrets::mint_nonce;
use crate::sync::SyncError;
use crate::sync::provider::{ConnectInput, ProviderKind};

/// Plaid's script. It may not be bundled or pinned: Plaid serves it from this
/// one URL and updates it in place.
const LINK_SCRIPT: &str = "https://cdn.plaid.com/link/v2/stable/link-initialize.js";

const EXCHANGE_PATH: &str = "/connections/plaid/exchange";
const AFTER_LINK: &str = "/connections";

const LABEL_INVALID: &str = "The label must be 1 to 100 characters without control characters.";
const TOKEN_MISSING: &str = "Plaid Link sent no public token.";

/// The Link pages' policy for `environment`: the only policy a handler may
/// set. It is the default policy plus exactly what Link needs from Plaid. The
/// nonce admits the stylesheet Plaid's script adds, which it marks with the
/// nonce of its own tag; without it Link has no layout on a phone.
fn plaid_link_csp(environment: PlaidEnvironment, nonce: &str) -> String {
    let host = environment.host();
    format!(
        "default-src 'self'; script-src 'self' {LINK_SCRIPT}; \
         frame-src https://cdn.plaid.com; connect-src 'self' https://{host}; \
         style-src 'self' 'nonce-{nonce}'; img-src 'self'; frame-ancestors 'none'; \
         form-action 'self'; base-uri 'none'"
    )
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/connections/plaid/new", get(new_page))
        .route(EXCHANGE_PATH, post(exchange))
        .route("/connections/{id}/plaid/reconnect", get(reconnect_page))
        .route("/connections/{id}/plaid/reconnected", post(reconnected))
}

/// What a Link page is for.
enum Link {
    /// A new Item: the member names the connection.
    New,
    /// Update mode for this connection.
    Update(ConnectionId),
}

async fn new_page(
    State(state): State<AppState>,
    session: PageSession,
) -> Result<Response, PageError> {
    let token = state
        .sync
        .new_link_token(&session.actor())
        .await
        .map_err(page_error)?;
    render(&state, &session, &token, &Link::New).await
}

async fn reconnect_page(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
) -> Result<Response, PageError> {
    let id: ConnectionId = id.parse().map_err(|_| PageError::not_found())?;
    let token = state
        .sync
        .update_link_token(&session.actor(), id)
        .await
        .map_err(page_error)?;
    render(&state, &session, &token, &Link::Update(id)).await
}

async fn render(
    state: &AppState,
    session: &PageSession,
    token: &SecretString,
    link: &Link,
) -> Result<Response, PageError> {
    let Some(plaid) = &state.config.plaid else {
        return Err(PageError::not_found());
    };
    let viewer = viewer(state, session);
    let (title, submit) = match link {
        Link::New => ("Connect with Plaid", EXCHANGE_PATH.to_owned()),
        Link::Update(id) => (
            "Reconnect with Plaid",
            format!("/connections/{id}/plaid/reconnected"),
        ),
    };
    let content = link_content(title, &submit, token, &viewer.csrf, link);
    let frame = Frame {
        look: session.look(state).await,
        viewer: Some(&viewer),
        current: Some(Nav::Connections),
        narrow: false,
    };
    let nonce = mint_nonce();
    let markup = page_with_scripts(
        &frame,
        title,
        &content,
        &[
            Script {
                src: LINK_SCRIPT,
                nonce: Some(&nonce),
            },
            Script {
                src: assets::JS_PLAID_LINK_JS.url,
                nonce: None,
            },
        ],
    );
    let policy = HeaderValue::from_str(&plaid_link_csp(plaid.environment, &nonce))
        .map_err(|_| PageError(internal()))?;
    Ok(([(header::CONTENT_SECURITY_POLICY, policy)], markup).into_response())
}

fn link_content(
    title: &str,
    submit: &str,
    token: &SecretString,
    csrf: &str,
    link: &Link,
) -> Markup {
    html! {
        h1 { (title) }
        section.card {
            @match link {
                Link::New => {
                    p { "Log in to your bank through Plaid. coffer keeps only the access it grants, encrypted." }
                    label.field {
                        "Label"
                        input #plaid-label .input type="text" maxlength="100" required;
                    }
                }
                Link::Update(_) => {
                    p { "Log in to your bank through Plaid again. The connection's accounts stay bound." }
                }
            }
            p #result role="status" {}
            button #plaid-link .btn.btn-primary type="button"
                data-link-token=(token.expose_secret()) data-csrf=(csrf) data-submit=(submit) {
                (title)
            }
        }
    }
}

/// Unknown fields are refused: a forged request fails whole.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExchangeBody {
    label: String,
    public_token: SecretString,
}

/// The session and the anti-forgery header come first: nothing about the body
/// is answered without them.
async fn exchange(
    State(state): State<AppState>,
    session: Result<PageSession, PageError>,
    headers: HeaderMap,
    body: Result<ApiJson<ExchangeBody>, Problem>,
) -> Result<Json<Value>, Problem> {
    let actor = json_session(session, &state, &headers)?;
    let ApiJson(body) = body?;
    let Ok(label) = ConnectionLabel::parse(body.label.trim()) else {
        return Err(invalid(LABEL_INVALID));
    };
    if body.public_token.expose_secret().trim().is_empty() {
        return Err(invalid(TOKEN_MISSING));
    }
    state
        .sync
        .create_connection(
            &actor,
            ProviderKind::Plaid,
            label,
            ConnectInput::PlaidPublicToken(body.public_token),
            None,
        )
        .await?;
    Ok(done())
}

/// The page's script reports that update mode finished; it sends no token,
/// because update mode leaves the access token as it is.
async fn reconnected(
    State(state): State<AppState>,
    session: Result<PageSession, PageError>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, Problem> {
    let actor = json_session(session, &state, &headers)?;
    let id: ConnectionId = id.parse().map_err(|_| Problem::not_found())?;
    state.sync.confirm_reauthorized(&actor, id).await?;
    Ok(done())
}

fn done() -> Json<Value> {
    Json(json!({ "redirect": AFTER_LINK }))
}

fn invalid(detail: &str) -> Problem {
    Problem::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        ProblemCode::Invalid,
        "Invalid value",
    )
    .with_detail(detail.to_owned())
}

fn internal() -> Problem {
    Problem::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        ProblemCode::Internal,
        "Internal error",
    )
}

fn page_error(err: SyncError) -> PageError {
    PageError(Problem::from(err))
}

#[cfg(test)]
mod tests {
    use super::{PlaidEnvironment, plaid_link_csp};
    use crate::secrets::mint_nonce;

    #[test]
    fn link_csp_allows_only_the_environment_host() {
        let nonce = mint_nonce();
        let sandbox = plaid_link_csp(PlaidEnvironment::Sandbox, &nonce);
        let production = plaid_link_csp(PlaidEnvironment::Production, &nonce);
        assert_eq!(
            production,
            format!(
                "default-src 'self'; \
                 script-src 'self' https://cdn.plaid.com/link/v2/stable/link-initialize.js; \
                 frame-src https://cdn.plaid.com; \
                 connect-src 'self' https://production.plaid.com; \
                 style-src 'self' 'nonce-{nonce}'; img-src 'self'; frame-ancestors 'none'; \
                 form-action 'self'; base-uri 'none'"
            )
        );
        assert!(!production.contains("sandbox.plaid.com"));
        assert!(!sandbox.contains("production.plaid.com"));
        for policy in [&sandbox, &production] {
            assert!(!policy.contains("unsafe"), "never an unsafe source");
            assert!(
                !policy.contains("style-src-elem") && !policy.contains("style-src-attr"),
                "no narrower style directive"
            );
            assert_eq!(
                policy.matches("nonce-").count(),
                1,
                "the nonce is for styles"
            );
        }
    }
}
