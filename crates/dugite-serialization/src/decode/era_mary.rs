//! In-house decoder for the Mary era (era tag 4).
//!
//! Mary adds multi-asset `mint` (tx body key 9) and multi-asset `Value`
//! (`[coin, multiasset_map]`) over Allegra. The block structure is identical
//! to Shelley/Allegra (4 elements, no `invalid_transactions` field).
//!
//! This module delegates to the Alonzo family decoder with `has_invalid_txs = false`
//! and `era = Era::Mary`. All tx body keys 0–9 are decoded; keys 11+ are
//! silently skipped for forward compatibility.

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
