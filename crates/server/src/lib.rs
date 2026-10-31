//! coffer server: configuration, secrets, passkey identity and token handling.

pub mod appearance;
pub mod assets;
pub mod auth;
pub mod cli;
pub mod config;
pub mod http;
pub mod ledger;
pub mod secrets;
#[cfg(feature = "testkit")]
pub mod testkit;
pub mod themes;
