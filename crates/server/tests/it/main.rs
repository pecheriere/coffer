//! The integration tests of `coffer-server`, one module per area, built as a
//! single test binary so a change relinks one program instead of one per file.

mod accounts;
mod agent_tokens;
mod api;
mod appearance;
mod assets;
mod categories;
mod cli;
mod connections;
mod connections_api;
mod connections_page;
mod duplicates;
mod egress;
mod egress_relay;
mod grants;
mod healthcheck;
mod identity;
mod matching;
mod openapi;
mod pages;
mod panics;
mod plaid_client;
mod plaid_forget;
mod plaid_link;
mod plaid_sync;
mod serve;
mod sessions;
mod simplefin;
mod sync_engine;
mod transactions_api;
mod transactions_core;
