//! In-house multi-era Cardano block / transaction decoder.
//!
//! This module is the production decoder for Cardano blocks across all 8 eras
//! (Byron through Dijkstra). It replaced the legacy decoder in
//! milestone M4 of the in-house decoder program (plan: humming-dewdrop).
//!
//! ## Layout
//!
//! ```text
//! decode/
//!   mod.rs            — public entry points + era dispatch
//!   reader.rs         — Reader: minicbor::Decoder + offset tracking
//!   raw.rs            — KeepRaw<'b, T>::parse_with helper
//!   primitives.rs     — Nullable, MaybeIndef, KeyValuePairs wrappers
//!   helpers.rs        — hash decode, lovelace, NetworkId
//!   cbor_helpers.rs   — stateless block-body byte walkers
//!   block.rs          — envelope walker + per-era dispatch
//!   era_byron.rs      — Byron main + EBB (era tags 0, 1)
//!   era_shelley.rs    — Shelley (era tag 2)
//!   era_allegra.rs    — Allegra (era tag 3)
//!   era_mary.rs       — Mary (era tag 4)
//!   era_alonzo.rs     — Alonzo (era tag 5; also Allegra/Mary delta base)
//!   era_babbage.rs    — Babbage (era tag 6)
//!   era_conway.rs     — Conway + Dijkstra (era tags 7, 8)
//! ```
//!
//! All public `decode_block*` entry points route through this module to the
//! in-house decoder. `decode_transaction` is currently still routed via the
//! legacy wrapper at [`crate::multi_era`] pending the in-house tx-level
//! decoder (M6 follow-up).

#[allow(dead_code)]
pub(crate) mod block;
pub(crate) mod cbor_helpers;
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

use crate::error::SerializationError;
use dugite_primitives::block::Block;
use dugite_primitives::transaction::Transaction;

// ---------------------------------------------------------------------------
// Public API — every block-level decode routes through the in-house decoder
// ---------------------------------------------------------------------------

/// Decode a multi-era block from raw CBOR bytes into a dugite [`Block`].
///
/// Routes every era (Byron through Dijkstra) through the in-house decoder
/// at [`block::decode_block`]. The full HFC-wrapped CBOR bytes are preserved
/// in the returned [`Block::raw_cbor`] so that ChainDB can serve byte-exact
/// CBOR on BlockFetch without re-encoding.
pub fn decode_block(cbor: &[u8]) -> Result<Block, SerializationError> {
    decode_block_with_byron_epoch_length(cbor, 0)
}

/// Decode a multi-era block with explicit Byron epoch length (for non-mainnet
/// networks). See [`decode_block`].
pub fn decode_block_with_byron_epoch_length(
    cbor: &[u8],
    byron_epoch_length: u64,
) -> Result<Block, SerializationError> {
    let mut blk = block::decode_block(cbor, byron_epoch_length, false)?;
    blk.raw_cbor = Some(cbor.to_vec());
    Ok(blk)
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
    let mut blk = block::decode_block(cbor, byron_epoch_length, true)?;
    blk.raw_cbor = Some(cbor.to_vec());
    Ok(blk)
}

/// Decode a transaction CBOR for a specific era.
///
/// `era_id` follows the Cardano HFC convention:
/// 0 = Byron, 1 = Shelley, 2 = Allegra, 3 = Mary, 4 = Alonzo,
/// 5 = Babbage, 6 = Conway, 7 = Dijkstra.
pub fn decode_transaction(era_id: u16, tx_cbor: &[u8]) -> Result<Transaction, SerializationError> {
    use dugite_primitives::era::Era;
    match era_id {
        0 => era_byron::decode_byron_tx_standalone(tx_cbor),
        1 => era_shelley::decode_shelley_tx_standalone(tx_cbor),
        2 => era_alonzo::decode_alonzo_family_tx_standalone(tx_cbor, Era::Allegra),
        3 => era_alonzo::decode_alonzo_family_tx_standalone(tx_cbor, Era::Mary),
        4 => era_alonzo::decode_alonzo_family_tx_standalone(tx_cbor, Era::Alonzo),
        5 => era_babbage::decode_babbage_tx_standalone(tx_cbor),
        6 => era_conway::decode_conway_tx_standalone(tx_cbor, Era::Conway),
        7 => era_conway::decode_conway_tx_standalone(tx_cbor, Era::Dijkstra),
        n => Err(SerializationError::CborDecode(format!(
            "unknown era id: {n}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_transaction_unknown_era_errors() {
        // era_id = 8 is past Dijkstra; era_id = 99 is gibberish.
        for n in [8u16, 9, 42, 99] {
            let err = decode_transaction(n, &[0x80]).unwrap_err();
            let SerializationError::CborDecode(msg) = err else {
                panic!("expected CborDecode for era_id={n}");
            };
            assert!(
                msg.contains("unknown era id"),
                "era_id={n}: unexpected message: {msg}"
            );
        }
    }

    #[test]
    fn decode_transaction_invalid_cbor_per_era_errors() {
        // Every era should produce an error (not panic) on truncated/garbage CBOR.
        for era_id in 0u16..=7 {
            assert!(
                decode_transaction(era_id, &[]).is_err(),
                "era_id={era_id}: empty CBOR must error"
            );
            assert!(
                decode_transaction(era_id, &[0xff]).is_err(),
                "era_id={era_id}: bare break byte must error"
            );
        }
    }

    #[test]
    fn decode_block_minimal_smokes_on_real_vector() {
        // Exercises decode_block_minimal — the minimal-mode public entrypoint
        // — against the bundled Conway vector. Mainly to cover the wrapper.
        let cbor = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/test_vectors/conway.hex"
        ))
        .unwrap();
        let raw = hex::decode(cbor.trim()).unwrap();
        let blk = decode_block_minimal(&raw).expect("minimal-mode decode");
        assert!(blk.raw_cbor.is_some(), "raw_cbor must be preserved");
    }

    #[test]
    fn decode_block_with_byron_epoch_length_passes_through() {
        // The non-zero byron_epoch_length path is exercised when decoding Byron
        // blocks; for a Shelley+ vector the value is ignored. We pin that the
        // wrapper at least returns Ok and preserves raw_cbor.
        let cbor = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/test_vectors/shelley.hex"
        ))
        .unwrap();
        let raw = hex::decode(cbor.trim()).unwrap();
        let blk = decode_block_with_byron_epoch_length(&raw, 21_600).expect("decode");
        assert!(blk.raw_cbor.is_some());
        let blk2 =
            decode_block_minimal_with_byron_epoch_length(&raw, 21_600).expect("minimal decode");
        assert!(blk2.raw_cbor.is_some());
    }
}
