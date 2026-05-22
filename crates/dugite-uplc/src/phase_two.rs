//! Phase-2 transaction evaluator — the entry point that
//! [`dugite_ledger::plutus`] calls per tx to validate every Plutus
//! redeemer in the witness set.
//!
//! ## API shape
//!
//! `eval_phase_two_raw` mirrors the function signature from
//! aiken-lang/uplc that dugite-ledger currently invokes, so the
//! ledger-side switch from aiken-uplc to dugite-uplc is a one-line
//! import change. The signature is:
//!
//! ```text
//! fn eval_phase_two_raw(
//!     tx_cbor: &[u8],
//!     utxos: &[(Vec<u8>, Vec<u8>)],     // (input_cbor, output_cbor) pairs
//!     cost_models_cbor: Option<&[u8]>,
//!     initial_budget: (u64, u64),       // (cpu, mem)
//!     slot_config: SlotConfig,
//!     run_phase_one: bool,
//!     with_redeemer: impl FnMut(&Redeemer),
//! ) -> Result<Vec<RedeemerResult>, PhaseTwoError>;
//! ```
//!
//! ## Implementation status
//!
//! The full byte-exact phase-2 evaluator requires:
//!
//! - Decoding tx + UTxO map (have via `dugite-serialization`)
//! - Building the per-version `TxInfo` from those (V1/V2 = subset of
//!   V3; V3 lands here)
//! - For each redeemer: resolve script + datum, build ScriptContext,
//!   encode to Data, apply args, evaluate via CEK with budget tracker,
//!   and record consumed ExUnits.
//!
//! This module currently lands the **API surface** + a `tx_info`
//! builder skeleton. The end-to-end wire-up arrives in follow-on
//! commits so callers can switch their import once and progress
//! happens incrementally underneath. Calling [`eval_phase_two_raw`]
//! today returns [`PhaseTwoError::NotImplemented`].
//!
//! Once the full path lands, dugite-ledger drops the
//! `uplc = { git = aiken-lang/aiken.git }` workspace dep and the
//! transitive `pallas-*` chain comes with it.

use crate::machine::cost::ExBudget;

/// Slot config — `(network_start_unix_seconds, slot_zero_offset,
/// slot_length_ms)`. Mirrors the Cardano `SlotConfig` used to
/// translate slots ↔ POSIX time for `txValidRange` in TxInfo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotConfig {
    pub network_start_unix_seconds: u64,
    pub slot_zero_offset: u64,
    pub slot_length_ms: u32,
}

/// Per-redeemer evaluation result. Returned by
/// [`eval_phase_two_raw`] for every successful redeemer evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedeemerResult {
    /// The redeemer's raw CBOR bytes (round-tripped from the tx for
    /// the caller's logging convenience).
    pub redeemer_cbor: Vec<u8>,
    /// The ExUnits consumed (cpu, mem). Matches the units the ledger
    /// charges as part of script-execution fees.
    pub consumed: ExBudget,
}

/// All failure modes the phase-2 evaluator can surface to the
/// ledger. Mirrors the typed taxonomy from aiken-uplc.
#[derive(Debug, thiserror::Error)]
pub enum PhaseTwoError {
    /// The evaluator is wired into the dependency graph but the
    /// per-version `TxInfo` builder and CEK glue have not yet
    /// landed. dugite-ledger should keep calling aiken-uplc until
    /// this variant disappears.
    #[error(
        "phase-2 evaluator not yet fully implemented (see crates/dugite-uplc/src/phase_two.rs)"
    )]
    NotImplemented,
    /// Failure decoding the tx CBOR.
    #[error("tx decode failed: {0}")]
    TxDecode(String),
    /// Failure decoding a UTxO entry.
    #[error("utxo decode failed: {0}")]
    UtxoDecode(String),
    /// Failure decoding cost models.
    #[error("cost model decode failed: {0}")]
    CostModelDecode(String),
    /// Script not found for a redeemer's purpose.
    #[error("script not found for redeemer purpose: {0}")]
    MissingScript(String),
    /// Datum not found for a V1/V2 spending redeemer.
    #[error("datum not found for V1/V2 spending redeemer: {hash}")]
    MissingDatum { hash: String },
    /// CEK evaluation failed.
    #[error("script evaluation failed: {0}")]
    ScriptEvaluationFailed(#[from] crate::UplcError),
    /// Generic internal error.
    #[error("internal phase-2 error: {0}")]
    Internal(String),
}

/// The redeemer trait callers implement to observe each redeemer
/// during evaluation (used by aiken-uplc to surface debug info).
/// We provide a no-op implementation by default so callers without
/// observation needs can pass `()`.
pub trait RedeemerObserver {
    fn on_redeemer(&mut self, redeemer_cbor: &[u8]);
}

impl RedeemerObserver for () {
    fn on_redeemer(&mut self, _redeemer_cbor: &[u8]) {}
}

/// Evaluate every Plutus redeemer in `tx_cbor` against the supplied
/// `utxos` map.
///
/// Returns one [`RedeemerResult`] per redeemer in the order the
/// redeemers appear in the tx witness set. If any redeemer fails,
/// the function returns `Err` immediately and does not continue to
/// subsequent redeemers (matches aiken-uplc's fail-fast semantics).
///
/// `run_phase_one` controls whether the function additionally
/// re-runs Phase-1 (structural) validation as a safety net before
/// any CEK invocation; dugite-ledger calls this with `true`.
///
/// `observer` is invoked once per redeemer with its raw CBOR bytes,
/// before the CEK evaluation. Pass `()` if you don't need this.
pub fn eval_phase_two_raw<O: RedeemerObserver>(
    _tx_cbor: &[u8],
    _utxos: &[(Vec<u8>, Vec<u8>)],
    _cost_models_cbor: Option<&[u8]>,
    _initial_budget: (u64, u64),
    _slot_config: SlotConfig,
    _run_phase_one: bool,
    _observer: &mut O,
) -> Result<Vec<RedeemerResult>, PhaseTwoError> {
    // Wire-up checklist before this returns Ok(...):
    //   1. Decode tx via dugite-serialization
    //   2. Decode UTxO entries
    //   3. Parse cost models per Plutus version
    //   4. Build per-version TxInfo
    //   5. For each redeemer:
    //      a. Resolve script (witness set or reference input)
    //      b. Resolve datum (V1/V2 only)
    //      c. Build ScriptContext + encode to Data
    //      d. Apply args + evaluate via CEK with budget tracker
    //      e. Push RedeemerResult
    Err(PhaseTwoError::NotImplemented)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_returns_not_implemented() {
        let result = eval_phase_two_raw(
            &[],
            &[],
            None,
            (10_000_000, 10_000_000),
            SlotConfig {
                network_start_unix_seconds: 1_596_491_091,
                slot_zero_offset: 4_492_800,
                slot_length_ms: 1_000,
            },
            true,
            &mut (),
        );
        assert!(matches!(result, Err(PhaseTwoError::NotImplemented)));
    }

    #[test]
    fn unit_redeemer_observer_is_no_op() {
        let mut obs = ();
        obs.on_redeemer(&[0xde, 0xad]);
    }

    #[test]
    fn slot_config_is_copy() {
        let sc = SlotConfig {
            network_start_unix_seconds: 0,
            slot_zero_offset: 0,
            slot_length_ms: 1000,
        };
        let _sc2 = sc; // Copy
        let _sc3 = sc; // still usable
    }
}
