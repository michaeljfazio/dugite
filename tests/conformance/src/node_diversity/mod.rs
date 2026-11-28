//! Node-diversity conformance harness (#1114, Phase 1: #1116).
//!
//! Cardano's alternative node implementations publish four shared scenario reports to
//! <https://pragma-org.github.io/node-diversity-conformance/> (NDC): `ledger-rules`,
//! `cbor-decoding`, `uplc-conformance` and `uplc-turbo`.  This module is the part every
//! scenario shares, so that each runner emits only its per-case results:
//!
//! * [`pins`]: reads `tests/conformance/node-diversity/corpora.toml`, the one file that pins
//!   every corpus (the same file `scripts/node-diversity/fetch-corpora.sh` verifies).
//! * [`report`]: the report envelope of each scenario, the writer, the error-report path for a
//!   crashed generator, and validation of every report against the scenario's own
//!   `schema.json` from the pinned NDC checkout.
//!
//! The pattern is the one the other nodes use: a runner is an ordinary test, the environment
//! variable [`report::REPORT_ENV`] names the report file, **reporting never fails on a score**
//! (only a harness failure fails: a corpus that was not fetched, a report that does not match
//! its schema, an unwritable path), and a generator that crashes writes an error report in its
//! place so every scenario is still published.
//!
//! The runners themselves are Phases 2a-2d of #1114.  Until then each is a stub that writes a
//! schema-valid report marked [`report::ReportStatus::Stub`]: it carries no result.

pub mod pins;
pub mod report;
pub mod uplc_conformance;
pub mod uplc_print;
pub mod uplc_turbo;
