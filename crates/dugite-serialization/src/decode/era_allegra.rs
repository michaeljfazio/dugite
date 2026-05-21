//! In-house decoder for the Allegra era (era tag 3).
//!
//! Allegra is a thin delta over Shelley: it adds `validity_interval_start`
//! (tx body key 8). The block structure is identical to Shelley (4 elements,
//! no `invalid_transactions` field).
//!
//! This module delegates to the Alonzo family decoder with `has_invalid_txs = false`
//! and `era = Era::Allegra`. All tx body keys 0–8 are decoded; keys 9+ are
//! silently skipped for forward compatibility.

use crate::decode::era_alonzo::{decode_alonzo_family_block, DecodeMode};
use crate::error::SerializationError;
use dugite_primitives::block::Block;
use dugite_primitives::era::Era;

/// Decode an Allegra block from the inner CBOR (after HFC envelope stripping).
pub fn decode_allegra_block(inner_cbor: &[u8]) -> Result<Block, SerializationError> {
    decode_alonzo_family_block(inner_cbor, Era::Allegra, false, DecodeMode::Full)
}

/// Decode an Allegra block in minimal mode (witness set skipped).
pub fn decode_allegra_block_minimal(inner_cbor: &[u8]) -> Result<Block, SerializationError> {
    decode_alonzo_family_block(inner_cbor, Era::Allegra, false, DecodeMode::Minimal)
}

#[cfg(test)]
mod tests {
    //! Allegra block structure is byte-identical to Shelley (4-element block,
    //! no `invalid_transactions` field). We exercise the Allegra wrappers
    //! against the Shelley test vector — Shelley CBOR is a strict subset of
    //! Allegra wire-format and must round-trip through both entry points.
    use super::*;

    fn shelley_inner_cbor() -> Vec<u8> {
        let hex_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/test_vectors/shelley.hex"
        ))
        .unwrap();
        let raw = hex::decode(hex_str.trim()).unwrap();
        // Outer wrapper is `[0x82, era_tag, inner_block]`. Era tag is one byte
        // (info <= 23) for all current Cardano eras, so inner starts at offset 2.
        assert_eq!(raw[0], 0x82, "expected outer array(2)");
        raw[2..].to_vec()
    }

    #[test]
    fn decode_allegra_block_full_smokes() {
        let inner = shelley_inner_cbor();
        let blk = decode_allegra_block(&inner).expect("full-mode allegra decode");
        assert_eq!(blk.era, Era::Allegra);
    }

    #[test]
    fn decode_allegra_block_minimal_smokes() {
        let inner = shelley_inner_cbor();
        let blk = decode_allegra_block_minimal(&inner).expect("minimal-mode allegra decode");
        assert_eq!(blk.era, Era::Allegra);
    }

    #[test]
    fn decode_allegra_block_rejects_garbage() {
        assert!(decode_allegra_block(&[]).is_err());
        assert!(decode_allegra_block_minimal(&[0xff]).is_err());
    }
}
