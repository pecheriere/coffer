//! Refuses to serve the staging stack's made-up data anywhere but localhost.
//!
//! The staging seed signs its admin in with a private key committed to this repository
//! (`staging/fixtures/passkeys.json`), so anyone holding it can sign in as that admin
//! wherever the data ends up. This is a guard against an accident, not a security boundary:
//! a script with that key signs for `http://localhost` whatever the address it reaches, so
//! on localhost the seed is safe only while nothing else can reach it. Nothing here lets
//! anyone in: it stops `serve`, or warns.

use std::net::SocketAddr;

use coffer_store::Store;

use super::CliError;
use crate::auth::staging_fixture::{STAGING_ADMIN_CREDENTIAL_ID, on_localhost};
use crate::config::Config;

const REFUSED: &str = "this database is the staging seed, whose admin passkey is public: anyone \
                       can sign in as its admin. It may only be served with COFFER_PUBLIC_URL \
                       on localhost, reachable from this machine alone";

/// Fails when an admin holds the seeded passkey and the public URL isn't localhost, and warns
/// when it is but the listener isn't loopback. Only an admin counts: a member's passkey doesn't
/// make this database the seed.
pub(super) async fn refuse_outside_localhost(
    store: &Store,
    config: &Config,
    listen: SocketAddr,
) -> Result<(), CliError> {
    if !store
        .admin_holds_credential(STAGING_ADMIN_CREDENTIAL_ID)
        .await?
    {
        return Ok(());
    }
    if !on_localhost(config) {
        return Err(CliError::Failed(REFUSED.to_owned()));
    }
    if !listen.ip().is_loopback() {
        tracing::warn!(
            %listen,
            "this database is the staging seed, whose admin passkey is public, and this \
             address isn't loopback: anyone who can reach it can sign in as its admin, \
             localhost public URL or not. Reach it from this machine alone"
        );
    }
    Ok(())
}
