//! Phase 4 — Compare: field-by-field state equivalence check.
//!
//! ## Current state (Phase 4 skeleton)
//!
//! Compares `initial_state` and `final_state` by byte length and CBOR top-level
//! shape (using `bridge::DecodedState`). This is the minimum meaningful check
//! that can be made before the full typed bridge is complete.
//!
//! When initial == final (byte-exact) the comparison is vacuously "same" — the
//! runner hasn't mutated anything yet. That is expected in skeleton mode.
//!
//! ## Phase 4 follow-on
//!
//! Once `runner.rs` produces a typed post-event state, replace the byte-length
//! comparison with field-by-field equivalence:
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
/// In Phase 4 skeleton mode: compares CBOR byte length and top-level shape.
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
