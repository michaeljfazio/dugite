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

// Foundation modules — Reader, KeepRaw, primitives, helpers, block envelope.
//
// Currently `pub(crate)` because no caller outside this crate consumes them
// directly; per-era decoder files (M4a/b/c) will use them. The dead-code
// allowance covers items not yet wired into a caller as those era files
// land; once each era's decoder is in place, callers exist and the allow
// reduces to no-ops.
#[allow(dead_code)]
pub(crate) mod block;
#[allow(dead_code)]
pub(crate) mod helpers;
#[allow(dead_code)]
pub(crate) mod primitives;
#[allow(dead_code)]
pub(crate) mod raw;
#[allow(dead_code)]
pub(crate) mod reader;

// In-house era decoders — all 8 Cardano eras.
//
// M4a: Byron (era tags 0/1), Shelley (2).
// M4b: Allegra (3), Mary (4), Alonzo (5), Babbage (6).
// M4c: Conway (7), Dijkstra (8).
pub(crate) mod era_allegra;
pub(crate) mod era_alonzo;
pub(crate) mod era_babbage;
pub(crate) mod era_byron;
pub(crate) mod era_conway;
pub(crate) mod era_mary;
pub(crate) mod era_shelley;

use crate::decode::block::EraTag;
use crate::decode::reader::Reader;
use crate::error::SerializationError;
use dugite_primitives::block::Block;
use dugite_primitives::transaction::Transaction;

// ---------------------------------------------------------------------------
// Era-tag peek helpers
// ---------------------------------------------------------------------------

/// Peek at the era tag in the HFC envelope without consuming it.
///
/// Returns `None` if the CBOR can't be read (e.g. empty slice), `Some(tag)`
/// otherwise.  Resets the reader position so the full block can still be
/// decoded.
fn peek_era_tag(cbor: &[u8]) -> Option<EraTag> {
    let mut r = Reader::new(cbor);
    // outer array(2)
    match r.read_array_header() {
        Ok(Some(2)) => {}
        _ => return None,
    }
    // era uint
    match r.read_uint() {
        Ok(n) => Some(EraTag::from_u64(n)),
        Err(_) => None,
    }
}

/// Returns `true` if the era tag is handled by an in-house decoder.
///
/// All eras (Byron 0/1, Shelley 2, Allegra 3, Mary 4, Alonzo 5, Babbage 6,
/// Conway 7, Dijkstra 8) are handled in-house. Unknown era tags fall through
/// to pallas as a safety net.
fn is_inhouse_era(tag: EraTag) -> bool {
    matches!(
        tag,
        EraTag::ByronMain
            | EraTag::ByronEbb
            | EraTag::Shelley
            | EraTag::Allegra
            | EraTag::Mary
            | EraTag::Alonzo
            | EraTag::Babbage
            | EraTag::Conway
            | EraTag::Dijkstra
    )
}

// ---------------------------------------------------------------------------
// Public API — dispatch Byron/Shelley in-house, others via pallas
// ---------------------------------------------------------------------------

/// Decode a multi-era block from raw CBOR bytes into a dugite [`Block`].
///
/// **M4a/c routing:**
/// - Byron (era tags 0/1), Shelley (2), Conway (7), Dijkstra (8) → in-house decoder.
/// - Allegra (3), Mary (4), Alonzo (5), Babbage (6) → pallas-backed decoder (M4b pending).
pub fn decode_block(cbor: &[u8]) -> Result<Block, SerializationError> {
    decode_block_with_byron_epoch_length(cbor, 0)
}

/// Decode a multi-era block with explicit Byron epoch length (for non-mainnet
/// networks). See [`decode_block`].
pub fn decode_block_with_byron_epoch_length(
    cbor: &[u8],
    byron_epoch_length: u64,
) -> Result<Block, SerializationError> {
    if let Some(tag) = peek_era_tag(cbor) {
        if is_inhouse_era(tag) {
            let mut blk = block::decode_block(cbor, byron_epoch_length, false)?;
            // Mirror the pallas path: store the full HFC-wrapped bytes so that
            // ChainDB can serve byte-exact CBOR on BlockFetch without re-encoding.
            blk.raw_cbor = Some(cbor.to_vec());
            return Ok(blk);
        }
    }
    crate::multi_era::decode_block_with_byron_epoch_length(cbor, byron_epoch_length)
}

/// Decode a multi-era block in minimal (witness-skipping) mode.
///
/// Used by block replay (`ApplyOnly` ledger mode); **not** safe at tip
/// where Phase-1/Phase-2 validation reads the witness set.
pub fn decode_block_minimal(cbor: &[u8]) -> Result<Block, SerializationError> {
    decode_block_minimal_with_byron_epoch_length(cbor, 0)
}

/// Minimal decode with explicit Byron epoch length.
pub fn decode_block_minimal_with_byron_epoch_length(
    cbor: &[u8],
    byron_epoch_length: u64,
) -> Result<Block, SerializationError> {
    if let Some(tag) = peek_era_tag(cbor) {
        if is_inhouse_era(tag) {
            let mut blk = block::decode_block(cbor, byron_epoch_length, true)?;
            blk.raw_cbor = Some(cbor.to_vec());
            return Ok(blk);
        }
    }
    crate::multi_era::decode_block_minimal_with_byron_epoch_length(cbor, byron_epoch_length)
}

/// Decode a transaction CBOR for a specific era.
pub fn decode_transaction(era_id: u16, tx_cbor: &[u8]) -> Result<Transaction, SerializationError> {
    crate::multi_era::decode_transaction(era_id, tx_cbor)
}
