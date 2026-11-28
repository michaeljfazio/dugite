//! The `ledger-rules` scenario runner (#1125, Phase 2b of #1114).
//!
//! 368 phase-one transaction fixtures (PV9/PV10, all preprod).  For each one this module builds a
//! real `LedgerState` from the fixture's `initial_state`, runs dugite's own LocalTxSubmission
//! validation path against it, and names the failure from the `ApplyTxErr` BYTES dugite would
//! send, never from a hand-written table of dugite's error variants.
//!
//! Three properties make the gate non-vacuous; each is closed by construction:
//!
//! 1. **Names come from bytes.**  [`run::run_fixture`] converts the ledger's failures with the
//!    node's own conversion ([`dugite_tx_admission`]), encodes them with the node's own encoder,
//!    and [`decode`] reads the result through [`tag_table`], which is transcribed from
//!    cardano-ledger.  The generic `ConwayMempoolFailure` fallback has no name: it is a failure.
//! 2. **The context is the ledger's.**  [`state`] populates a `LedgerState` and the runner asks it
//!    for `mempool_validation_context()`.  A test asserts which context fields are `None`, and a
//!    negative control drops each `initial_state` section.
//! 3. **Expectations are a contract, not a skip list.**  [`contract`] holds the committed
//!    `(fixture id, observed result)` pairs; the gate fails on an unknown id, on an entry that now
//!    matches, and on an observed result that changed.

pub mod contract;
pub mod decode;
pub mod fixture;
pub mod names;
pub mod ndc_names;
pub mod report;
pub mod run;
pub mod state;
pub mod tag_table;
