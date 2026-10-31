//! `WebAuthn` relying party and in-memory ceremony storage.
//!
//! webauthn-rs picks its own random challenge and returns a state object
//! that must be kept server-side until the authenticator answers. `begin`
//! stores that state, together with what the ceremony is bound to (invite,
//! member), under a random ceremony id for 120 s; `finish` removes it (a
//! ceremony is single use, whether it then succeeds or fails) and verifies
//! the response against it. Ceremonies live in memory only. Each kind has its
//! own budget, so a flood of unauthenticated sign-in attempts can never
//! starve enrollment or adding a passkey; and a member has at most
//! [`MAX_LOGINS_PER_MEMBER`] sign-ins pending, the oldest being dropped for a
//! new one. User verification is required for registration and
//! authentication.

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use coffer_domain::identity::{DisplayName, Role, Username};
use coffer_domain::ids::{InviteId, MemberId};
use jiff::{SignedDuration, Timestamp};
use url::Url;
use webauthn_rs::prelude::{
    AuthenticationResult, CreationChallengeResponse, CredentialID, Passkey, PasskeyAuthentication,
    PasskeyRegistration, PublicKeyCredential, RegisterPublicKeyCredential,
    RequestChallengeResponse, Webauthn, WebauthnBuilder, WebauthnError,
};

use super::AuthError;

/// How long a begun ceremony may be finished.
pub const CEREMONY_TTL: SignedDuration = SignedDuration::from_secs(120);

/// Sign-in ceremonies kept at once.
const MAX_LOGINS: usize = 64;

/// Sign-in ceremonies kept at once for one member; a new one evicts that
/// member's oldest.
pub const MAX_LOGINS_PER_MEMBER: usize = 4;

/// Enrollment ceremonies kept at once.
const MAX_ENROLLMENTS: usize = 32;

/// Add-passkey ceremonies kept at once.
const MAX_ADD_PASSKEYS: usize = 32;

/// Random bytes in a ceremony id.
const CEREMONY_ID_BYTES: usize = 32;

/// Relying party name shown by authenticators.
const RP_NAME: &str = "coffer";

/// Identifies a ceremony in progress. 256 random bits, base64url; it is a
/// bearer handle for the ceremony, so `Debug` does not print it.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct CeremonyId(String);

impl CeremonyId {
    fn random() -> Result<Self, AuthError> {
        let mut bytes = [0_u8; CEREMONY_ID_BYTES];
        getrandom::fill(&mut bytes)
            .map_err(|_| AuthError::Internal("operating system random source failed"))?;
        Ok(Self(URL_SAFE_NO_PAD.encode(bytes)))
    }

    /// The id as sent to the browser.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for CeremonyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CeremonyId(<redacted>)")
    }
}

impl fmt::Display for CeremonyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for CeremonyId {
    type Err = AuthError;

    /// Accepts only the form [`CeremonyId`] produces; anything else is a
    /// ceremony that cannot exist.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match URL_SAFE_NO_PAD.decode(s) {
            Ok(bytes) if bytes.len() == CEREMONY_ID_BYTES => Ok(Self(s.to_owned())),
            _ => Err(AuthError::CeremonyExpired),
        }
    }
}

/// Who an enrollment ceremony registers a passkey for.
pub(crate) enum EnrollTarget {
    /// A member created when the ceremony finishes. `member` is chosen at
    /// `begin` because it is the `WebAuthn` user handle.
    New {
        member: MemberId,
        username: Username,
        display_name: DisplayName,
        role: Role,
        bootstrap: bool,
    },
    /// An existing member being re-enrolled.
    Existing(MemberId),
}

/// What a ceremony is bound to, fixed when it begins.
pub(crate) enum Ceremony {
    Enrollment {
        invite: InviteId,
        token_hash: [u8; 32],
        invite_expires_at: Timestamp,
        target: EnrollTarget,
        state: Box<PasskeyRegistration>,
    },
    AddPasskey {
        member: MemberId,
        state: Box<PasskeyRegistration>,
    },
    Login {
        member: MemberId,
        state: Box<PasskeyAuthentication>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Enrollment,
    AddPasskey,
    Login,
}

impl Kind {
    fn budget(self) -> usize {
        match self {
            Self::Enrollment => MAX_ENROLLMENTS,
            Self::AddPasskey => MAX_ADD_PASSKEYS,
            Self::Login => MAX_LOGINS,
        }
    }
}

impl Ceremony {
    fn kind(&self) -> Kind {
        match self {
            Self::Enrollment { .. } => Kind::Enrollment,
            Self::AddPasskey { .. } => Kind::AddPasskey,
            Self::Login { .. } => Kind::Login,
        }
    }

    fn login_member(&self) -> Option<MemberId> {
        match self {
            Self::Login { member, .. } => Some(*member),
            Self::Enrollment { .. } | Self::AddPasskey { .. } => None,
        }
    }
}

struct Pending {
    ceremony: Ceremony,
    expires_at: Timestamp,
    /// Insertion order, to find a member's oldest sign-in.
    seq: u64,
}

#[derive(Default)]
struct Table {
    pending: HashMap<CeremonyId, Pending>,
    next_seq: u64,
}

/// The `WebAuthn` relying party and its ceremonies in progress.
pub struct Passkeys {
    webauthn: Webauthn,
    ceremonies: Mutex<Table>,
}

impl fmt::Debug for Passkeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Passkeys").finish_non_exhaustive()
    }
}

impl Passkeys {
    /// The relying party for `public_url`: its host is the RP id and its
    /// origin the only accepted origin. RP name `coffer`; user verification
    /// is required.
    ///
    /// # Errors
    ///
    /// [`AuthError::Webauthn`] if `public_url` has no domain (an IP address
    /// cannot be a relying party) or is otherwise unusable.
    pub fn new(public_url: &Url) -> Result<Self, AuthError> {
        let rp_id = public_url
            .domain()
            .ok_or(AuthError::Webauthn(WebauthnError::Configuration))?;
        let origin = Url::parse(&public_url.origin().ascii_serialization())
            .map_err(|_| AuthError::Webauthn(WebauthnError::Configuration))?;
        let timeout = Duration::from_secs(CEREMONY_TTL.as_secs().unsigned_abs());
        let webauthn = WebauthnBuilder::new(rp_id, &origin)
            .and_then(|builder| builder.rp_name(RP_NAME).timeout(timeout).build())
            .map_err(AuthError::Webauthn)?;
        Ok(Self {
            webauthn,
            ceremonies: Mutex::new(Table::default()),
        })
    }

    /// Registration options for `member`, excluding credentials it already
    /// has.
    pub(crate) fn start_registration(
        &self,
        member: MemberId,
        username: &Username,
        display_name: &DisplayName,
        exclude: Vec<CredentialID>,
    ) -> Result<(CreationChallengeResponse, PasskeyRegistration), AuthError> {
        self.webauthn
            .start_passkey_registration(
                member.as_uuid(),
                username.as_str(),
                display_name.as_str(),
                Some(exclude),
            )
            .map_err(AuthError::Webauthn)
    }

    /// Verifies a registration response against its ceremony state. A
    /// response that does not verify is the client's doing
    /// ([`AuthError::PasskeyRejected`]); a failure on the server's side
    /// (configuration, crypto backend) is [`AuthError::Webauthn`].
    pub(crate) fn finish_registration(
        &self,
        credential: &RegisterPublicKeyCredential,
        state: &PasskeyRegistration,
    ) -> Result<Passkey, AuthError> {
        self.webauthn
            .finish_passkey_registration(credential, state)
            .map_err(rejection)
    }

    /// Authentication options restricted to `passkeys`.
    pub(crate) fn start_authentication(
        &self,
        passkeys: &[Passkey],
    ) -> Result<(RequestChallengeResponse, PasskeyAuthentication), AuthError> {
        self.webauthn
            .start_passkey_authentication(passkeys)
            .map_err(AuthError::Webauthn)
    }

    /// Verifies an assertion against its ceremony state, requiring user
    /// verification. An assertion that does not verify is
    /// [`AuthError::PasskeyRejected`], carrying the cause so the caller can
    /// tell a counter regression (possible cloned credential) apart for
    /// logging; callers must still report every such failure the same way.
    /// A failure on the server's side is [`AuthError::Webauthn`].
    pub(crate) fn finish_authentication(
        &self,
        credential: &PublicKeyCredential,
        state: &PasskeyAuthentication,
    ) -> Result<AuthenticationResult, AuthError> {
        let result = self
            .webauthn
            .finish_passkey_authentication(credential, state)
            .map_err(rejection)?;
        // Required by the ceremony's policy already; checked again because
        // a sign-in must never rest on user presence alone.
        if result.user_verified() {
            Ok(result)
        } else {
            Err(AuthError::PasskeyRejected(WebauthnError::UserNotVerified))
        }
    }

    /// Stores `ceremony` until `now + CEREMONY_TTL`. Expired ceremonies are
    /// dropped first; a sign-in beyond [`MAX_LOGINS_PER_MEMBER`] evicts that
    /// member's oldest pending sign-in.
    pub(crate) fn insert(
        &self,
        ceremony: Ceremony,
        now: Timestamp,
    ) -> Result<CeremonyId, AuthError> {
        let expires_at = now
            .checked_add(CEREMONY_TTL)
            .map_err(|_| AuthError::Internal("ceremony expiry out of range"))?;
        let id = CeremonyId::random()?;
        let kind = ceremony.kind();
        let mut table = self.lock()?;
        table.pending.retain(|_, pending| pending.expires_at > now);
        if let Some(member) = ceremony.login_member() {
            let mut mine: Vec<(u64, CeremonyId)> = table
                .pending
                .iter()
                .filter(|(_, pending)| pending.ceremony.login_member() == Some(member))
                .map(|(id, pending)| (pending.seq, id.clone()))
                .collect();
            mine.sort_unstable_by_key(|(seq, _)| *seq);
            let excess = (mine.len() + 1).saturating_sub(MAX_LOGINS_PER_MEMBER);
            for (_, oldest) in mine.into_iter().take(excess) {
                table.pending.remove(&oldest);
            }
        }
        let in_use = table
            .pending
            .values()
            .filter(|pending| pending.ceremony.kind() == kind)
            .count();
        if in_use >= kind.budget() {
            return Err(AuthError::Busy);
        }
        let seq = table.next_seq;
        table.next_seq = seq.wrapping_add(1);
        table.pending.insert(
            id.clone(),
            Pending {
                ceremony,
                expires_at,
                seq,
            },
        );
        Ok(id)
    }

    /// Removes ceremony `id` (it is single use) and returns it if it has not
    /// expired at `now`.
    pub(crate) fn take(&self, id: &CeremonyId, now: Timestamp) -> Result<Ceremony, AuthError> {
        let pending = self
            .lock()?
            .pending
            .remove(id)
            .ok_or(AuthError::CeremonyExpired)?;
        if pending.expires_at <= now {
            return Err(AuthError::CeremonyExpired);
        }
        Ok(pending.ceremony)
    }

    fn lock(&self) -> Result<MutexGuard<'_, Table>, AuthError> {
        self.ceremonies
            .lock()
            .map_err(|_| AuthError::Internal("ceremony table lock poisoned"))
    }
}

/// A failed ceremony verification: [`AuthError::PasskeyRejected`] when the
/// client's response is at fault, [`AuthError::Webauthn`] when the server is.
fn rejection(err: WebauthnError) -> AuthError {
    if server_fault(&err) {
        AuthError::Webauthn(err)
    } else {
        AuthError::PasskeyRejected(err)
    }
}

/// Whether a `WebAuthn` failure lies on the server's side: its relying party
/// configuration or stored credentials, or its crypto backend (OpenSSL). Every
/// other failure comes from what the client sent (attestation, challenge,
/// origin, flags, signature, encoding). The match is exhaustive (the enum is
/// not `non_exhaustive`), so a variant added by a webauthn-rs upgrade must be
/// classified here before the crate compiles.
fn server_fault(err: &WebauthnError) -> bool {
    match err {
        WebauthnError::Configuration
        | WebauthnError::ChallengePersistenceError
        | WebauthnError::AttestationCertificateTrustStoreEmpty
        | WebauthnError::MissingAttestationCaList
        | WebauthnError::OpenSSLError(_)
        | WebauthnError::OpenSSLErrorNoCurveName
        | WebauthnError::CredentialExistCheckError
        | WebauthnError::CredentialPersistenceError
        | WebauthnError::CredentialRetrievalError
        | WebauthnError::CredentialCounterUpdateFailure
        | WebauthnError::CredentialCompromiseReportFailure
        | WebauthnError::InconsistentUserVerificationPolicy
        | WebauthnError::InvalidUsername => true,
        WebauthnError::InvalidClientDataType
        | WebauthnError::MismatchedChallenge
        | WebauthnError::ChallengeNotFound
        | WebauthnError::InvalidRPOrigin
        | WebauthnError::InvalidRPIDHash
        | WebauthnError::UserNotPresent
        | WebauthnError::UserNotVerified
        | WebauthnError::InvalidExtensions
        | WebauthnError::AuthenticatorDataMissingExtension
        | WebauthnError::MissingAttestationCredentialData
        | WebauthnError::AttestationNotSupported
        | WebauthnError::AttestationStatementMapInvalid
        | WebauthnError::AttestationStatementResponseMissing
        | WebauthnError::AttestationStatementResponseInvalid
        | WebauthnError::AttestationStatementSigMissing
        | WebauthnError::AttestationStatementSigInvalid
        | WebauthnError::AttestationStatementVerMissing
        | WebauthnError::AttestationStatementVerInvalid
        | WebauthnError::AttestationStatementVerUnsupported
        | WebauthnError::AttestationStatementX5CMissing
        | WebauthnError::AttestationStatementX5CInvalid
        | WebauthnError::AttestationStatementAlgMissing
        | WebauthnError::AttestationStatementCertInfoMissing
        | WebauthnError::AttestationStatementMissingExtension
        | WebauthnError::AttestationStatementPubAreaMissing
        | WebauthnError::AttestationStatementAlgMismatch
        | WebauthnError::AttestationStatementAlgInvalid
        | WebauthnError::AttestationTrustFailure
        | WebauthnError::AttestationCertificateAAGUIDMismatch
        | WebauthnError::AttestationCertificateNonceMismatch
        | WebauthnError::AttestationTpmStInvalid
        | WebauthnError::AttestationTpmPubAreaMismatch
        | WebauthnError::AttestationTpmExtraDataInvalid
        | WebauthnError::AttestationTpmExtraDataMismatch
        | WebauthnError::AttestationTpmPubAreaHashUnknown
        | WebauthnError::AttestationTpmPubAreaHashInvalid
        | WebauthnError::AttestationTpmAttestCertifyInvalid
        | WebauthnError::AttestationCertificateRequirementsNotMet
        | WebauthnError::AttestationLeafCertMissing
        | WebauthnError::AttestationNotVerifiable
        | WebauthnError::AttestationUntrustedAaguid
        | WebauthnError::AttestationFormatMissingAaguid
        | WebauthnError::AttestationChainNotTrusted(_)
        | WebauthnError::AttestationCredentialSubjectKeyMismatch
        | WebauthnError::CertificatePublicKeyInvalid
        | WebauthnError::ParseBase64Failure(_)
        | WebauthnError::ParseCBORFailure(_)
        | WebauthnError::ParseJSONFailure(_)
        | WebauthnError::ParseNOMFailure
        | WebauthnError::ParseInsufficientBytesAvailable
        | WebauthnError::COSEKeyInvalidCBORValue
        | WebauthnError::COSEKeyInvalidType
        | WebauthnError::COSEKeyEDUnsupported
        | WebauthnError::COSEKeyECDSAXYInvalid
        | WebauthnError::COSEKeyRSANEInvalid
        | WebauthnError::COSEKeyECDSAInvalidCurve
        | WebauthnError::COSEKeyEDDSAXInvalid
        | WebauthnError::COSEKeyEDDSAInvalidCurve
        | WebauthnError::COSEKeyInvalidAlgorithm
        | WebauthnError::ECDSACurveInvalidNid
        | WebauthnError::CredentialMayNotBeHardwareBound
        | WebauthnError::CredentialInsecureCryptography
        | WebauthnError::CredentialAlreadyExists
        | WebauthnError::CredentialNotFound
        | WebauthnError::CredentialAlteredAlgFromRequest
        | WebauthnError::CredentialExcludedFromRequest
        | WebauthnError::CredentialPossibleCompromise
        | WebauthnError::CredentialBackupEligibilityInconsistent
        | WebauthnError::CredentialCrossOrigin
        | WebauthnError::TrustFailure
        | WebauthnError::AuthenticationFailure
        | WebauthnError::InvalidUserUniqueId
        | WebauthnError::SshPublicKeyBackupState
        | WebauthnError::SshPublicKeyEDUnsupported
        | WebauthnError::SshPublicKeyInvalidCurve
        | WebauthnError::SshPublicKeyInvalidPubkey
        | WebauthnError::SshPublicKeyInconsistentUserVerification => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{CeremonyId, Passkeys, rejection};
    use crate::auth::AuthError;
    use url::Url;
    use webauthn_rs::prelude::WebauthnError;

    #[test]
    fn server_side_failures_are_not_blamed_on_the_client() {
        for err in [
            WebauthnError::Configuration,
            WebauthnError::OpenSSLErrorNoCurveName,
            WebauthnError::MissingAttestationCaList,
            WebauthnError::InconsistentUserVerificationPolicy,
            WebauthnError::CredentialPersistenceError,
        ] {
            let mapped = rejection(err);
            assert!(matches!(mapped, AuthError::Webauthn(_)), "{mapped:?}");
        }
    }

    #[test]
    fn unverifiable_responses_are_the_clients_fault() {
        let json = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
        for err in [
            WebauthnError::MismatchedChallenge,
            WebauthnError::InvalidRPOrigin,
            WebauthnError::UserNotVerified,
            WebauthnError::AttestationStatementSigInvalid,
            WebauthnError::CredentialExcludedFromRequest,
            WebauthnError::ParseNOMFailure,
            WebauthnError::ParseJSONFailure(json),
        ] {
            let mapped = rejection(err);
            assert!(
                matches!(mapped, AuthError::PasskeyRejected(_)),
                "{mapped:?}"
            );
        }
    }

    #[test]
    fn ceremony_ids_are_random_and_parse_back() {
        let a = CeremonyId::random().unwrap();
        let b = CeremonyId::random().unwrap();
        assert_ne!(a, b);
        assert_eq!(a.as_str().parse::<CeremonyId>().unwrap(), a);
        assert_eq!(format!("{a:?}"), "CeremonyId(<redacted>)");
        for bad in ["", "short", &format!("{a}x"), &"=".repeat(43)] {
            assert!(
                matches!(bad.parse::<CeremonyId>(), Err(AuthError::CeremonyExpired)),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn relying_party_needs_a_domain() {
        Passkeys::new(&Url::parse("https://coffer.example").unwrap()).unwrap();
        assert!(matches!(
            Passkeys::new(&Url::parse("http://127.0.0.1:8080").unwrap()),
            Err(AuthError::Webauthn(_))
        ));
    }
}
