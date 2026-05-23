//! CBOR decode for ImpSpec dump vectors.
//!
//! Each `.cbor` file produced by cardano-ledger's ImpSpec test suite with
//! `CONFORMANCE_CBOR_DUMP_PATH` set is a 5-element CBOR array:
//!
//! ```text
//! [config, initial_state, final_state, events, title]
//! ```
//!
//! Where:
//! - `config` — arr[13] of protocol-param fields
//! - `initial_state` — arr[7] ledger state snapshot
//! - `final_state` — arr[7] expected state after applying events
//! - `events` — arr[N] of event discriminants (0=Transaction, 1=PassTick, 2=PassEpoch)
//! - `title` — UTF-8 text describing the test scenario

/// Decoded ImpSpec event.
#[derive(Debug)]
pub enum ImpEvent {
    /// Apply a transaction.
    Transaction {
        tx_cbor: Vec<u8>,
        expected_valid: bool,
        slot: u64,
    },
    /// Advance the slot clock without a transaction.
    PassTick { slot: u64 },
    /// Cross an epoch boundary.
    PassEpoch { delta: u64 },
}

/// Partially-decoded ImpSpec dump vector.
///
/// `config`, `initial_state`, and `final_state` are kept as raw CBOR bytes for
/// downstream bridge/compare modules. Decoding them fully requires the complete
/// ledger state bridge (`bridge.rs`), which is a Phase 4 follow-on.
#[derive(Debug)]
pub struct ImpVector {
    pub title: String,
    pub config_cbor: Vec<u8>,
    pub initial_state_cbor: Vec<u8>,
    pub final_state_cbor: Vec<u8>,
    pub events: Vec<ImpEvent>,
}

/// Decode an ImpSpec dump vector from raw CBOR bytes.
///
/// Returns `Err` with a human-readable message on any decode failure.
pub fn decode_vector(data: &[u8]) -> Result<ImpVector, String> {
    use minicbor::data::Type;

    let mut dec = minicbor::Decoder::new(data);

    // Outer: 5-element array.
    match dec.array().map_err(|e| format!("outer array: {e}"))? {
        Some(5) => {}
        Some(n) => return Err(format!("expected 5-element outer array, got {n}")),
        None => return Err("expected definite outer array, got indefinite".to_string()),
    }

    // Element 0 — config (opaque; we record its CBOR bytes).
    let config_start = dec.position();
    skip_cbor_value(&mut dec)?;
    let config_end = dec.position();
    let config_cbor = data[config_start..config_end].to_vec();

    // Element 1 — initial_state (opaque).
    let init_start = dec.position();
    skip_cbor_value(&mut dec)?;
    let init_end = dec.position();
    let initial_state_cbor = data[init_start..init_end].to_vec();

    // Element 2 — final_state (opaque).
    let final_start = dec.position();
    skip_cbor_value(&mut dec)?;
    let final_end = dec.position();
    let final_state_cbor = data[final_start..final_end].to_vec();

    // Element 3 — events array.
    let event_count = dec.array().map_err(|e| format!("events array: {e}"))?;
    let mut events = Vec::new();
    let count = event_count.unwrap_or(u64::MAX); // indefinite: decode until Break
    for i in 0..count {
        if count == u64::MAX {
            // Check for indefinite-length break.
            if matches!(dec.datatype(), Ok(Type::Break)) {
                dec.skip().map_err(|e| format!("skip break: {e}"))?;
                break;
            }
        }
        let event = decode_event(&mut dec, i)?;
        events.push(event);
    }

    // Element 4 — title (text string).
    let title = dec.str().map_err(|e| format!("title: {e}"))?.to_owned();

    Ok(ImpVector {
        title,
        config_cbor,
        initial_state_cbor,
        final_state_cbor,
        events,
    })
}

fn decode_event(dec: &mut minicbor::Decoder, idx: u64) -> Result<ImpEvent, String> {
    // Each event is a definite-length array: [discriminant, ...fields].
    let len = dec
        .array()
        .map_err(|e| format!("event[{idx}] array: {e}"))?
        .ok_or_else(|| format!("event[{idx}] must be definite array"))?;

    if len == 0 {
        return Err(format!("event[{idx}] is empty"));
    }

    let discriminant: u64 = dec
        .u64()
        .map_err(|e| format!("event[{idx}] discriminant: {e}"))?;

    match discriminant {
        0 => {
            // Transaction: [0, tx_cbor_bytes, expected_valid, slot]
            if len != 4 {
                return Err(format!("Transaction event must have 4 elements, got {len}"));
            }
            let tx_bytes = dec
                .bytes()
                .map_err(|e| format!("event[{idx}] tx_cbor: {e}"))?
                .to_vec();
            let expected_valid = dec.bool().map_err(|e| format!("event[{idx}] valid: {e}"))?;
            let slot = dec.u64().map_err(|e| format!("event[{idx}] slot: {e}"))?;
            Ok(ImpEvent::Transaction {
                tx_cbor: tx_bytes,
                expected_valid,
                slot,
            })
        }
        1 => {
            // PassTick: [1, slot]
            if len != 2 {
                return Err(format!("PassTick event must have 2 elements, got {len}"));
            }
            let slot = dec.u64().map_err(|e| format!("event[{idx}] slot: {e}"))?;
            Ok(ImpEvent::PassTick { slot })
        }
        2 => {
            // PassEpoch: [2, delta]
            if len != 2 {
                return Err(format!("PassEpoch event must have 2 elements, got {len}"));
            }
            let delta = dec.u64().map_err(|e| format!("event[{idx}] delta: {e}"))?;
            Ok(ImpEvent::PassEpoch { delta })
        }
        d => Err(format!("unknown event discriminant {d}")),
    }
}

/// Skip one CBOR value at the current position (recursively for nested structures).
fn skip_cbor_value(dec: &mut minicbor::Decoder) -> Result<(), String> {
    dec.skip().map_err(|e| format!("skip: {e}"))
}
