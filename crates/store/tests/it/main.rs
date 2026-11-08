//! The integration tests of `coffer-store`, one module per area, built as a
//! single test binary so a change relinks one program instead of one per file.

mod account_belonging;
mod duplicates;
mod emergency_funds;
mod forecast;
mod foundation;
mod fx;
mod identity;
mod ledger;
mod net_worth;
mod plan;
mod plan_tiers;
mod provider_dismissal;
mod review_count;
mod rules;
mod sync;
