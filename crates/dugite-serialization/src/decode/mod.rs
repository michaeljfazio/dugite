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
use dugite_primitives::hash::Hash32;
use dugite_primitives::transaction::{Transaction, TransactionInput, TransactionOutput};

// ---------------------------------------------------------------------------
// Re-exports for specific era decoders used outside this crate
// ---------------------------------------------------------------------------

/// Decode a raw CBOR `protocol_param_update` map into a
/// [`dugite_primitives::transaction::ProtocolParamUpdate`].
///
/// Accepts the map bytes directly (without a block or tx context).
/// Conway keys 0-33 and Dijkstra keys 34-37 are all handled.
pub use era_conway::ppu_from_cbor;

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
        // CIP-0167: Dijkstra removes the top-level `isValid` flag. The
        // standalone tx wire shape is array(3) — body, witness_set, aux_data
        // — instead of Conway's array(4). Route to the Dijkstra-specific
        // decoder so the missing element does not cause a decode error.
        7 => era_conway::decode_dijkstra_tx_standalone(tx_cbor),
        n => Err(SerializationError::CborDecode(format!(
            "unknown era id: {n}"
        ))),
    }
}

/// Decode a single `transaction_output` CBOR value for a specific era.
///
/// `era_id` follows the same Cardano HFC convention as [`decode_transaction`].
/// Only eras that admit Plutus scripts (Alonzo onwards) are supported here, as
/// this entry is intended for phase-2 resolved-UTxO decoding. Pre-Alonzo eras
/// return a [`SerializationError::CborDecode`] error.
///
/// | era_id | era      | output format                                |
/// |--------|----------|----------------------------------------------|
/// | 2      | Allegra  | legacy array `[addr, value, ?datum_hash]`    |
/// | 3      | Mary     | legacy array `[addr, value, ?datum_hash]`    |
/// | 4      | Alonzo   | legacy array `[addr, value, ?datum_hash]`    |
/// | 5      | Babbage  | legacy array OR post-Alonzo map              |
/// | 6      | Conway   | legacy array OR post-Alonzo map              |
/// | 7      | Dijkstra | legacy array OR post-Alonzo map              |
pub fn decode_transaction_output(
    era_id: u16,
    cbor: &[u8],
) -> Result<TransactionOutput, SerializationError> {
    use dugite_primitives::era::Era;
    match era_id {
        2 => era_alonzo::decode_alonzo_tx_output_standalone(cbor, Era::Allegra),
        3 => era_alonzo::decode_alonzo_tx_output_standalone(cbor, Era::Mary),
        4 => era_alonzo::decode_alonzo_tx_output_standalone(cbor, Era::Alonzo),
        5 => era_babbage::decode_babbage_tx_output_standalone(cbor),
        6 | 7 => era_conway::decode_conway_tx_output_standalone(cbor),
        n => Err(SerializationError::CborDecode(format!(
            "decode_transaction_output: unsupported era id: {n}"
        ))),
    }
}

/// Decode a single `transaction_input` CBOR value (era-invariant).
///
/// A transaction input is encoded as a 2-element array `[tx_hash(32), index]`
/// in every era from Shelley onwards. This entry is intended for phase-2
/// resolved-UTxO decoding where the ledger supplies input CBOR alongside the
/// output CBOR.
pub fn decode_transaction_input(cbor: &[u8]) -> Result<TransactionInput, SerializationError> {
    use minicbor::Decoder;
    let mut d = Decoder::new(cbor);
    let arr = d
        .array()
        .map_err(|e| SerializationError::CborDecode(format!("tx_in: {e}")))?;
    if !matches!(arr, Some(2)) {
        return Err(SerializationError::CborDecode(format!(
            "tx_in: expected array(2), got {arr:?}"
        )));
    }
    let hash_bytes = d
        .bytes()
        .map_err(|e| SerializationError::CborDecode(format!("tx_in hash: {e}")))?;
    let transaction_id =
        Hash32::try_from(hash_bytes).map_err(|_| SerializationError::InvalidLength {
            expected: 32,
            got: hash_bytes.len(),
        })?;
    let index = d
        .u32()
        .map_err(|e| SerializationError::CborDecode(format!("tx_in idx: {e}")))?;
    Ok(TransactionInput {
        transaction_id,
        index,
    })
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

    // ───────────────────────────────────────────────────────────────
    // decode_transaction_input
    // ───────────────────────────────────────────────────────────────

    #[test]
    fn decode_transaction_input_roundtrips_a_canonical_input() {
        // [bytes(32), uint] — the canonical Cardano `transaction_input` shape.
        // Construct: array(2) [bytes(32) of 0x11..0x11, uint(42)].
        let mut cbor = vec![0x82, 0x58, 0x20];
        cbor.extend(std::iter::repeat_n(0x11u8, 32));
        cbor.push(0x18);
        cbor.push(42);
        let input = decode_transaction_input(&cbor).expect("decode");
        assert_eq!(input.index, 42);
        assert_eq!(input.transaction_id.as_bytes(), &[0x11; 32]);
    }

    #[test]
    fn decode_transaction_input_rejects_wrong_arity() {
        // array(3) instead of array(2).
        let mut cbor = vec![0x83, 0x58, 0x20];
        cbor.extend([0u8; 32]);
        cbor.push(0x00);
        cbor.push(0x00);
        let err = decode_transaction_input(&cbor).unwrap_err();
        let SerializationError::CborDecode(msg) = err else {
            panic!("expected CborDecode");
        };
        assert!(msg.contains("expected array(2)"), "got: {msg}");
    }

    #[test]
    fn decode_transaction_input_rejects_short_hash() {
        // array(2) [bytes(28) ..., uint] — short hash.
        let mut cbor = vec![0x82, 0x58, 0x1c];
        cbor.extend([0u8; 28]);
        cbor.push(0x00);
        let err = decode_transaction_input(&cbor).unwrap_err();
        match err {
            SerializationError::InvalidLength { expected, got } => {
                assert_eq!(expected, 32);
                assert_eq!(got, 28);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn decode_transaction_input_rejects_truncated() {
        // Truncated array header — must error, not panic.
        for bytes in [&[][..], &[0x82][..], &[0x82, 0x58][..]] {
            assert!(decode_transaction_input(bytes).is_err());
        }
    }

    // ───────────────────────────────────────────────────────────────
    // decode_transaction_output
    // ───────────────────────────────────────────────────────────────

    /// A minimal Babbage/Conway map-form output: `{0: addr_bytes, 1: lovelace}`.
    ///
    /// `addr` is a 29-byte preview enterprise address (header 0x60 + 28-byte
    /// key hash). Lovelace = 1_000_000.
    fn make_babbage_map_output_cbor() -> Vec<u8> {
        let mut out = vec![0xa2]; // map(2)
        out.push(0x00); // key 0
        out.push(0x58); // bytes(29)
        out.push(29);
        out.push(0x60); // mainnet enterprise header
        out.extend([0xaa; 28]);
        out.push(0x01); // key 1
        out.push(0x1a); // uint(u32)
        out.extend(1_000_000u32.to_be_bytes());
        out
    }

    /// A minimal Alonzo-era legacy output: `[addr_bytes, lovelace]`.
    fn make_alonzo_legacy_output_cbor() -> Vec<u8> {
        let mut out = vec![0x82]; // array(2)
        out.push(0x58);
        out.push(29);
        out.push(0x60);
        out.extend([0xbb; 28]);
        out.push(0x1a);
        out.extend(2_000_000u32.to_be_bytes());
        out
    }

    #[test]
    fn decode_transaction_output_decodes_conway_map_form() {
        let cbor = make_babbage_map_output_cbor();
        for era_id in [5u16, 6, 7] {
            let out = decode_transaction_output(era_id, &cbor).expect("decode");
            assert_eq!(out.value.coin.0, 1_000_000, "era_id={era_id}");
            assert!(out.raw_cbor.is_some(), "raw_cbor must be preserved");
        }
    }

    #[test]
    fn decode_transaction_output_decodes_alonzo_legacy_array() {
        let cbor = make_alonzo_legacy_output_cbor();
        for era_id in [2u16, 3, 4] {
            let out = decode_transaction_output(era_id, &cbor).expect("decode");
            assert_eq!(out.value.coin.0, 2_000_000, "era_id={era_id}");
        }
    }

    #[test]
    fn decode_transaction_output_rejects_pre_alonzo_eras() {
        let cbor = make_alonzo_legacy_output_cbor();
        for era_id in [0u16, 1, 8, 42] {
            let err = decode_transaction_output(era_id, &cbor).unwrap_err();
            let SerializationError::CborDecode(msg) = err else {
                panic!("era_id={era_id}: expected CborDecode, got: {err:?}");
            };
            assert!(msg.contains("unsupported era id"), "era_id={era_id}: {msg}");
        }
    }

    #[test]
    fn decode_transaction_output_errors_on_truncated_input() {
        // Empty / truncated / malformed bytes must error, never panic.
        for era_id in [4u16, 5, 6, 7] {
            for bytes in [&[][..], &[0xff][..], &[0x82, 0x58][..], &[0xa2, 0x00][..]] {
                assert!(
                    decode_transaction_output(era_id, bytes).is_err(),
                    "era_id={era_id} bytes={bytes:?} must error"
                );
            }
        }
    }
}
