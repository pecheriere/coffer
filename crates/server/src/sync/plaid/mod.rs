//! The Plaid provider (spec §8.6): `client` speaks Plaid's API, `provider`
//! puts it behind the [`Provider`](super::provider::Provider) interface.

pub mod client;
pub mod provider;
