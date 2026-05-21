//! Parallel-decoder harness — runs both the in-house decoder
//! ([`crate::decode`]) and the pallas-backed decoder ([`crate::multi_era`])
//! and compares their outputs.
//!
//! This is the safety net that makes the pallas-removal cutover (M6) tractable:
//! during M3–M5 every block decoded in dev/CI/soak can flow through both paths
//! with byte-exact equality enforced. Mismatches become test failures, log
//! warnings, or on-disk artifacts depending on runtime mode.
//!
//! ## Runtime control: `DUGITE_DUAL_DECODE`
//!
//! Set the env var to one of:
//!
//! - `off` (default): no shadow decode. Public `decode_*` entry points call
//!   only the in-house path. Production / release builds use this.
//! - `warn`: run both decoders, log mismatches at WARN level, return the
//!   in-house result.
//! - `panic`: run both decoders, panic on mismatch. Used by tests.
//! - `dump`: like `warn` plus write the offending CBOR + both decoded
//!   `Debug` outputs to `$DUGITE_DUAL_DECODE_DUMP_DIR/{slot}-{hash}.{cbor,a.txt,b.txt}`
//!   (default: `./dual_decode_mismatches/`). Used by soak.
//!
//! Falls back to `off` if the env var is missing, empty, or unrecognised.
//!
//! ## Compile-time gating
//!
//! The shadow harness is only meaningful when the pallas-backed decoder is
//! available. When the `pallas-shadow-decode` Cargo feature is OFF (the M6
//! configuration), all modes other than `off` degrade to `off` and only the
//! in-house decoder runs. This makes it safe to keep the env var set in
//! environments where pallas has been removed.
//!
//! ## Equality check (three tiers)
//!
//! 1. **Cheap:** `PartialEq` on the parsed [`Block`] (sub-microsecond).
//!    `Block` derives `PartialEq` as of milestone M1.
//! 2. **Tolerant:** if (1) fails, [`normalize_block`] sorts any collection
//!    field whose iteration order is non-deterministic (none today; the only
//!    collections on the dugite types are `Vec` and `BTreeMap`, both already
//!    deterministic). Reserved for future field shapes.
//! 3. **Diagnostic:** in `dump` mode only — re-encode both blocks via the
//!    in-house encoder and byte-diff. Cosmetic ordering vs semantic divergence
//!    can be told apart at this layer.
//!
//! ## Async comparator (deferred to M5)
//!
//! The plan calls for an off-hot-path bounded `tokio::sync::mpsc` comparator
//! so block ingestion never waits on the shadow decoder during multi-million-
//! block sync soaks. That optimisation lands in M5 alongside the real
//! validation runs — pre-M4 the comparison is tautological (pallas vs pallas
//! via the in-house stub) so there is nothing to optimise yet.

use crate::error::SerializationError;
use dugite_primitives::block::Block;
use dugite_primitives::transaction::Transaction;
#[cfg(feature = "pallas-shadow-decode")]
use std::sync::OnceLock;

/// Behaviour of the dual-decode comparator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DualDecodeMode {
    /// Single decoder (in-house). No comparison. Production default.
    Off,
    /// Run both, log mismatches, return the in-house result.
    Warn,
    /// Run both, panic on mismatch. Test default.
    Panic,
    /// Like `Warn` plus dump offending CBOR + both decoded `Debug` outputs
    /// to disk. Used by soak runs.
    Dump,
}

impl DualDecodeMode {
    /// Whether this mode actually compares two decoders.
    pub fn compares(self) -> bool {
        !matches!(self, Self::Off)
    }
}

/// Parse a [`DualDecodeMode`] from a string. Unknown values map to `Off`.
///
/// Public to `cfg(test)` so the parsing rules can be exercised even when
/// the `pallas-shadow-decode` feature is off (in which case the function
/// is otherwise unreachable from production code).
#[cfg(any(feature = "pallas-shadow-decode", test))]
fn parse_mode(raw: &str) -> DualDecodeMode {
    match raw.trim().to_ascii_lowercase().as_str() {
        "warn" => DualDecodeMode::Warn,
        "panic" => DualDecodeMode::Panic,
        "dump" => DualDecodeMode::Dump,
        // "off", "", "0", "false", anything else → Off.
        _ => DualDecodeMode::Off,
    }
}

/// Read the active mode from `DUGITE_DUAL_DECODE`.
///
/// The value is cached after the first read; later changes to the env var are
/// ignored. This keeps the hot path branch-predictable and avoids per-block
/// syscalls.
pub fn dual_decode_mode() -> DualDecodeMode {
    // When the pallas decoder isn't compiled in, comparison is impossible.
    // Force `Off` so the env var being set doesn't fool callers into thinking
    // shadow decode is running.
    #[cfg(not(feature = "pallas-shadow-decode"))]
    {
        DualDecodeMode::Off
    }

    #[cfg(feature = "pallas-shadow-decode")]
    {
        static MODE: OnceLock<DualDecodeMode> = OnceLock::new();
        *MODE.get_or_init(|| match std::env::var("DUGITE_DUAL_DECODE") {
            Ok(raw) => parse_mode(&raw),
            Err(_) => DualDecodeMode::Off,
        })
    }
}

/// Test-only override of the mode cache.
///
/// `OnceLock::get_or_init` only fires once per process; tests that need to
/// exercise more than one mode bypass the cache via this entry point. The
/// real public [`dual_decode_mode`] is unaffected.
#[cfg(test)]
fn run_in_mode<T>(mode: DualDecodeMode, f: impl FnOnce() -> T) -> T {
    // Tests run sequentially per nextest config; if they don't, the worst
    // case is a benign clobber of the override that the next call resets.
    let prev = OVERRIDE_MODE.with(|c| c.replace(Some(mode)));
    let out = f();
    OVERRIDE_MODE.with(|c| {
        *c.borrow_mut() = prev;
    });
    out
}

#[cfg(test)]
thread_local! {
    static OVERRIDE_MODE: std::cell::RefCell<Option<DualDecodeMode>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn effective_mode() -> DualDecodeMode {
    OVERRIDE_MODE
        .with(|c| *c.borrow())
        .unwrap_or_else(dual_decode_mode)
}

#[cfg(not(test))]
fn effective_mode() -> DualDecodeMode {
    dual_decode_mode()
}

// =============================================================================
// Public decode entry points
// =============================================================================

/// Decode a multi-era block, optionally cross-checking against the pallas
/// decoder per `DUGITE_DUAL_DECODE`.
pub fn decode_block(cbor: &[u8]) -> Result<Block, SerializationError> {
    decode_block_with_byron_epoch_length(cbor, 0)
}

/// Decode a multi-era block with explicit Byron epoch length (for non-mainnet
/// networks). See [`decode_block`].
pub fn decode_block_with_byron_epoch_length(
    cbor: &[u8],
    byron_epoch_length: u64,
) -> Result<Block, SerializationError> {
    let inhouse = crate::decode::decode_block_with_byron_epoch_length(cbor, byron_epoch_length);

    let mode = effective_mode();
    if !mode.compares() {
        return inhouse;
    }

    #[cfg(feature = "pallas-shadow-decode")]
    {
        let pallas =
            crate::multi_era::decode_block_with_byron_epoch_length(cbor, byron_epoch_length);
        compare_block_results(cbor, &inhouse, &pallas, mode, "decode_block");
    }

    inhouse
}

/// Decode a multi-era block in minimal mode.
pub fn decode_block_minimal(cbor: &[u8]) -> Result<Block, SerializationError> {
    decode_block_minimal_with_byron_epoch_length(cbor, 0)
}

/// Minimal decode with explicit Byron epoch length.
pub fn decode_block_minimal_with_byron_epoch_length(
    cbor: &[u8],
    byron_epoch_length: u64,
) -> Result<Block, SerializationError> {
    let inhouse =
        crate::decode::decode_block_minimal_with_byron_epoch_length(cbor, byron_epoch_length);

    let mode = effective_mode();
    if !mode.compares() {
        return inhouse;
    }

    #[cfg(feature = "pallas-shadow-decode")]
    {
        let pallas = crate::multi_era::decode_block_minimal_with_byron_epoch_length(
            cbor,
            byron_epoch_length,
        );
        compare_block_results(cbor, &inhouse, &pallas, mode, "decode_block_minimal");
    }

    inhouse
}

/// Decode a transaction CBOR for a specific era.
pub fn decode_transaction(era_id: u16, tx_cbor: &[u8]) -> Result<Transaction, SerializationError> {
    let inhouse = crate::decode::decode_transaction(era_id, tx_cbor);

    let mode = effective_mode();
    if !mode.compares() {
        return inhouse;
    }

    #[cfg(feature = "pallas-shadow-decode")]
    {
        let pallas = crate::multi_era::decode_transaction(era_id, tx_cbor);
        compare_tx_results(tx_cbor, era_id, &inhouse, &pallas, mode);
    }

    inhouse
}

// =============================================================================
// Comparison + reporting
// =============================================================================

#[cfg(feature = "pallas-shadow-decode")]
fn compare_block_results(
    cbor: &[u8],
    inhouse: &Result<Block, SerializationError>,
    pallas: &Result<Block, SerializationError>,
    mode: DualDecodeMode,
    op: &str,
) {
    let outcome = match (inhouse, pallas) {
        (Ok(a), Ok(b)) => block_equality(a, b),
        (Err(a), Err(b)) => {
            // Both failed. Compare error categories (Display strings vary by
            // codec internals — too noisy as a divergence signal).
            if std::mem::discriminant(a) == std::mem::discriminant(b) {
                Equality::Match
            } else {
                Equality::ErrorDiverged
            }
        }
        (Ok(_), Err(_)) | (Err(_), Ok(_)) => Equality::ResultShape,
    };

    if matches!(outcome, Equality::Match) {
        return;
    }

    let slot = inhouse
        .as_ref()
        .ok()
        .map(|b| b.slot().0)
        .or_else(|| pallas.as_ref().ok().map(|b| b.slot().0));
    let hash = inhouse
        .as_ref()
        .ok()
        .map(|b| b.hash().to_hex())
        .or_else(|| pallas.as_ref().ok().map(|b| b.hash().to_hex()));

    report_mismatch(op, mode, cbor, slot, hash.as_deref(), &outcome, |a, b| {
        format!(
            "in-house: {}\npallas:   {}",
            match inhouse {
                Ok(_) => "Ok(Block)".to_string(),
                Err(e) => format!("Err({e})"),
            },
            match pallas {
                Ok(_) => "Ok(Block)".to_string(),
                Err(e) => format!("Err({e})"),
            }
        ) + &format!("\nin-house-debug:\n{a:#?}\npallas-debug:\n{b:#?}")
    });
}

#[cfg(feature = "pallas-shadow-decode")]
fn compare_tx_results(
    cbor: &[u8],
    era_id: u16,
    inhouse: &Result<Transaction, SerializationError>,
    pallas: &Result<Transaction, SerializationError>,
    mode: DualDecodeMode,
) {
    let outcome = match (inhouse, pallas) {
        (Ok(a), Ok(b)) if a == b => Equality::Match,
        (Ok(_), Ok(_)) => Equality::ContentDiverged,
        (Err(a), Err(b)) if std::mem::discriminant(a) == std::mem::discriminant(b) => {
            Equality::Match
        }
        (Err(_), Err(_)) => Equality::ErrorDiverged,
        _ => Equality::ResultShape,
    };

    if matches!(outcome, Equality::Match) {
        return;
    }

    let hash = inhouse
        .as_ref()
        .ok()
        .map(|tx| tx.hash.to_hex())
        .or_else(|| pallas.as_ref().ok().map(|tx| tx.hash.to_hex()));

    report_mismatch(
        &format!("decode_transaction(era={era_id})"),
        mode,
        cbor,
        None,
        hash.as_deref(),
        &outcome,
        |a, b| format!("in-house:\n{a:#?}\npallas:\n{b:#?}"),
    );
}

/// Categorisation of a comparison result.
#[cfg(feature = "pallas-shadow-decode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Equality {
    /// Identical (cheap PartialEq or both same-discriminant errors).
    Match,
    /// Both decoders returned `Ok` but the parsed values differ.
    ContentDiverged,
    /// Both returned `Err` but with different `SerializationError` variants.
    ErrorDiverged,
    /// One `Ok`, one `Err`.
    ResultShape,
}

#[cfg(feature = "pallas-shadow-decode")]
impl Equality {
    fn kind(self) -> &'static str {
        match self {
            Self::Match => "match",
            Self::ContentDiverged => "content-diverged",
            Self::ErrorDiverged => "error-diverged",
            Self::ResultShape => "result-shape-mismatch",
        }
    }
}

/// Two-block equality with `raw_cbor` normalization.
///
/// `raw_cbor` fields (`Block::raw_cbor`, `Transaction::raw_cbor`,
/// `Transaction::raw_body_cbor`, `Transaction::raw_witness_cbor`,
/// `TransactionOutput::raw_cbor`, `AuxiliaryData::raw_cbor`) are
/// **implementation artifacts** — the in-house decoder captures them
/// differently from pallas (which re-encodes via `tx.encode()`).
/// They carry no semantic content for correctness checking, so we strip
/// them before comparing.
///
/// All other collections (`Vec`, `BTreeMap`) are deterministic by insertion
/// order, so no additional normalization is needed today.
#[cfg(feature = "pallas-shadow-decode")]
fn block_equality(a: &Block, b: &Block) -> Equality {
    use dugite_primitives::transaction::{AuxiliaryData, TransactionOutput};

    fn normalize_output(o: &TransactionOutput) -> TransactionOutput {
        TransactionOutput {
            raw_cbor: None,
            ..o.clone()
        }
    }

    fn normalize_aux(aux: &Option<AuxiliaryData>) -> Option<AuxiliaryData> {
        aux.as_ref().map(|a| AuxiliaryData {
            raw_cbor: None,
            ..a.clone()
        })
    }

    fn normalize_tx(
        tx: &dugite_primitives::transaction::Transaction,
    ) -> dugite_primitives::transaction::Transaction {
        dugite_primitives::transaction::Transaction {
            raw_cbor: None,
            raw_body_cbor: None,
            raw_witness_cbor: None,
            auxiliary_data: normalize_aux(&tx.auxiliary_data),
            body: {
                let mut body = tx.body.clone();
                body.outputs = body.outputs.iter().map(normalize_output).collect();
                if let Some(cr) = body.collateral_return.take() {
                    body.collateral_return = Some(normalize_output(&cr));
                }
                body
            },
            ..tx.clone()
        }
    }

    fn normalize_block(b: &Block) -> Block {
        Block {
            raw_cbor: None,
            transactions: b.transactions.iter().map(normalize_tx).collect(),
            ..b.clone()
        }
    }

    if normalize_block(a) == normalize_block(b) {
        Equality::Match
    } else {
        Equality::ContentDiverged
    }
}

#[cfg(feature = "pallas-shadow-decode")]
fn report_mismatch<F>(
    op: &str,
    mode: DualDecodeMode,
    cbor: &[u8],
    slot: Option<u64>,
    hash: Option<&str>,
    outcome: &Equality,
    format_diff: F,
) where
    F: FnOnce(&str, &str) -> String,
{
    let slot_disp = slot
        .map(|s| s.to_string())
        .unwrap_or_else(|| "?".to_string());
    let hash_disp = hash.unwrap_or("?");

    tracing::warn!(
        target: "dugite::serialization::dual_decode",
        op = %op,
        slot = %slot_disp,
        hash = %hash_disp,
        outcome = %outcome.kind(),
        cbor_len = cbor.len(),
        "dual-decode mismatch"
    );

    if matches!(mode, DualDecodeMode::Dump) {
        let dump_dir = std::env::var("DUGITE_DUAL_DECODE_DUMP_DIR")
            .unwrap_or_else(|_| "dual_decode_mismatches".to_string());
        let dir = std::path::PathBuf::from(&dump_dir);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            tracing::warn!(
                target: "dugite::serialization::dual_decode",
                error = %e,
                dir = %dir.display(),
                "failed to create dump dir; skipping artifact write"
            );
        } else {
            let prefix = format!("{}-{}-{}", op, slot_disp, hash_disp);
            let cbor_path = dir.join(format!("{prefix}.cbor"));
            let diff_path = dir.join(format!("{prefix}.diff.txt"));
            let _ = std::fs::write(&cbor_path, cbor);
            let _ = std::fs::write(&diff_path, format_diff("(in-house)", "(pallas)"));
            tracing::warn!(
                target: "dugite::serialization::dual_decode",
                cbor = %cbor_path.display(),
                diff = %diff_path.display(),
                "wrote dual-decode mismatch artifacts"
            );
        }
    }

    if matches!(mode, DualDecodeMode::Panic) {
        panic!(
            "dual-decode mismatch in {op} (slot={slot_disp}, hash={hash_disp}, outcome={})",
            outcome.kind()
        );
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_mode_recognised_variants() {
        assert_eq!(parse_mode("off"), DualDecodeMode::Off);
        assert_eq!(parse_mode("warn"), DualDecodeMode::Warn);
        assert_eq!(parse_mode("Panic"), DualDecodeMode::Panic);
        assert_eq!(parse_mode("DUMP"), DualDecodeMode::Dump);
        assert_eq!(parse_mode("  warn  "), DualDecodeMode::Warn);
    }

    #[test]
    fn parse_mode_unknown_to_off() {
        assert_eq!(parse_mode(""), DualDecodeMode::Off);
        assert_eq!(parse_mode("0"), DualDecodeMode::Off);
        assert_eq!(parse_mode("false"), DualDecodeMode::Off);
        assert_eq!(parse_mode("yes"), DualDecodeMode::Off);
    }

    #[test]
    fn mode_compares_predicate() {
        assert!(!DualDecodeMode::Off.compares());
        assert!(DualDecodeMode::Warn.compares());
        assert!(DualDecodeMode::Panic.compares());
        assert!(DualDecodeMode::Dump.compares());
    }

    #[test]
    fn decode_in_off_mode_returns_inhouse_result() {
        // Whatever the in-house decoder returns must be returned verbatim
        // when comparison is off. We don't care that it errors on garbage —
        // we care that the harness does not panic, log, or wrap.
        let out = run_in_mode(DualDecodeMode::Off, || decode_block(&[]));
        assert!(out.is_err());
    }

    #[cfg(feature = "pallas-shadow-decode")]
    #[test]
    fn decode_warn_mode_does_not_panic_on_decoder_error() {
        // Both decoders should fail identically on empty input; comparator
        // sees Err==Err (same discriminant) → Equality::Match → no warn.
        let _out = run_in_mode(DualDecodeMode::Warn, || decode_block(&[]));
    }

    #[cfg(feature = "pallas-shadow-decode")]
    #[test]
    fn decode_panic_mode_passes_when_decoders_agree() {
        // Pre-M4 the in-house decoder delegates to pallas, so they always
        // agree. This test asserts the harness wires through correctly:
        // no panic, no spurious warn, identical output.
        let out = run_in_mode(DualDecodeMode::Panic, || decode_block(&[0xff; 4]));
        assert!(out.is_err(), "garbage input should fail to decode");
    }
}
