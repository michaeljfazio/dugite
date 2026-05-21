//! In-house multi-era Cardano block / transaction decoder.
//!
//! This module is the eventual replacement for [`crate::multi_era`], the
//! pallas-backed decoder. Currently every function delegates to its
//! pallas counterpart while the in-house implementation is built out in
//! milestone M4 of the pallas-removal program (plan: humming-dewdrop).
//!
//! ## Layout (target — populated in M4)
//!
//! ```text
//! decode/
//!   mod.rs            — this file: public entry points + era dispatch
//!   reader.rs         — Reader: minicbor::Decoder + offset tracking
//!   raw.rs            — KeepRaw<'b, T>::parse_with helper
//!   primitives.rs     — Nullable, MaybeIndef, KeyValuePairs wrappers
//!   helpers.rs        — hash decode, lovelace, NetworkId (no 28→32 padding)
//!   block.rs, transaction.rs, witness_set.rs, header.rs
//!   value.rs, script.rs, plutus_data.rs, redeemer.rs
//!   certificate.rs, governance.rs, protocol_params.rs, auxiliary.rs
//!   era_byron.rs, era_shelley.rs, era_allegra.rs, era_mary.rs,
//!   era_alonzo.rs, era_babbage.rs, era_conway.rs, era_dijkstra.rs
//! ```
//!
//! While this module delegates to pallas, the [`crate::dual_decode`] harness
//! still exercises the comparator end-to-end — the comparison is tautological
//! (pallas vs pallas) but every code path including dump-mode artifact
//! generation runs, so M4 only needs to swap the implementation, not the
//! plumbing.

use crate::error::SerializationError;
use dugite_primitives::block::Block;
use dugite_primitives::transaction::Transaction;

/// Decode a multi-era block from raw CBOR bytes into a dugite [`Block`].
///
/// **M3:** delegates to the pallas-backed decoder.
/// **M4:** replaced with a from-scratch minicbor-based walker.
pub fn decode_block(cbor: &[u8]) -> Result<Block, SerializationError> {
    crate::multi_era::decode_block(cbor)
}

/// Decode a multi-era block with explicit Byron epoch length (for non-mainnet
/// networks). See [`decode_block`].
pub fn decode_block_with_byron_epoch_length(
    cbor: &[u8],
    byron_epoch_length: u64,
) -> Result<Block, SerializationError> {
    crate::multi_era::decode_block_with_byron_epoch_length(cbor, byron_epoch_length)
}

/// Decode a multi-era block in minimal (witness-skipping) mode.
///
/// Used by block replay (`ApplyOnly` ledger mode); **not** safe at tip
/// where Phase-1/Phase-2 validation reads the witness set.
pub fn decode_block_minimal(cbor: &[u8]) -> Result<Block, SerializationError> {
    crate::multi_era::decode_block_minimal(cbor)
}

/// Minimal decode with explicit Byron epoch length.
pub fn decode_block_minimal_with_byron_epoch_length(
    cbor: &[u8],
    byron_epoch_length: u64,
) -> Result<Block, SerializationError> {
    crate::multi_era::decode_block_minimal_with_byron_epoch_length(cbor, byron_epoch_length)
}

/// Decode a transaction CBOR for a specific era.
pub fn decode_transaction(era_id: u16, tx_cbor: &[u8]) -> Result<Transaction, SerializationError> {
    crate::multi_era::decode_transaction(era_id, tx_cbor)
}
