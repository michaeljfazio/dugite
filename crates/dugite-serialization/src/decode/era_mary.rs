//! In-house decoder for the Mary era (era tag 4).
//!
//! Mary adds multi-asset `mint` (tx body key 9) and multi-asset `Value`
//! (`[coin, multiasset_map]`) over Allegra. The block structure is identical
//! to Shelley/Allegra (4 elements, no `invalid_transactions` field).
//!
//! This module delegates to the Alonzo family decoder with `has_invalid_txs = false`
//! and `era = Era::Mary`. Mary knows tx body keys 0–9; any out-of-era key
//! (e.g. Alonzo's 11/13/14/15, or 10+) is HARD-REJECTED, mirroring Haskell
//! cardano-ledger's per-era SparseKeyed `bodyFields` catch-all. See #31-E.

use crate::decode::era_alonzo::{decode_alonzo_family_block, DecodeMode};
use crate::error::SerializationError;
use dugite_primitives::block::Block;
use dugite_primitives::era::Era;

/// Decode a Mary block from the inner CBOR (after HFC envelope stripping).
pub fn decode_mary_block(inner_cbor: &[u8]) -> Result<Block, SerializationError> {
    decode_alonzo_family_block(inner_cbor, Era::Mary, false, DecodeMode::Full)
}

/// Decode a Mary block in minimal mode (witness set skipped).
pub fn decode_mary_block_minimal(inner_cbor: &[u8]) -> Result<Block, SerializationError> {
    decode_alonzo_family_block(inner_cbor, Era::Mary, false, DecodeMode::Minimal)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mary_inner_cbor() -> Vec<u8> {
        let hex_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/test_vectors/mary.hex"
        ))
        .unwrap();
        let raw = hex::decode(hex_str.trim()).unwrap();
        assert_eq!(raw[0], 0x82, "expected outer array(2)");
        // Era tag for Mary is 4 (single-byte form).
        assert_eq!(raw[1], 0x04, "expected mary era tag 4");
        raw[2..].to_vec()
    }

    #[test]
    fn decode_mary_block_minimal_smokes() {
        let inner = mary_inner_cbor();
        let blk = decode_mary_block_minimal(&inner).expect("minimal-mode mary decode");
        assert_eq!(blk.era, Era::Mary);
        // Witnesses are skipped in minimal mode; raw CBOR for them is captured
        // but parsed sets default to empty.
        for tx in &blk.transactions {
            assert!(
                tx.raw_witness_cbor.is_some(),
                "raw_witness_cbor must be preserved even in minimal mode"
            );
        }
    }

    #[test]
    fn decode_mary_block_rejects_garbage() {
        assert!(decode_mary_block(&[]).is_err());
        assert!(decode_mary_block_minimal(&[0xff]).is_err());
    }
}
