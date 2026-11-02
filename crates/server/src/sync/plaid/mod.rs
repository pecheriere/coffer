//! The Plaid provider (spec §8.6): `client` speaks Plaid's API, `provider`
//! puts it behind the [`Provider`](super::provider::Provider) interface, and
//! `kinds` reads what Plaid says an account is.

pub mod client;
mod kinds;
pub mod provider;
