//! Phase 4 — Runner: validate ImpSpec vectors against Dugite's engine.
//!
//! ## Current validation (Phase 4)
//!
//! For each vector the runner dispatches on the `rule` field derived from the
//! directory name:
//!
//! - **ConwayNEWEPOCH** (and any `*NEWEPOCH*` rule): reads the initial epoch
//!   number from `st_cbor[0]` and the signal epoch number from `sig_cbor`,
//!   then validates the hard invariant `signal_epoch > initial_epoch`.
//!
//! - **ConwayUTXO** (and any `*UTXO*` rule): calls
//!   `dugite_serialization::decode_transaction(era_id, sig_cbor)` on the
//!   signal file and returns `Failed` if it errors.
//!
//! - **Unknown rules**: returns `Skipped` (no validation available yet).
//!
//! ## Phase 4 follow-on: full ledger replay
//!
//! Full ledger execution (apply_tx / apply_tick / apply_epoch) requires:
//! 1. A `LedgerState::from_cbor(imp_spec_format)` bridge for the `arr[7]`
//!    NewEpochState encoding (tracked as a separate ledger bridge task).
//! 2. Real ImpSpec fixture files generated from the Haskell toolchain.
//!
//! Until both are available the runner provides the deepest validation
//! achievable without that bridge.

use dugite_serialization::decode_transaction;

use crate::upstream::ledger_rules_replay::{
    bridge::{decode_epoch_no, decode_initial_epoch_no, decode_new_epoch_state},
    vector::ImpVector,
};

/// Outcome of validating one ImpSpec vector.
#[derive(Debug)]
pub enum RunOutcome {
    /// NEWEPOCH invariant validated: signal_epoch > initial_epoch.
    NewEpochValidated {
        initial_epoch: u64,
        signal_epoch: u64,
        /// Initial treasury (lovelace) from AccountState, or `None` if
        /// structural decode of the full NewEpochState failed.
        treasury: Option<u64>,
        /// Initial reserves (lovelace) from AccountState.
        reserves: Option<u64>,
    },
    /// UTXO signal decoded successfully as a transaction.
    UtxoDecoded { era_id: u16, tx_bytes: usize },
    /// Vector was skipped because no rule handler is implemented yet.
    Skipped { reason: String },
    /// Validation failed.
    Failed { detail: String },
}

/// Derive a Cardano HFC `era_id` from a rule name prefix.
///
/// Rule names follow the pattern `<Era><RULENAME>`, e.g. "ConwayNEWEPOCH",
/// "BabbageUTXO", "ShelleyNEWEPOCH". The era prefix determines the CBOR
/// decoder used for Transaction signals.
fn era_id_from_rule(rule: &str) -> u16 {
    if rule.starts_with("Conway") || rule.starts_with("CONWAY") {
        6
    } else if rule.starts_with("Babbage") || rule.starts_with("BABBAGE") {
        5
    } else if rule.starts_with("Alonzo") || rule.starts_with("ALONZO") {
        4
    } else if rule.starts_with("Mary") || rule.starts_with("MARY") {
        3
    } else if rule.starts_with("Allegra") || rule.starts_with("ALLEGRA") {
        2
    } else if rule.starts_with("Shelley") || rule.starts_with("SHELLEY") {
        1
    } else {
        6 // default Conway
    }
}

/// Validate `vec` according to its rule and return the outcome.
///
/// The `era_id` used for UTXO tx decoding is derived from the rule name.
pub fn run_vector(vec: &ImpVector) -> RunOutcome {
    let rule = vec.rule.as_str();

    if rule.contains("NEWEPOCH") || rule.contains("NewEpoch") {
        run_newepoch(vec)
    } else if rule.contains("UTXO") || rule.contains("Utxo") {
        run_utxo(vec)
    } else {
        RunOutcome::Skipped {
            reason: format!("no handler for rule '{rule}'"),
        }
    }
}

/// Validate a NEWEPOCH vector.
///
/// Invariant: `signal_epoch > initial_epoch`.
///
/// The signal for NEWEPOCH is a bare CBOR u64 (target epoch number).
/// The initial epoch number is field [0] of the NewEpochState `array(7)`.
///
/// Additionally, `decode_new_epoch_state` is called on the state blob to
/// extract treasury + reserves from `AccountState`.  A failure here does NOT
/// fail the test — only the epoch-invariant check is gating.  The
/// treasury/reserves are included in the PASS message for diagnostic value
/// when real ImpSpec fixtures arrive.
fn run_newepoch(vec: &ImpVector) -> RunOutcome {
    let initial_epoch = match decode_initial_epoch_no(&vec.st_cbor) {
        Ok(n) => n,
        Err(e) => {
            return RunOutcome::Failed {
                detail: format!("st_cbor decode (initial epoch_no): {e}"),
            }
        }
    };

    let signal_epoch = match decode_epoch_no(&vec.sig_cbor) {
        Ok(n) => n,
        Err(e) => {
            return RunOutcome::Failed {
                detail: format!("sig_cbor decode (signal epoch_no): {e}"),
            }
        }
    };

    if signal_epoch <= initial_epoch {
        return RunOutcome::Failed {
            detail: format!(
                "NEWEPOCH invariant violated: signal_epoch ({signal_epoch}) \
                 must be > initial_epoch ({initial_epoch})"
            ),
        };
    }

    // Best-effort: decode the full NewEpochState to extract treasury/reserves.
    // Failure here is non-fatal — the epoch-invariant is the gating check.
    let (treasury, reserves) = match decode_new_epoch_state(&vec.st_cbor) {
        Ok(nes) => (Some(nes.treasury), Some(nes.reserves)),
        Err(e) => {
            eprintln!("[ledger-rules] WARN decode_new_epoch_state (non-fatal): {e}");
            (None, None)
        }
    };

    RunOutcome::NewEpochValidated {
        initial_epoch,
        signal_epoch,
        treasury,
        reserves,
    }
}

/// Validate a UTXO vector by decoding the signal as a transaction.
fn run_utxo(vec: &ImpVector) -> RunOutcome {
    let era_id = era_id_from_rule(&vec.rule);
    match decode_transaction(era_id, &vec.sig_cbor) {
        Ok(_tx) => RunOutcome::UtxoDecoded {
            era_id,
            tx_bytes: vec.sig_cbor.len(),
        },
        Err(e) => RunOutcome::Failed {
            detail: format!(
                "UTXO sig tx decode failed (era_id={era_id}, {} bytes): {e}",
                vec.sig_cbor.len()
            ),
        },
    }
}
