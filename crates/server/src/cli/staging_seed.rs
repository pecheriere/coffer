//! Refuses to serve the staging stack's made-up data anywhere but localhost.
//!
//! The staging seed signs its admin in with a private key committed to this
//! repository (`staging/fixtures/passkeys.json`), so anyone can sign in as that
//! admin wherever the data ends up. Nothing here lets anyone in: it only stops
//! `serve`.

use coffer_store::Store;

use super::CliError;
use crate::config::Config;

/// The credential id of the seeded admin's passkey.
pub const STAGING_ADMIN_CREDENTIAL_ID: &[u8] = b"coffer-staging-fixture-admin-passkey";

const REFUSED: &str = "this database is the staging seed, whose admin passkey is public: \
                       it may only be served with COFFER_PUBLIC_URL on localhost";

/// Fails when an admin holds the seeded passkey and the public URL isn't localhost. Only an
/// admin counts: a member can register any credential for themselves, and that must not be
/// a way to stop the server.
pub(super) async fn refuse_outside_localhost(
    store: &Store,
    config: &Config,
) -> Result<(), CliError> {
    if config.public_url.domain() == Some("localhost") {
        return Ok(());
    }
    if store
        .admin_holds_credential(STAGING_ADMIN_CREDENTIAL_ID)
        .await?
    {
        return Err(CliError::Failed(REFUSED.to_owned()));
    }
    Ok(())
}
