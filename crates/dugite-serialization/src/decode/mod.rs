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

/// Recover per-element original CBOR byte spans from a preserved `plutus_data`
/// witness array, so datum hashes can be computed over the original bytes
/// (matching Haskell `MemoBytes`) rather than a non-reproducible re-encoding.
pub use era_alonzo::plutus_data_element_spans;

/// Decode a Byron main-chain block from its inner CBOR (post envelope strip).
/// Re-exported so fuzz harnesses and external tooling (Mithril chunk
/// inspection, era-specific replay) can call the Byron path directly without
/// constructing a full envelope.
pub use era_byron::{decode_byron_ebb_block, decode_byron_main_block};

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

/// Decode JUST the block header from an HFC-wrapped N2N ChainSync header
/// CBOR (issue #654 — eager per-peer header validation in the ChainSync
/// receive loop).
///
/// Input format is the `MsgRollForward` payload's header bytes:
/// `[era_tag(uint), tag(24)(bytes(inner_header))]` for Shelley+ eras.
///
/// Returns the decoded [`BlockHeader`] with `header_hash` set to
/// `blake2b_256(inner_header_bytes)` — the canonical Cardano block hash
/// for Shelley+ eras. **Byron headers are intentionally NOT supported**
/// (return `Err`): the eager-validation path that owns this decoder
/// (#652 C8) is PBFT-unaware and skips Byron headers via a separate
/// predicate before dispatching here.
///
/// Cost: one CBOR walk + one Blake2b-256 over the inner header bytes.
/// Substantially cheaper than `decode_block_minimal` which walks the
/// entire block body + every transaction body.
pub fn decode_wrapped_block_header(
    wrapped_cbor: &[u8],
) -> Result<dugite_primitives::block::BlockHeader, SerializationError> {
    use minicbor::Decoder;

    // Outer wrap: [era_tag(uint), tag24(bytes(inner_header))]
    let mut dec = Decoder::new(wrapped_cbor);
    let arr_len = dec
        .array()
        .map_err(|e| SerializationError::CborDecode(format!("wrapped header outer: {e}")))?;
    if arr_len != Some(2) {
        return Err(SerializationError::CborDecode(format!(
            "wrapped header: expected outer array(2), got {arr_len:?}"
        )));
    }
    // First element must be a uint — Byron wraps it as a nested array
    // which we reject here so callers can fall back to a Byron-specific
    // path.
    let dt = dec
        .datatype()
        .map_err(|e| SerializationError::CborDecode(format!("wrapped header tag type: {e}")))?;
    if !matches!(
        dt,
        minicbor::data::Type::U8
            | minicbor::data::Type::U16
            | minicbor::data::Type::U32
            | minicbor::data::Type::U64
    ) {
        return Err(SerializationError::CborDecode(format!(
            "wrapped header: era tag is not a uint (got {dt:?}) — \
             Byron headers are not supported by this decoder"
        )));
    }
    let era_tag = dec
        .u64()
        .map_err(|e| SerializationError::CborDecode(format!("wrapped header era_tag: {e}")))?;
    let tag = dec
        .tag()
        .map_err(|e| SerializationError::CborDecode(format!("wrapped header tag: {e}")))?;
    if tag != minicbor::data::Tag::new(24) {
        return Err(SerializationError::CborDecode(format!(
            "wrapped header: expected tag(24), got {tag:?}"
        )));
    }
    let inner = dec
        .bytes()
        .map_err(|e| SerializationError::CborDecode(format!("wrapped header inner bytes: {e}")))?;

    // Dispatch by era tag. Mapping matches `decode_block`:
    //   2 Shelley, 3 Allegra, 4 Mary, 5 Alonzo, 6 Babbage,
    //   7 Conway, 8 Dijkstra.
    match era_tag {
        2 => era_shelley::decode_shelley_block_header(inner),
        3..=5 => era_alonzo::decode_alonzo_block_header(inner),
        6 => era_babbage::decode_babbage_block_header(inner),
        7 | 8 => era_conway::decode_conway_block_header(inner),
        0 | 1 => Err(SerializationError::CborDecode(format!(
            "wrapped header: Byron era_tag {era_tag} unsupported by decode_wrapped_block_header"
        ))),
        n => Err(SerializationError::CborDecode(format!(
            "wrapped header: unknown era_tag {n}"
        ))),
    }
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

    // ── Issue #654: decode_wrapped_block_header (eager validation) ────────

    /// Extract the inner-header bytes from a full block test vector by
    /// walking the outer envelope + the first array element of the
    /// inner block (`[header, tx_bodies, ...]`).
    fn extract_inner_header_bytes_from_block_vector(full_block_cbor: &[u8]) -> Vec<u8> {
        // Outer wrap is [era_tag(uint), inner_block_array]. We want the bytes
        // of the FIRST element of the inner_block_array — the header.
        // For Shelley+ blocks the inner_block is just an array, not a
        // tag24-wrapped bytes — different shape from a `MsgRollForward`
        // header payload but we only need the inner header bytes here.
        use minicbor::Decoder;
        let mut d = Decoder::new(full_block_cbor);
        assert_eq!(d.array().unwrap(), Some(2), "outer wrap must be array(2)");
        let _era_tag = d.u64().unwrap();
        // Now positioned at the inner block array. Record start, walk into it.
        let inner_block_start = d.position();
        let inner_arr = d.array().unwrap();
        assert!(inner_arr.is_some(), "inner block must be array");
        // First element is the header. Record its start, then skip it to
        // discover its end.
        let header_start = d.position();
        d.skip().unwrap();
        let header_end = d.position();
        let _ = inner_block_start; // (only used for documentation)
        full_block_cbor[header_start..header_end].to_vec()
    }

    /// Wrap inner-header bytes into the N2N `MsgRollForward` payload shape
    /// `[era_tag(uint), tag(24)(bytes(inner))]` — the form the chainsync
    /// receive loop sees on the wire.
    fn wrap_inner_header(era_tag: u64, inner: &[u8]) -> Vec<u8> {
        use minicbor::Encoder;
        let mut out = Vec::new();
        let mut enc = Encoder::new(&mut out);
        enc.array(2).unwrap();
        enc.u64(era_tag).unwrap();
        enc.tag(minicbor::data::Tag::new(24)).unwrap();
        enc.bytes(inner).unwrap();
        out
    }

    /// Round-trip a Conway block's header through
    /// `decode_wrapped_block_header` and assert the slot + header_hash
    /// match what `decode_block` produces for the full block.
    #[test]
    fn decode_wrapped_block_header_roundtrips_conway_vector() {
        let cbor = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/test_vectors/conway.hex"
        ))
        .unwrap();
        let full_block = hex::decode(cbor.trim()).unwrap();
        let block = decode_block_minimal(&full_block).expect("full block decode");

        let inner = extract_inner_header_bytes_from_block_vector(&full_block);
        let wrapped = wrap_inner_header(7, &inner);
        let header = decode_wrapped_block_header(&wrapped).expect("wrapped header decode");

        assert_eq!(
            header.slot, block.header.slot,
            "slot must match the full-block decode"
        );
        assert_eq!(
            header.header_hash, block.header.header_hash,
            "header_hash must match the full-block decode"
        );
        assert_eq!(
            header.block_number, block.header.block_number,
            "block_number must match"
        );
    }

    /// Round-trip a Babbage block's header.
    #[test]
    fn decode_wrapped_block_header_roundtrips_babbage_vector() {
        let cbor = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/test_vectors/babbage.hex"
        ))
        .unwrap();
        let full_block = hex::decode(cbor.trim()).unwrap();
        let block = decode_block_minimal(&full_block).expect("full block decode");

        let inner = extract_inner_header_bytes_from_block_vector(&full_block);
        let wrapped = wrap_inner_header(6, &inner);
        let header = decode_wrapped_block_header(&wrapped).expect("wrapped header decode");

        assert_eq!(header.slot, block.header.slot);
        assert_eq!(header.header_hash, block.header.header_hash);
    }

    /// Round-trip a Shelley block's header.
    #[test]
    fn decode_wrapped_block_header_roundtrips_shelley_vector() {
        let cbor = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/test_vectors/shelley.hex"
        ))
        .unwrap();
        let full_block = hex::decode(cbor.trim()).unwrap();
        let block = decode_block_minimal(&full_block).expect("full block decode");

        let inner = extract_inner_header_bytes_from_block_vector(&full_block);
        let wrapped = wrap_inner_header(2, &inner);
        let header = decode_wrapped_block_header(&wrapped).expect("wrapped header decode");

        assert_eq!(header.slot, block.header.slot);
        assert_eq!(header.header_hash, block.header.header_hash);
    }

    /// Round-trip an Alonzo (Mary-shaped) block's header.
    #[test]
    fn decode_wrapped_block_header_roundtrips_mary_vector() {
        let cbor = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/test_vectors/mary.hex"
        ))
        .unwrap();
        let full_block = hex::decode(cbor.trim()).unwrap();
        let block = decode_block_minimal(&full_block).expect("full block decode");

        let inner = extract_inner_header_bytes_from_block_vector(&full_block);
        // Mary's HFC era tag is 4 → routes through decode_alonzo_block_header.
        let wrapped = wrap_inner_header(4, &inner);
        let header = decode_wrapped_block_header(&wrapped).expect("wrapped header decode");

        assert_eq!(header.slot, block.header.slot);
        assert_eq!(header.header_hash, block.header.header_hash);
    }

    /// Byron era_tags (0, 1) are rejected so the caller can route them to a
    /// PBFT-aware path instead of attempting Praos validation.
    #[test]
    fn decode_wrapped_block_header_rejects_byron_era_tags() {
        for byron_tag in [0u64, 1] {
            let wrapped = wrap_inner_header(byron_tag, b"dummy");
            let err = decode_wrapped_block_header(&wrapped).unwrap_err();
            let SerializationError::CborDecode(msg) = err else {
                panic!("expected CborDecode for byron tag {byron_tag}");
            };
            assert!(msg.contains("Byron"));
        }
    }

    /// Unknown era_tags are rejected with a structured error rather than
    /// being silently routed to one of the known decoders.
    #[test]
    fn decode_wrapped_block_header_rejects_unknown_era_tag() {
        let wrapped = wrap_inner_header(99, b"dummy");
        let err = decode_wrapped_block_header(&wrapped).unwrap_err();
        let SerializationError::CborDecode(msg) = err else {
            panic!("expected CborDecode for unknown era");
        };
        assert!(msg.contains("unknown era_tag"));
    }

    /// Malformed outer wrap (wrong tag) is rejected.
    #[test]
    fn decode_wrapped_block_header_rejects_wrong_tag() {
        use minicbor::Encoder;
        let mut out = Vec::new();
        let mut enc = Encoder::new(&mut out);
        enc.array(2).unwrap();
        enc.u64(7).unwrap();
        // tag(99) instead of tag(24)
        enc.tag(minicbor::data::Tag::new(99)).unwrap();
        enc.bytes(b"inner").unwrap();
        assert!(decode_wrapped_block_header(&out).is_err());
    }

    /// Empty input is rejected, not panicking.
    #[test]
    fn decode_wrapped_block_header_rejects_empty_input() {
        assert!(decode_wrapped_block_header(&[]).is_err());
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
