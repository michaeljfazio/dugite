//! Top-level error type for dugite-uplc.
//!
//! Every fallible entry point in the crate returns `Result<T, UplcError>`.
//! No public API panics on malformed or adversarial input — see the
//! crate-level docs.

use thiserror::Error;

/// All errors that can be produced anywhere in dugite-uplc.
///
/// Variants are intentionally fine-grained so the caller can distinguish
/// "we ran out of budget" from "the script bytes did not decode" from
/// "the redeemer returned an error term" — each has different semantics
/// in cardano-ledger's phase-2 validation rules.
#[derive(Debug, Error)]
pub enum UplcError {
    /// Wire-format / flat-decoder error (truncated input, invalid tags,
    /// unknown builtin id, depth limit exceeded, etc.).
    #[error("flat decode error: {0}")]
    FlatDecode(String),

    /// CBOR-level decode error for the outer `Program` envelope or for
    /// PlutusData payloads.
    #[error("cbor decode error: {0}")]
    CborDecode(String),

    /// Encoder error (we never expect to hit one in practice, but the
    /// signature is `Result` to keep the API uniform).
    #[error("encode error: {0}")]
    Encode(String),

    /// The CEK machine evaluated a `Term::Error` term — the script
    /// signalled failure deterministically.
    #[error("script returned Error term")]
    ScriptError,

    /// The CEK machine ran out of `ExBudget` before the script completed.
    #[error("budget exhausted: cpu_remaining={cpu_remaining}, mem_remaining={mem_remaining}")]
    BudgetExhausted {
        cpu_remaining: i64,
        mem_remaining: i64,
    },

    /// A builtin function was applied to an argument of the wrong type
    /// or shape (e.g. `addInteger` applied to a `ByteString`).
    #[error("builtin {builtin}: type error: {reason}")]
    BuiltinTypeError {
        builtin: &'static str,
        reason: String,
    },

    /// A builtin function failed in a builtin-defined way (e.g. division
    /// by zero, ed25519 signature verification failure, BLS curve
    /// point not on subgroup).
    #[error("builtin {builtin}: {reason}")]
    BuiltinFailure {
        builtin: &'static str,
        reason: String,
    },

    /// PlutusV3 only: the script returned a value other than `Unit`.
    #[error("PlutusV3 script returned non-Unit value")]
    NonUnitReturn,

    /// An unbound de Bruijn variable was found by the eager `checkScope`
    /// pass run over the fully-applied term before CEK evaluation
    /// starts (mirrors Haskell
    /// `UntypedPlutusCore.Check.Scope.checkScope` / its
    /// `FreeVariableError`). This is a phase-2 script-evaluation
    /// failure — collateral is consumed — not an internal bug: an
    /// adversary can construct a script whose applied term contains an
    /// out-of-scope variable in a never-dynamically-evaluated branch,
    /// and Haskell rejects it statically regardless of reachability.
    #[error("free variable: de Bruijn index {0} is unbound")]
    FreeVariable(u64),

    /// Internal invariant violated; this indicates a bug in dugite-uplc
    /// itself, not in the input. Such errors are still returned (not
    /// panicked) so they can be surfaced cleanly in tests.
    #[error("internal: {0}")]
    Internal(String),
}
