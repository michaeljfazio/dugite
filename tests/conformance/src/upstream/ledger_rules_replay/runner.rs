//! Phase 4 — Runner: apply ImpSpec events and collect outcomes.
//!
//! ## Current state (Phase 4 skeleton)
//!
//! The runner validates that every event in a vector decodes correctly and
//! records which event types appear. The actual ledger calls
//! (`Ledger::apply_tx`, `Ledger::apply_tick`, `Ledger::apply_epoch`) are
//! wired up as Phase 4 follow-on work once the CBOR bridge from
//! `bridge.rs` is complete enough to produce Dugite's internal types.
//!
//! ## Phase 4 follow-on
//!
//! Replace `RunOutcome::Decoded` with full ledger execution:
//! 1. Use `bridge::decode_state` to initialise a `LedgerState`.
//! 2. For each `ImpEvent::Transaction`: call `dugite_ledger::apply_tx`.
//! 3. For each `ImpEvent::PassTick`: advance the slot clock.
//! 4. For each `ImpEvent::PassEpoch`: trigger epoch-boundary logic.
//! 5. Return the resulting state for `compare.rs`.

use crate::upstream::ledger_rules_replay::vector::{ImpEvent, ImpVector};

/// Outcome of replaying one ImpSpec vector through the runner.
#[derive(Debug)]
pub enum RunOutcome {
    /// All events decoded successfully; full ledger replay pending Phase 4 follow-on.
    Decoded {
        transactions: usize,
        ticks: usize,
        epoch_advances: usize,
    },
    /// Vector was skipped (e.g., known-broken scenario in SKIP_LIST).
    Skipped { reason: String },
    /// A ledger error occurred at event `event_idx`.
    Failed { event_idx: usize, detail: String },
}

/// Apply every event in `vec` and return the run outcome.
///
/// In Phase 4 skeleton mode this validates event decoding only. Full ledger
/// replay is wired in the follow-on once `bridge.rs` produces typed state.
pub fn run_vector(vec: &ImpVector) -> RunOutcome {
    let mut txs = 0usize;
    let mut ticks = 0usize;
    let mut epochs = 0usize;

    for event in &vec.events {
        match event {
            ImpEvent::Transaction { .. } => txs += 1,
            ImpEvent::PassTick { .. } => ticks += 1,
            ImpEvent::PassEpoch { .. } => epochs += 1,
        }
    }

    RunOutcome::Decoded {
        transactions: txs,
        ticks,
        epoch_advances: epochs,
    }
}
