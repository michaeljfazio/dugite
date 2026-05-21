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
