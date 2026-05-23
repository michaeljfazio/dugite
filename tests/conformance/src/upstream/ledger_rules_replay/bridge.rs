//! Phase 4 — Bridge: inspect and decode ImpSpec CBOR blobs.
//!
//! Each ImpSpec test case is stored as 4 separate CBOR files:
//!   conformance_dump_ctx.cbor  — ExecContext
//!   conformance_dump_env.cbor  — Environment
//!   conformance_dump_st.cbor   — State (NewEpochState array(7))
//!   conformance_dump_sig.cbor  — Signal (u64 EpochNo for NEWEPOCH, tx CBOR for UTXO)
//!
//! This module provides:
//!
//! 1. `DecodedState` — a lightweight wrapper that carries the raw bytes together
//!    with a human-readable shape description extracted via minicbor.
//!
//! 2. `decode_state(cbor, label)` — entry point for tests; returns a
//!    `DecodedState` or an error string.
//!
//! 3. `decode_epoch_no(sig_cbor)` — reads a CBOR u64 from the signal file
//!    (used for NEWEPOCH rule where the signal is the target epoch number).
//!
//! 4. `decode_initial_epoch_no(st_cbor)` — reads field [0] of the array(7)
//!    NewEpochState, which is the current epoch number before the transition.
//!
//! ## Future work (Phase 4 follow-on)
//!
//! Once `dugite-ledger` exposes a public deserialization API for
//! `NewEpochState` / `LedgerState`, replace the raw-CBOR path with a full
//! typed decode so that `runner.rs` can call `Ledger::apply_tx` directly.

use minicbor::data::Type;

/// A partially-decoded ledger state blob.
///
/// `raw_cbor` is passed through unchanged to `compare.rs`; `shape` is shown
/// in diagnostic output when comparisons fail.
pub struct DecodedState {
    /// Raw CBOR bytes from the dump vector.
    pub raw_cbor: Vec<u8>,
    /// Human-readable top-level CBOR shape (e.g. `"arr[7]"`, `"arr[2]"`).
    pub shape: String,
}

/// Decode the top-level CBOR shape and return a `DecodedState`.
///
/// Returns `Err` with a human-readable message on any decode failure.
pub fn decode_state(cbor: &[u8], label: &str) -> Result<DecodedState, String> {
    let shape = top_level_shape(cbor, label)?;
    Ok(DecodedState {
        raw_cbor: cbor.to_vec(),
        shape,
    })
}

/// Decode the signal file for a NEWEPOCH rule as a CBOR u64.
///
/// The signal for NEWEPOCH is the target `EpochNo` (a bare CBOR unsigned
/// integer). Returns the epoch number on success.
pub fn decode_epoch_no(sig_cbor: &[u8]) -> Result<u64, String> {
    if sig_cbor.is_empty() {
        return Err("sig_cbor is empty".to_string());
    }
    let mut dec = minicbor::Decoder::new(sig_cbor);
    match dec.datatype().map_err(|e| format!("sig datatype: {e}"))? {
        Type::U8 | Type::U16 | Type::U32 | Type::U64 => {
            dec.u64().map_err(|e| format!("sig epoch_no decode: {e}"))
        }
        other => Err(format!(
            "sig_cbor: expected unsigned integer, got {other:?}"
        )),
    }
}

/// Decode the initial epoch number from field [0] of a NewEpochState blob.
///
/// NewEpochState is encoded as `array(7)` where field [0] is the current
/// `EpochNo` (a bare CBOR u64).
pub fn decode_initial_epoch_no(st_cbor: &[u8]) -> Result<u64, String> {
    if st_cbor.is_empty() {
        return Err("st_cbor is empty".to_string());
    }
    let mut dec = minicbor::Decoder::new(st_cbor);

    // Outer array(7)
    match dec
        .array()
        .map_err(|e| format!("NewEpochState outer array: {e}"))?
    {
        Some(7) => {}
        Some(n) => return Err(format!("NewEpochState: expected array(7), got array({n})")),
        None => return Err("NewEpochState: expected definite array(7), got indefinite".to_string()),
    }

    // Field [0] = EpochNo (u64)
    match dec
        .datatype()
        .map_err(|e| format!("field[0] datatype: {e}"))?
    {
        Type::U8 | Type::U16 | Type::U32 | Type::U64 => dec
            .u64()
            .map_err(|e| format!("NewEpochState field[0] epoch_no: {e}")),
        other => Err(format!(
            "NewEpochState field[0]: expected unsigned integer, got {other:?}"
        )),
    }
}

/// Extract the human-readable top-level CBOR shape of `cbor`.
fn top_level_shape(cbor: &[u8], label: &str) -> Result<String, String> {
    let mut dec = minicbor::Decoder::new(cbor);
    let ty = dec
        .datatype()
        .map_err(|e| format!("{label} datatype: {e}"))?;
    match ty {
        Type::Array => {
            let len = dec.array().map_err(|e| format!("{label} array: {e}"))?;
            match len {
                Some(n) => Ok(format!("arr[{n}]")),
                None => Ok("arr[indef]".to_string()),
            }
        }
        Type::Map => {
            let len = dec.map().map_err(|e| format!("{label} map: {e}"))?;
            match len {
                Some(n) => Ok(format!("map({n})")),
                None => Ok("map(indef)".to_string()),
            }
        }
        other => Ok(format!("{other:?}")),
    }
}
