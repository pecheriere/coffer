//! Provider credential vault (spec §8.3).
//!
//! Provider credentials (a `SimpleFin` access URL, later a Plaid access token)
//! are stored only sealed: XChaCha20-Poly1305 under a key derived from the
//! server key, with the connection id as associated data so a sealed blob
//! opens only for the connection it was sealed for. In clear they exist only
//! in memory, as a [`SecretString`] or in [`Zeroizing`] buffers.
//!
//! Sealed format: `version (0x01) || nonce (24 bytes) || ciphertext || tag (16
//! bytes)`. The version byte selects the key, so a dedicated credential key
//! can be introduced later without rewriting existing rows at once.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use coffer_domain::ids::ConnectionId;
use secrecy::{ExposeSecret, SecretString};
use thiserror::Error;
use zeroize::Zeroizing;

use crate::secrets::ServerKey;

/// BLAKE3 `derive_key` context of the version 1 key.
const KEY_CONTEXT_V1: &str = "coffer 2026-10 provider credentials v1";
/// Version byte of blobs sealed under the derived version 1 key.
const VERSION_V1: u8 = 0x01;
const NONCE_LEN: usize = 24;
const TAG_LEN: usize = 16;
/// Prefix of the associated data; the connection id's 16 bytes follow.
const AAD_PREFIX: &[u8] = b"coffer connection credentials\0";

/// Why credentials could not be sealed or opened. Every message is fixed: no
/// variant carries credential bytes, sealed or in clear.
#[derive(Debug, Error)]
pub enum VaultError {
    /// The sealed blob is too short, or what it holds is not UTF-8; also a
    /// plaintext too long for the cipher to seal.
    #[error("sealed credentials are malformed")]
    Malformed,
    /// The version byte names no key this server knows.
    #[error("sealed credentials have an unknown version")]
    UnknownVersion,
    /// Authentication failed: another key, another connection, or a tampered
    /// blob.
    #[error("sealed credentials could not be opened")]
    Decrypt,
    /// The operating system could not supply a nonce.
    #[error("operating system random source failed")]
    Random,
}

/// Credentials as stored in the `connections.credentials` column. Opaque:
/// only the [`CredentialVault`] that sealed them can open them. Its `Debug`
/// output shows only the length.
pub struct SealedCredentials(Vec<u8>);

impl SealedCredentials {
    /// Length of the sealed blob in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the blob is empty (never true for a blob this vault sealed).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for SealedCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SealedCredentials({} bytes)", self.0.len())
    }
}

/// Wraps bytes read from the store.
impl From<Vec<u8>> for SealedCredentials {
    fn from(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
}

/// The bytes to write to the store.
impl From<SealedCredentials> for Vec<u8> {
    fn from(sealed: SealedCredentials) -> Self {
        sealed.0
    }
}

/// Seals and opens provider credentials under a key derived from the server
/// key. The key is zeroized on drop and never printed.
pub struct CredentialVault {
    key: Zeroizing<[u8; 32]>,
}

impl std::fmt::Debug for CredentialVault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CredentialVault(<redacted>)")
    }
}

impl CredentialVault {
    /// A vault whose version 1 key is derived from `key`.
    #[must_use]
    pub fn new(key: &ServerKey) -> Self {
        Self {
            key: key.derived(KEY_CONTEXT_V1),
        }
    }

    /// Seals `plaintext` for `connection` under a fresh random nonce.
    ///
    /// # Errors
    ///
    /// [`VaultError::Random`] when the operating system cannot supply a
    /// nonce; [`VaultError::Malformed`] when the plaintext is too long for
    /// the cipher.
    pub fn seal(
        &self,
        connection: ConnectionId,
        plaintext: &SecretString,
    ) -> Result<SealedCredentials, VaultError> {
        let mut nonce = [0_u8; NONCE_LEN];
        getrandom::fill(&mut nonce).map_err(|_| VaultError::Random)?;
        let ciphertext = self
            .cipher()
            .encrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: plaintext.expose_secret().as_bytes(),
                    aad: &associated_data(connection),
                },
            )
            .map_err(|_| VaultError::Malformed)?;
        let mut sealed = Vec::with_capacity(1 + NONCE_LEN + ciphertext.len());
        sealed.push(VERSION_V1);
        sealed.extend_from_slice(&nonce);
        sealed.extend_from_slice(&ciphertext);
        Ok(SealedCredentials(sealed))
    }

    /// Opens credentials sealed for `connection`.
    ///
    /// # Errors
    ///
    /// [`VaultError::Malformed`] for a blob too short to be sealed
    /// credentials or holding non-UTF-8 text, [`VaultError::UnknownVersion`]
    /// for an unknown version byte, [`VaultError::Decrypt`] when
    /// authentication fails (another key, another connection, tampering).
    pub fn open(
        &self,
        connection: ConnectionId,
        sealed: &SealedCredentials,
    ) -> Result<SecretString, VaultError> {
        let (&version, rest) = sealed.0.split_first().ok_or(VaultError::Malformed)?;
        if version != VERSION_V1 {
            return Err(VaultError::UnknownVersion);
        }
        if rest.len() < NONCE_LEN + TAG_LEN {
            return Err(VaultError::Malformed);
        }
        let (nonce, ciphertext) = rest.split_at(NONCE_LEN);
        let nonce = XNonce::try_from(nonce).map_err(|_| VaultError::Malformed)?;
        let plaintext = Zeroizing::new(
            self.cipher()
                .decrypt(
                    &nonce,
                    Payload {
                        msg: ciphertext,
                        aad: &associated_data(connection),
                    },
                )
                .map_err(|_| VaultError::Decrypt)?,
        );
        let text = std::str::from_utf8(&plaintext).map_err(|_| VaultError::Malformed)?;
        Ok(SecretString::from(text))
    }

    fn cipher(&self) -> XChaCha20Poly1305 {
        let key: &Key = (&*self.key).into();
        XChaCha20Poly1305::new(key)
    }
}

/// `"coffer connection credentials\0" || connection id (16 bytes)`.
fn associated_data(connection: ConnectionId) -> Vec<u8> {
    let mut aad = Vec::with_capacity(AAD_PREFIX.len() + 16);
    aad.extend_from_slice(AAD_PREFIX);
    aad.extend_from_slice(connection.as_uuid().as_bytes());
    aad
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use chacha20poly1305::aead::{Aead, KeyInit, Payload};
    use chacha20poly1305::{XChaCha20Poly1305, XNonce};
    use coffer_domain::ids::ConnectionId;
    use secrecy::{ExposeSecret, SecretString};

    use super::{CredentialVault, SealedCredentials, VaultError};
    use crate::secrets::ServerKey;

    const PLAINTEXT: &str = "https://user:pass-7f3a9c@bridge.example.test/simplefin";

    fn server_key(dir: &tempfile::TempDir, name: &str) -> ServerKey {
        ServerKey::load_or_create(&dir.path().join(name)).unwrap()
    }

    fn secret() -> SecretString {
        SecretString::from(PLAINTEXT)
    }

    fn sealed_bytes(sealed: SealedCredentials) -> Vec<u8> {
        Vec::from(sealed)
    }

    #[test]
    fn seal_open_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let key = server_key(&dir, "k");
        let connection = ConnectionId::new();
        let sealed = CredentialVault::new(&key)
            .seal(connection, &secret())
            .unwrap();

        // Through the stored form (the connections table's BLOB) and a vault
        // rebuilt from the same server key, as after a restart.
        let stored = sealed_bytes(sealed);
        let reopened = CredentialVault::new(&key)
            .open(connection, &SealedCredentials::from(stored))
            .unwrap();
        assert_eq!(reopened.expose_secret(), PLAINTEXT);
    }

    #[test]
    fn sealed_format_matches_spec() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("k");
        std::fs::write(&path, [42_u8; 32]).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let key = ServerKey::load_or_create(&path).unwrap();
        let connection = ConnectionId::new();

        let blob = sealed_bytes(
            CredentialVault::new(&key)
                .seal(connection, &secret())
                .unwrap(),
        );

        // Spec §8.3: version 0x01, 24-byte nonce, ciphertext and 16-byte tag,
        // under BLAKE3 derive_key of the server key, connection id as AAD.
        assert_eq!(blob[0], 0x01);
        assert_eq!(blob.len(), 1 + 24 + PLAINTEXT.len() + 16);
        let derived = blake3::derive_key("coffer 2026-10 provider credentials v1", &[42_u8; 32]);
        let cipher = XChaCha20Poly1305::new(&derived.into());
        let mut aad = b"coffer connection credentials\0".to_vec();
        aad.extend_from_slice(connection.as_uuid().as_bytes());
        let nonce = XNonce::try_from(&blob[1..25]).unwrap();
        let plain = cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: &blob[25..],
                    aad: &aad,
                },
            )
            .unwrap();
        assert_eq!(plain, PLAINTEXT.as_bytes());
    }

    #[test]
    fn sealing_twice_differs() {
        let dir = tempfile::tempdir().unwrap();
        let vault = CredentialVault::new(&server_key(&dir, "k"));
        let connection = ConnectionId::new();
        let one = sealed_bytes(vault.seal(connection, &secret()).unwrap());
        let two = sealed_bytes(vault.seal(connection, &secret()).unwrap());
        assert_ne!(one[1..25], two[1..25], "fresh nonce per seal");
        assert_ne!(one, two);
    }

    #[test]
    fn tampered_byte_fails() {
        let dir = tempfile::tempdir().unwrap();
        let vault = CredentialVault::new(&server_key(&dir, "k"));
        let connection = ConnectionId::new();
        let blob = sealed_bytes(vault.seal(connection, &secret()).unwrap());
        let last = blob.len() - 1;
        let cases = [
            ("version", 0, VaultError::UnknownVersion),
            ("nonce", 5, VaultError::Decrypt),
            ("ciphertext", 30, VaultError::Decrypt),
            ("tag", last, VaultError::Decrypt),
        ];
        for (part, index, expected) in cases {
            let mut tampered = blob.clone();
            tampered[index] ^= 0x01;
            let err = vault
                .open(connection, &SealedCredentials::from(tampered))
                .unwrap_err();
            assert_eq!(
                std::mem::discriminant(&err),
                std::mem::discriminant(&expected),
                "{part}: {err:?}"
            );
        }
    }

    #[test]
    fn truncated_blob_is_malformed() {
        let dir = tempfile::tempdir().unwrap();
        let vault = CredentialVault::new(&server_key(&dir, "k"));
        let connection = ConnectionId::new();
        for blob in [Vec::new(), vec![0x01], vec![0x01; 1 + 24 + 15]] {
            let err = vault
                .open(connection, &SealedCredentials::from(blob.clone()))
                .unwrap_err();
            assert!(
                matches!(err, VaultError::Malformed),
                "{} bytes: {err:?}",
                blob.len()
            );
        }
    }

    #[test]
    fn wrong_connection_fails() {
        let dir = tempfile::tempdir().unwrap();
        let vault = CredentialVault::new(&server_key(&dir, "k"));
        let sealed = vault.seal(ConnectionId::new(), &secret()).unwrap();
        assert!(matches!(
            vault.open(ConnectionId::new(), &sealed),
            Err(VaultError::Decrypt)
        ));
    }

    #[test]
    fn other_server_key_fails() {
        let dir = tempfile::tempdir().unwrap();
        let connection = ConnectionId::new();
        let sealed = CredentialVault::new(&server_key(&dir, "one"))
            .seal(connection, &secret())
            .unwrap();
        assert!(matches!(
            CredentialVault::new(&server_key(&dir, "two")).open(connection, &sealed),
            Err(VaultError::Decrypt)
        ));
    }

    #[test]
    fn unknown_version_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let vault = CredentialVault::new(&server_key(&dir, "k"));
        let connection = ConnectionId::new();
        let mut blob = sealed_bytes(vault.seal(connection, &secret()).unwrap());
        for version in [0x00, 0x02, 0xff] {
            blob[0] = version;
            assert!(
                matches!(
                    vault.open(connection, &SealedCredentials::from(blob.clone())),
                    Err(VaultError::UnknownVersion)
                ),
                "version {version:#04x}"
            );
        }
    }

    #[test]
    fn debug_never_prints_plaintext_or_ciphertext() {
        let dir = tempfile::tempdir().unwrap();
        let vault = CredentialVault::new(&server_key(&dir, "k"));
        let connection = ConnectionId::new();
        let sealed = vault.seal(connection, &secret()).unwrap();
        let len = sealed.len();

        let vault_debug = format!("{vault:?}");
        assert_eq!(vault_debug, "CredentialVault(<redacted>)");

        // Only the length: no sealed byte, in any notation.
        assert_eq!(
            format!("{sealed:?}"),
            format!("SealedCredentials({len} bytes)")
        );

        // Every error, in Debug and Display, is a fixed text.
        let mut texts = Vec::new();
        for err in [
            vault.open(
                ConnectionId::new(),
                &vault.seal(connection, &secret()).unwrap(),
            ),
            vault.open(connection, &SealedCredentials::from(vec![0x02; 64])),
            vault.open(connection, &SealedCredentials::from(vec![0x01; 3])),
        ]
        .into_iter()
        .map(Result::unwrap_err)
        {
            texts.push(format!("{err:?}"));
            texts.push(err.to_string());
        }
        texts.push(format!(
            "{:?}",
            vault
                .open(connection, &vault.seal(connection, &secret()).unwrap())
                .unwrap()
        ));
        for text in texts {
            assert!(!text.contains(PLAINTEXT), "{text}");
            assert!(!text.contains("pass-7f3a9c"), "{text}");
        }
    }
}
