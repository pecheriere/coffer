//! Server secret key, token minting and keyed hashing.

use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use secrecy::{ExposeSecret, SecretString};
use thiserror::Error;
use zeroize::Zeroizing;

const KEY_LEN: usize = 32;
const AGENT_PREFIX: &str = "coffer_";
const AGENT_PREFIX_LEN: usize = 12;

/// A problem loading or creating the server key.
#[derive(Debug, Error)]
pub enum SecretError {
    /// Filesystem failure.
    #[error("secret key file {path}: {source}")]
    Io {
        /// Key file path.
        path: String,
        /// Underlying error.
        source: io::Error,
    },
    /// The file does not hold exactly 32 bytes.
    #[error("secret key file {path} must be exactly 32 bytes")]
    InvalidLength {
        /// Key file path.
        path: String,
    },
    /// The file is accessible to group or others.
    #[error("secret key file {path} must not be group or world accessible (use mode 0600)")]
    InsecurePermissions {
        /// Key file path.
        path: String,
    },
    /// The operating system could not supply random bytes.
    #[error("operating system random source failed: {0}")]
    Random(getrandom::Error),
}

/// The kind of secret token; separates hash domains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    /// Browser session token.
    Session,
    /// Household invite token.
    Invite,
    /// Agent API token.
    Agent,
}

impl TokenKind {
    fn name(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Invite => "invite",
            Self::Agent => "agent",
        }
    }
}

/// A freshly minted secret token. The value is only reachable through
/// [`ExposeSecret::expose_secret`].
pub struct SecretToken(SecretString);

impl ExposeSecret<str> for SecretToken {
    fn expose_secret(&self) -> &str {
        self.0.expose_secret()
    }
}

impl std::fmt::Debug for SecretToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretToken(<redacted>)")
    }
}

/// Mints a token: 32 random bytes, base64url without padding. Agent tokens
/// carry the `coffer_` prefix.
///
/// # Panics
///
/// Panics if the operating system cannot supply random bytes; continuing
/// without entropy would be unsafe.
#[must_use]
pub fn mint(kind: TokenKind) -> SecretToken {
    let mut bytes = Zeroizing::new([0u8; KEY_LEN]);
    if let Err(err) = getrandom::fill(bytes.as_mut_slice()) {
        panic!("operating system random source failed: {err}");
    }
    let encoded = URL_SAFE_NO_PAD.encode(bytes.as_slice());
    let token = match kind {
        TokenKind::Agent => format!("{AGENT_PREFIX}{encoded}"),
        TokenKind::Session | TokenKind::Invite => encoded,
    };
    SecretToken(SecretString::from(token))
}

/// Returns the non-secret display prefix of an agent token: the first 12
/// characters (`coffer_` plus 5).
#[must_use]
pub fn agent_token_prefix(token: &str) -> String {
    token.chars().take(AGENT_PREFIX_LEN).collect()
}

/// The 32-byte server secret from which token hashes and CSRF tokens derive.
/// Zeroized on drop.
pub struct ServerKey(Zeroizing<[u8; KEY_LEN]>);

impl std::fmt::Debug for ServerKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ServerKey(<redacted>)")
    }
}

impl ServerKey {
    /// Loads the key from `path`, creating it (atomically, mode 0600) when
    /// absent. The parent directory must already exist.
    ///
    /// # Errors
    ///
    /// Fails if the file cannot be read or created, is not exactly 32 bytes,
    /// or is accessible to group or others.
    pub fn load_or_create(path: &Path) -> Result<Self, SecretError> {
        let display = path.display().to_string();
        let io_err = |source| SecretError::Io {
            path: display.clone(),
            source,
        };

        // Generate before creating the file so a failure never leaves an empty key file.
        let mut fresh = Zeroizing::new([0u8; KEY_LEN]);
        getrandom::fill(fresh.as_mut_slice()).map_err(SecretError::Random)?;

        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);

        match options.open(path) {
            Ok(mut file) => {
                if let Err(source) = file
                    .write_all(fresh.as_slice())
                    .and_then(|()| file.sync_all())
                {
                    drop(file);
                    // Best effort: a partial key file would be rejected on the next start.
                    let _ = std::fs::remove_file(path);
                    return Err(io_err(source));
                }
                Ok(Self(fresh))
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                let file = std::fs::File::open(path).map_err(io_err)?;
                let metadata = file.metadata().map_err(io_err)?;
                #[cfg(unix)]
                if std::os::unix::fs::PermissionsExt::mode(&metadata.permissions()) & 0o077 != 0 {
                    return Err(SecretError::InsecurePermissions { path: display });
                }
                if metadata.len() != KEY_LEN as u64 {
                    return Err(SecretError::InvalidLength { path: display });
                }
                let mut bytes = Zeroizing::new([0u8; KEY_LEN]);
                file.take(KEY_LEN as u64)
                    .read_exact(bytes.as_mut_slice())
                    .map_err(io_err)?;
                Ok(Self(bytes))
            }
            Err(err) => Err(io_err(err)),
        }
    }

    fn derived(&self, context: &str) -> Zeroizing<[u8; KEY_LEN]> {
        Zeroizing::new(blake3::derive_key(context, self.0.as_slice()))
    }

    /// Keyed BLAKE3 hash of `token` under a per-kind derived key. This is
    /// what gets stored; the token itself never is.
    #[must_use]
    pub fn hash(&self, kind: TokenKind, token: &str) -> [u8; 32] {
        let key = self.derived(&format!("coffer 2026-10 token hash {}", kind.name()));
        *blake3::keyed_hash(&key, token.as_bytes()).as_bytes()
    }

    /// CSRF token bound to a session: base64url of a keyed hash of the
    /// session's token hash.
    #[must_use]
    pub fn csrf_token(&self, session_hash: &[u8; 32]) -> String {
        let key = self.derived("coffer 2026-10 csrf");
        URL_SAFE_NO_PAD.encode(blake3::keyed_hash(&key, session_hash).as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn key_created_with_0600_and_reloaded_identically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.key");
        let first = ServerKey::load_or_create(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let second = ServerKey::load_or_create(&path).unwrap();
        assert_eq!(
            first.hash(TokenKind::Session, "tok"),
            second.hash(TokenKind::Session, "tok")
        );
    }

    #[test]
    fn key_rejects_world_readable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.key");
        ServerKey::load_or_create(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            ServerKey::load_or_create(&path),
            Err(SecretError::InsecurePermissions { .. })
        ));
    }

    #[test]
    fn key_rejects_wrong_length() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.key");
        std::fs::write(&path, [0u8; 16]).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(matches!(
            ServerKey::load_or_create(&path),
            Err(SecretError::InvalidLength { .. })
        ));
    }

    #[test]
    fn key_does_not_create_missing_parent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing").join("secret.key");
        assert!(matches!(
            ServerKey::load_or_create(&path),
            Err(SecretError::Io { .. })
        ));
    }

    #[test]
    fn debug_does_not_print_key_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let key = ServerKey::load_or_create(&dir.path().join("k")).unwrap();
        assert_eq!(format!("{key:?}"), "ServerKey(<redacted>)");
    }

    #[test]
    fn hash_differs_by_kind() {
        let dir = tempfile::tempdir().unwrap();
        let key = ServerKey::load_or_create(&dir.path().join("k")).unwrap();
        assert_ne!(
            key.hash(TokenKind::Session, "same"),
            key.hash(TokenKind::Agent, "same")
        );
    }

    #[test]
    fn different_keys_give_different_hashes_and_csrf_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let one = ServerKey::load_or_create(&dir.path().join("one")).unwrap();
        let two = ServerKey::load_or_create(&dir.path().join("two")).unwrap();
        for kind in [TokenKind::Session, TokenKind::Invite, TokenKind::Agent] {
            assert_ne!(one.hash(kind, "same"), two.hash(kind, "same"), "{kind:?}");
        }
        let session = [7_u8; 32];
        assert_ne!(one.csrf_token(&session), two.csrf_token(&session));
    }

    #[test]
    fn agent_token_format() {
        let t = mint(TokenKind::Agent);
        let s = t.expose_secret();
        assert!(s.starts_with("coffer_"));
        assert_eq!(s.len(), 50);
        assert_eq!(agent_token_prefix(s).len(), 12);
        assert_eq!(agent_token_prefix(s), s[..12]);
    }

    #[test]
    fn session_and_invite_tokens_are_43_chars_and_unique() {
        let a = mint(TokenKind::Session);
        let b = mint(TokenKind::Invite);
        assert_eq!(a.expose_secret().len(), 43);
        assert_eq!(b.expose_secret().len(), 43);
        assert_ne!(a.expose_secret(), mint(TokenKind::Session).expose_secret());
    }

    #[test]
    fn csrf_token_is_stable_per_session_and_differs_across_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let key = ServerKey::load_or_create(&dir.path().join("k")).unwrap();
        let h1 = key.hash(TokenKind::Session, "one");
        let h2 = key.hash(TokenKind::Session, "two");
        assert_eq!(key.csrf_token(&h1), key.csrf_token(&h1));
        assert_ne!(key.csrf_token(&h1), key.csrf_token(&h2));
    }
}
