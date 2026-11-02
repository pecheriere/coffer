//! Software passkeys that drive identity ceremonies without HTTP.

use std::sync::Arc;

use coffer_domain::identity::{DisplayName, TokenName, Username};
use coffer_domain::ids::MemberId;
use url::Url;
use webauthn_authenticator_rs::WebauthnAuthenticator;
use webauthn_authenticator_rs::error::WebauthnCError;
use webauthn_authenticator_rs::softpasskey::SoftPasskey;
use webauthn_rs::prelude::{
    CreationChallengeResponse, PublicKeyCredential, RegisterPublicKeyCredential,
    RequestChallengeResponse,
};

use super::TEST_PUBLIC_URL;
use crate::auth::{Identity, Sessions};
use crate::secrets::SecretToken;

/// The token at the end of a setup, invite or re-enroll link.
///
/// # Panics
///
/// If the link has no path segment.
#[must_use]
pub fn link_token(link: &Url) -> String {
    link.path_segments()
        .and_then(|mut segments| segments.next_back())
        .expect("link ends with a token")
        .to_owned()
}

fn origin() -> Url {
    Url::parse(TEST_PUBLIC_URL).expect("valid test origin")
}

/// A software authenticator bound to a test server's identity service.
pub struct SoftAuthenticator {
    inner: WebauthnAuthenticator<SoftPasskey>,
    identity: Arc<Identity>,
    sessions: Arc<Sessions>,
}

impl SoftAuthenticator {
    pub(super) fn new(
        identity: Arc<Identity>,
        sessions: Arc<Sessions>,
        verifies_user: bool,
    ) -> Self {
        Self {
            inner: WebauthnAuthenticator::new(SoftPasskey::new(verifies_user)),
            identity,
            sessions,
        }
    }

    /// Answers registration `options` as a browser would.
    ///
    /// # Errors
    ///
    /// When the authenticator declines (for example, it cannot verify the
    /// user, or it already holds an excluded credential).
    pub fn register(
        &mut self,
        options: CreationChallengeResponse,
    ) -> Result<RegisterPublicKeyCredential, WebauthnCError> {
        self.inner.do_registration(origin(), options)
    }

    /// Signs authentication `options` as a browser would.
    ///
    /// # Panics
    ///
    /// If the authenticator holds none of the allowed credentials.
    pub fn sign(&mut self, options: RequestChallengeResponse) -> PublicKeyCredential {
        self.inner
            .do_authentication(origin(), options)
            .expect("soft passkey assertion")
    }

    /// Redeems `link` with this authenticator: begin, register, finish.
    /// `username` and `display_name` are ignored for re-enroll links.
    ///
    /// # Panics
    ///
    /// If any step fails.
    pub async fn enroll(&mut self, link: &Url, username: &str, display_name: &str) -> MemberId {
        let (ceremony, options) = self
            .identity
            .begin_enrollment(
                &link_token(link),
                Some(Username::parse(username).expect("valid username")),
                Some(DisplayName::parse(display_name).expect("valid display name")),
            )
            .await
            .expect("begin enrollment");
        let credential = self.register(options).expect("soft passkey registration");
        self.identity
            .finish_enrollment(
                ceremony,
                TokenName::parse("soft passkey").expect("valid label"),
                credential,
            )
            .await
            .expect("finish enrollment")
    }

    /// Signs in as `username`: begin, sign, finish.
    ///
    /// # Panics
    ///
    /// If any step fails.
    pub async fn login(&mut self, username: &str) -> MemberId {
        let (ceremony, options) = self
            .identity
            .begin_login(&Username::parse(username).expect("valid username"))
            .await
            .expect("begin login");
        let credential = self.sign(options);
        self.identity
            .finish_login(ceremony, credential)
            .await
            .expect("finish login")
    }

    /// Signs in as `username` and starts a session, as the login endpoint
    /// does: returns the member and the cookie value.
    ///
    /// # Panics
    ///
    /// If sign-in or session creation fails.
    pub async fn login_with_session(&mut self, username: &str) -> (MemberId, SecretToken) {
        let member = self.login(username).await;
        let cookie = self.sessions.create(member).await.expect("create session");
        (member, cookie)
    }
}
