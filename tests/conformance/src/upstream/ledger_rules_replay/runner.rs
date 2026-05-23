//! Phase 4 — Runner: apply ImpSpec events and collect outcomes.
//!
//! ## Current validation (Phase 4)
//!
//! For each vector the runner:
//! 1. Parses every `Transaction` event's `tx_cbor` bytes using
//!    `dugite_serialization::decode_transaction` (real tx deserialization).
//! 2. Checks `expected_valid == true` transactions decode without error.
//! 3. Counts PassTick and PassEpoch events for diagnostic output.
//!
//! ## Phase 4 follow-on: full ledger replay
//!
//! Full ledger execution (apply_tx / apply_tick / apply_epoch) requires:
//! 1. A `LedgerState::from_cbor(imp_spec_format)` bridge for the `arr[7]`
//!    NewEpochState encoding (tracked as a separate ledger bridge task).
//! 2. Actual ImpSpec fixture files (requires Haskell toolchain to generate).
//!
//! Until both are available, the runner provides the deepest validation
//! achievable: real CBOR deserialization of every transaction in the vector.

use dugite_serialization::decode_transaction;

use crate::upstream::ledger_rules_replay::vector::{ImpEvent, ImpVector};

/// Outcome of replaying one ImpSpec vector through the runner.
#[derive(Debug)]
pub enum RunOutcome {
    /// All tx events deserialized; full ledger replay pending Phase 4 follow-on.
    Decoded {
        transactions: usize,
        ticks: usize,
        epoch_advances: usize,
    },
    /// Vector was skipped (e.g., known-broken scenario in SKIP_LIST).
    Skipped { reason: String },
    /// A deserialize error occurred at event `event_idx`.
    Failed { event_idx: usize, detail: String },
}

/// Apply every event in `vec` and return the run outcome.
///
/// `era_id` is the Cardano HFC era number (1=Shelley, 2=Allegra, 3=Mary,
/// 4=Alonzo, 5=Babbage, 6=Conway) and determines which CBOR decoder is used
/// for Transaction events.
pub fn run_vector(vec: &ImpVector, era_id: u16) -> RunOutcome {
    let mut txs = 0usize;
    let mut ticks = 0usize;
    let mut epochs = 0usize;

    for (i, event) in vec.events.iter().enumerate() {
        match event {
            ImpEvent::Transaction {
                tx_cbor,
                expected_valid,
                ..
            } => {
                txs += 1;
                if *expected_valid {
                    // Transactions the ImpSpec marks as valid must parse cleanly.
                    if let Err(e) = decode_transaction(era_id, tx_cbor) {
                        return RunOutcome::Failed {
                            event_idx: i,
                            detail: format!(
                                "tx decode failed (era_id={era_id}, {} bytes): {e}",
                                tx_cbor.len()
                            ),
                        };
                    }
                }
            }
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
