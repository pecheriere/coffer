//! The integration tests of `coffer-store`, one module per area, built as a
//! single test binary so a change relinks one program instead of one per file.

mod duplicates;
mod foundation;
mod identity;
mod ledger;
mod plan;
mod sync;
