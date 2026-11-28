//! Compare: byte/shape equivalence helper for two decoded states.
//!
//! **Nothing calls `compare_states`.**  The runner never produces a Dugite-side
//! state, so no test compares Dugite's output against a vector's `st_out`.  The
//! function is a placeholder for a future ledger check.
//!
//! It compares CBOR byte length and top-level shape (using
//! `bridge::DecodedState`); when both blobs are byte-identical it reports a
//! match.
//!
//! Once a ledger check produces a typed post-event state, replace the
//! byte-length comparison with field-by-field equivalence:
//! - UTxO set contents (input → output pairs)
//! - Certificate state (delegation, registration)
//! - Protocol parameters (current, future)
//! - Reward accounts
//! - Epoch number and slot clock
//!
//! Human-readable diffs should name the mismatched field and both values so
//! that test failures point directly at the ledger bug.

use crate::upstream::ledger_rules_replay::bridge::DecodedState;

/// Result of comparing an observed state against an expected state.
pub struct CompareResult {
    /// True when observed == expected at the current comparison depth.
    pub matches: bool,
    /// Human-readable description of any mismatch (empty when `matches` is true).
    pub diff: String,
}

/// Compare `observed` (post-runner state) against `expected` (vector's final_state).
///
/// Compares CBOR byte length and top-level shape only (no caller yet).
/// Matches when both are equal; otherwise reports the discrepancy.
pub fn compare_states(observed: &DecodedState, expected: &DecodedState) -> CompareResult {
    if observed.raw_cbor == expected.raw_cbor {
        return CompareResult {
            matches: true,
            diff: String::new(),
        };
    }

    let mut diffs = Vec::new();

    if observed.raw_cbor.len() != expected.raw_cbor.len() {
        diffs.push(format!(
            "byte length: observed {} vs expected {}",
            observed.raw_cbor.len(),
            expected.raw_cbor.len()
        ));
    }

    if observed.shape != expected.shape {
        diffs.push(format!(
            "CBOR shape: observed '{}' vs expected '{}'",
            observed.shape, expected.shape
        ));
    }

    if diffs.is_empty() {
        diffs.push(format!(
            "content differs at same length ({} bytes) and same shape ({})",
            observed.raw_cbor.len(),
            observed.shape
        ));
    }

    CompareResult {
        matches: false,
        diff: diffs.join("; "),
    }
}
