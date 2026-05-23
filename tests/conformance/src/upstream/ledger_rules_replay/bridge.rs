//! Phase 4 — Bridge: inspect and summarise ImpSpec dump CBOR.
//!
//! Each ImpSpec dump vector is a 5-element CBOR array:
//!   [config, initial_state, final_state, events, title]
//!
//! `config` and the two state blobs are kept as raw CBOR by `vector.rs`.
//! This module provides:
//!
//! 1. `DecodedState` — a lightweight wrapper that records the raw bytes
//!    together with a human-readable shape description extracted via minicbor.
//!
//! 2. `decode_state(cbor, label)` — entry point for tests; returns a
//!    `DecodedState` or an error string.
//!
//! 3. `decode_config(cbor)` — validates that the config blob looks like a
//!    non-empty CBOR array (arr[13] in the ImpSpec format).
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

/// Validate that the config blob is a non-empty CBOR array.
///
/// The ImpSpec spec says config = arr[13] of protocol-param fields.
/// We validate the outer container only; full typed decode is a Phase 4
/// follow-on that requires the ledger bridge.
pub fn decode_config(cbor: &[u8]) -> Result<usize, String> {
    let mut dec = minicbor::Decoder::new(cbor);
    match dec.datatype().map_err(|e| format!("config type: {e}"))? {
        Type::Array => {
            let len = dec
                .array()
                .map_err(|e| format!("config array: {e}"))?
                .ok_or_else(|| "config: expected definite array, got indefinite".to_string())?;
            Ok(len as usize)
        }
        other => Err(format!("config: expected array, got {other:?}")),
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
