use crate::cbor::*;
use dugite_primitives::block::{Block, BlockHeader, OperationalCert, ProtocolVersion, VrfOutput};
use dugite_primitives::hash::{blake2b_256, Hash32};
use dugite_primitives::transaction::Transaction;

use super::transaction::{
    encode_auxiliary_data, encode_transaction_body_for_era, encode_witness_set_for_era,
};

/// Encode an operational certificate: [hot_vkey, sequence_number, kes_period, sigma]
pub fn encode_operational_cert(cert: &OperationalCert) -> Vec<u8> {
    let mut buf = encode_array_header(4);
    buf.extend(encode_bytes(&cert.hot_vkey));
    buf.extend(encode_uint(cert.sequence_number));
    buf.extend(encode_uint(cert.kes_period));
    buf.extend(encode_bytes(&cert.sigma));
    buf
}

/// Encode a VRF result: [output, proof]
pub fn encode_vrf_result(vrf: &VrfOutput) -> Vec<u8> {
    let mut buf = encode_array_header(2);
    buf.extend(encode_bytes(&vrf.output));
    buf.extend(encode_bytes(&vrf.proof));
    buf
}

/// Encode a protocol version: [major, minor]
pub fn encode_protocol_version(pv: &ProtocolVersion) -> Vec<u8> {
    let mut buf = encode_array_header(2);
    buf.extend(encode_uint(pv.major));
    buf.extend(encode_uint(pv.minor));
    buf
}

/// Encode a block header body (the part that gets signed by KES).
///
/// [block_number, slot, prev_hash, issuer_vkey, vrf_vkey, vrf_result,
///  body_size, body_hash, operational_cert, protocol_version]
///
/// The `prev_hash` field is a CBOR nullable hash (Haskell's `PrevHash`):
/// the first block after genesis encodes it as CBOR null (`0xF6`), all
/// subsequent blocks encode it as a 32-byte bytestring. We use the domain
/// sentinel `Hash32::ZERO` to mean "no previous / genesis" — blake2b_256
/// will never collide with all zeros.
pub fn encode_block_header_body(header: &BlockHeader) -> Vec<u8> {
    let mut buf = encode_array_header(10);
    buf.extend(encode_uint(header.block_number.0));
    buf.extend(encode_uint(header.slot.0));
    if header.prev_hash == Hash32::ZERO {
        buf.extend(encode_null());
    } else {
        buf.extend(encode_hash32(&header.prev_hash));
    }
    buf.extend(encode_bytes(&header.issuer_vkey));
    buf.extend(encode_bytes(&header.vrf_vkey));
    buf.extend(encode_vrf_result(&header.vrf_result));
    buf.extend(encode_uint(header.body_size));
    buf.extend(encode_hash32(&header.body_hash));
    buf.extend(encode_operational_cert(&header.operational_cert));
    buf.extend(encode_protocol_version(&header.protocol_version));
    buf
}

/// Encode a complete block header: [header_body, body_signature]
///
/// The `kes_signature` parameter is the KES signature over the header body.
pub fn encode_block_header(header: &BlockHeader, kes_signature: &[u8]) -> Vec<u8> {
    let mut buf = encode_array_header(2);
    buf.extend(encode_block_header_body(header));
    buf.extend(encode_bytes(kes_signature));
    buf
}

/// Encode a complete Shelley+ era block.
///
/// Block = [storage_era_tag, [header, tx_bodies, tx_witness_sets, aux_data_map, invalid_txs]]
///
/// Uses **on-disk/ImmutableDB storage era tags**, which differ from the HFC NS
/// indices used in the N2N ChainSync header wire format:
///
/// | Era     | Storage tag (this fn) | HFC NS index (ChainSync header) |
/// |---------|-----------------------|---------------------------------|
/// | Byron   | 0                     | 0                               |
/// | Shelley | 2                     | 1                               |
/// | Allegra | 3                     | 2                               |
/// | Mary    | 4                     | 3                               |
/// | Alonzo  | 5                     | 4                               |
/// | Babbage | 6                     | 5                               |
/// | Conway  | 7                     | 6                               |
///
/// When serving headers over N2N ChainSync, `extract_header_for_chainsync` in
/// `dugite-network` converts the storage tag to the correct HFC NS index.
pub fn encode_block(block: &Block, kes_signature: &[u8]) -> Vec<u8> {
    let era_tag = match block.era {
        dugite_primitives::era::Era::Byron => 0u64,
        dugite_primitives::era::Era::Shelley => 2,
        dugite_primitives::era::Era::Allegra => 3,
        dugite_primitives::era::Era::Mary => 4,
        dugite_primitives::era::Era::Alonzo => 5,
        dugite_primitives::era::Era::Babbage => 6,
        dugite_primitives::era::Era::Conway => 7,
        dugite_primitives::era::Era::Dijkstra => 8,
    };

    // Outer array: [era_tag, block_content]
    let mut buf = encode_array_header(2);
    buf.extend(encode_uint(era_tag));

    // Block content: [header, tx_bodies, tx_witness_sets, aux_data_map, invalid_txs]
    buf.extend(encode_array_header(5));

    // Header
    buf.extend(encode_block_header(&block.header, kes_signature));

    // Transaction bodies — prefer preserved raw CBOR from the original
    // transaction to avoid re-serialization mismatches that would invalidate
    // witness signatures (the tx hash is blake2b-256 of the body CBOR).
    buf.extend(encode_array_header(block.transactions.len()));
    for tx in &block.transactions {
        if let Some(raw) = &tx.raw_body_cbor {
            buf.extend_from_slice(raw);
        } else {
            buf.extend(encode_transaction_body_for_era(&tx.body, tx.era));
        }
    }

    // Transaction witness sets — prefer preserved raw CBOR to avoid encoding
    // differences (map vs array redeemers, definite vs indefinite lengths).
    buf.extend(encode_array_header(block.transactions.len()));
    for tx in &block.transactions {
        if let Some(raw) = &tx.raw_witness_cbor {
            buf.extend_from_slice(raw);
        } else {
            buf.extend(encode_witness_set_for_era(&tx.witness_set, tx.era));
        }
    }

    // Auxiliary data map: {tx_index: aux_data} — shared with
    // compute_block_body_hash so the wire segment and the h3 preimage can
    // never diverge.
    buf.extend(encode_aux_data_segment(&block.transactions));

    // Invalid transactions (indices of txs with is_valid=false)
    let invalid_indices: Vec<_> = block
        .transactions
        .iter()
        .enumerate()
        .filter(|(_, tx)| !tx.is_valid)
        .map(|(i, _)| i)
        .collect();
    buf.extend(encode_array_header(invalid_indices.len()));
    for idx in &invalid_indices {
        buf.extend(encode_uint(*idx as u64));
    }

    buf
}

/// Encode the block-body auxiliary-data segment: `{ tx_index => aux_data }`.
///
/// Shared by [`encode_block`] and [`compute_block_body_hash`] so the on-wire
/// segment and the h3 hash preimage can never diverge. Haskell builds this
/// map with `encodeFoldableMapEncoder` (Shelley/Alonzo `BlockBody.Internal`
/// `txSeqAuxDatas`), which calls `variableMapLenEncoding` — the SAME
/// definite-<=23 / indefinite->23 threshold as `encodeMap` (#932).
///
/// Per-entry values prefer each tx's preserved raw aux bytes (Haskell
/// `encodePreEncoded originalBytes`) to avoid ConflictingMetadataHash from
/// re-encoding differences — only the map framing is synthetic.
///
/// `pub` so dugite-node's forge `compute_body_size` sizes the segment via
/// this exact function instead of a hand-rolled duplicate (which used a
/// definite-length header and over-declared body_size by 1 byte for blocks
/// with >255 aux-carrying txs — #932 audit).
pub fn encode_aux_data_segment(transactions: &[Transaction]) -> Vec<u8> {
    let aux_entries: Vec<_> = transactions
        .iter()
        .enumerate()
        .filter_map(|(i, tx)| tx.auxiliary_data.as_ref().map(|aux| (i, aux)))
        .collect();
    let mut buf = encode_map_open(aux_entries.len());
    for (idx, aux) in &aux_entries {
        buf.extend(encode_uint(*idx as u64));
        if let Some(raw) = &aux.raw_cbor {
            buf.extend_from_slice(raw);
        } else {
            buf.extend(encode_auxiliary_data(aux));
        }
    }
    encode_map_close(&mut buf, aux_entries.len());
    buf
}

/// Compute the block body hash using the Alonzo+ segregated witness structure.
///
/// Per Haskell cardano-ledger, the block body hash is:
///   blake2b_256(h1 || h2 || h3 || h4)
/// where:
///   h1 = blake2b_256(CBOR array of transaction bodies)
///   h2 = blake2b_256(CBOR array of witness sets)
///   h3 = blake2b_256(CBOR map of {tx_index: auxiliary_data})
///   h4 = blake2b_256(CBOR array of invalid tx indices)
// NOTE: `transactions` is intentionally `&[Transaction]` rather than `&[&Transaction]`
// so callers can pass `block.transactions.as_slice()` directly.
pub fn compute_block_body_hash(transactions: &[Transaction]) -> Hash32 {
    // 1. Transaction bodies — prefer preserved raw CBOR from the original
    // transaction to ensure the body hash matches what the witnesses signed.
    let mut bodies_cbor = encode_array_header(transactions.len());
    for tx in transactions {
        if let Some(raw) = &tx.raw_body_cbor {
            bodies_cbor.extend_from_slice(raw);
        } else {
            bodies_cbor.extend(encode_transaction_body_for_era(&tx.body, tx.era));
        }
    }
    let h1 = blake2b_256(&bodies_cbor);

    // 2. Transaction witness sets — prefer preserved raw CBOR.
    let mut wits_cbor = encode_array_header(transactions.len());
    for tx in transactions {
        if let Some(raw) = &tx.raw_witness_cbor {
            wits_cbor.extend_from_slice(raw);
        } else {
            wits_cbor.extend(encode_witness_set_for_era(&tx.witness_set, tx.era));
        }
    }
    let h2 = blake2b_256(&wits_cbor);

    // 3. Auxiliary data map: {tx_index: aux_data} — same shared segment
    // encoder as encode_block.
    let aux_cbor = encode_aux_data_segment(transactions);
    let h3 = blake2b_256(&aux_cbor);

    // 4. Invalid transaction indices (txs with is_valid=false)
    let invalid_indices: Vec<_> = transactions
        .iter()
        .enumerate()
        .filter(|(_, tx)| !tx.is_valid)
        .map(|(i, _)| i)
        .collect();
    let mut isvalid_cbor = encode_array_header(invalid_indices.len());
    for idx in &invalid_indices {
        isvalid_cbor.extend(encode_uint(*idx as u64));
    }
    let h4 = blake2b_256(&isvalid_cbor);

    // Combine: blake2b_256(h1 || h2 || h3 || h4)
    let mut combined = Vec::with_capacity(128);
    combined.extend_from_slice(h1.as_bytes());
    combined.extend_from_slice(h2.as_bytes());
    combined.extend_from_slice(h3.as_bytes());
    combined.extend_from_slice(h4.as_bytes());
    blake2b_256(&combined)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cbor::encode_array_header;
    use dugite_primitives::block::{
        Block, BlockHeader, OperationalCert, ProtocolVersion, VrfOutput,
    };
    use dugite_primitives::era::Era;
    use dugite_primitives::hash::Hash32;
    use dugite_primitives::time::{BlockNo, SlotNo};
    use dugite_primitives::transaction::Transaction;

    // -----------------------------------------------------------------------
    // Helper: build a minimal BlockHeader for encoder tests.
    // All byte-vectors use recognisable fill bytes so mismatches are easy to spot.
    // -----------------------------------------------------------------------
    fn make_header() -> BlockHeader {
        BlockHeader {
            header_hash: Hash32::from_bytes([0xaa; 32]),
            prev_hash: Hash32::from_bytes([0xbb; 32]),
            issuer_vkey: vec![0x01; 32],
            vrf_vkey: vec![0x02; 32],
            vrf_result: VrfOutput {
                output: vec![0x03; 32],
                proof: vec![0x04; 80],
            },
            block_number: BlockNo(42),
            slot: SlotNo(1000),
            epoch_nonce: Hash32::ZERO,
            body_size: 512,
            body_hash: Hash32::from_bytes([0xcc; 32]),
            operational_cert: OperationalCert {
                hot_vkey: vec![0x05; 32],
                sequence_number: 7,
                kes_period: 3,
                sigma: vec![0x06; 64],
            },
            protocol_version: ProtocolVersion { major: 9, minor: 0 },
            kes_signature: vec![0x07; 448],
            nonce_vrf_output: vec![],
            nonce_vrf_proof: vec![],
            prev_nonce: None,
            raw_header_body: None,
        }
    }

    // -----------------------------------------------------------------------
    // Test: encode_operational_cert  →  array(4)
    // -----------------------------------------------------------------------

    #[test]
    fn test_encode_operational_cert_is_array4() {
        let cert = OperationalCert {
            hot_vkey: vec![0xde; 32],
            sequence_number: 5,
            kes_period: 10,
            sigma: vec![0xad; 64],
        };
        let encoded = encode_operational_cert(&cert);

        // First byte must be array(4) = 0x84
        assert_eq!(
            encoded[0], 0x84,
            "operational cert must start with CBOR array(4) header 0x84"
        );
    }

    #[test]
    fn test_encode_operational_cert_sequence_number_and_kes_period() {
        // Use small values that appear unambiguously as single-byte CBOR uints.
        let cert = OperationalCert {
            hot_vkey: vec![0u8; 0],
            sequence_number: 0,
            kes_period: 1,
            sigma: vec![],
        };
        let encoded = encode_operational_cert(&cert);
        // array(4) | bytes(0) | uint(0) | uint(1) | bytes(0)
        // 0x84 0x40 0x00 0x01 0x40
        assert_eq!(encoded, vec![0x84, 0x40, 0x00, 0x01, 0x40]);
    }

    // -----------------------------------------------------------------------
    // Test: encode_vrf_result  →  array(2)
    // -----------------------------------------------------------------------

    #[test]
    fn test_encode_vrf_result_is_array2() {
        let vrf = VrfOutput {
            output: vec![0xab; 32],
            proof: vec![0xcd; 80],
        };
        let encoded = encode_vrf_result(&vrf);

        // First byte must be array(2) = 0x82
        assert_eq!(
            encoded[0], 0x82,
            "VRF result must start with CBOR array(2) header 0x82"
        );
    }

    #[test]
    fn test_encode_vrf_result_empty() {
        // Empty output and proof — useful baseline.
        let vrf = VrfOutput {
            output: vec![],
            proof: vec![],
        };
        let encoded = encode_vrf_result(&vrf);
        // array(2) | bytes(0) | bytes(0)  →  0x82 0x40 0x40
        assert_eq!(encoded, vec![0x82, 0x40, 0x40]);
    }

    #[test]
    fn test_encode_vrf_result_contains_both_fields() {
        let output_bytes = vec![0x11; 3];
        let proof_bytes = vec![0x22; 5];
        let vrf = VrfOutput {
            output: output_bytes.clone(),
            proof: proof_bytes.clone(),
        };
        let encoded = encode_vrf_result(&vrf);

        // Manually reconstruct expected bytes:
        // array(2) + bytes(3) + <output> + bytes(5) + <proof>
        let mut expected = vec![0x82_u8];
        expected.push(0x43); // bstr len 3
        expected.extend_from_slice(&output_bytes);
        expected.push(0x45); // bstr len 5
        expected.extend_from_slice(&proof_bytes);
        assert_eq!(encoded, expected);
    }

    // -----------------------------------------------------------------------
    // Test: encode_protocol_version  →  array(2) with major/minor
    // -----------------------------------------------------------------------

    #[test]
    fn test_encode_protocol_version_is_array2() {
        let pv = ProtocolVersion { major: 9, minor: 0 };
        let encoded = encode_protocol_version(&pv);
        assert_eq!(
            encoded[0], 0x82,
            "protocol version must start with CBOR array(2) header 0x82"
        );
    }

    #[test]
    fn test_encode_protocol_version_major_minor_values() {
        let pv = ProtocolVersion { major: 9, minor: 2 };
        let encoded = encode_protocol_version(&pv);
        // array(2) | uint(9) | uint(2)  →  0x82 0x09 0x02
        assert_eq!(encoded, vec![0x82, 0x09, 0x02]);
    }

    #[test]
    fn test_encode_protocol_version_large_minor() {
        // minor = 300 requires 3 CBOR bytes (0x19 0x01 0x2c)
        let pv = ProtocolVersion {
            major: 7,
            minor: 300,
        };
        let encoded = encode_protocol_version(&pv);
        assert_eq!(encoded[0], 0x82);
        assert_eq!(encoded[1], 0x07); // major = 7
        assert_eq!(encoded[2], 0x19); // uint 2-byte follows
        assert_eq!(encoded[3..5], [0x01, 0x2c]); // 300 in big-endian
    }

    // -----------------------------------------------------------------------
    // Test: encode_block_header_body  →  array(10)
    // -----------------------------------------------------------------------

    #[test]
    fn test_encode_block_header_body_starts_with_array10() {
        let header = make_header();
        let encoded = encode_block_header_body(&header);
        // array(10) = 0x8a
        assert_eq!(
            encoded[0], 0x8a,
            "header body must start with CBOR array(10) header 0x8a"
        );
    }

    #[test]
    fn test_encode_block_header_body_block_number_at_index1() {
        // block_number = 42  →  single-byte CBOR uint 0x18 0x2a
        let header = make_header();
        let encoded = encode_block_header_body(&header);
        // byte 0 is 0x8a (array header); byte 1 starts block_number
        assert_eq!(encoded[1], 0x18, "block_number uint prefix");
        assert_eq!(encoded[2], 42, "block_number value");
    }

    #[test]
    fn test_encode_block_header_body_slot_after_block_number() {
        // slot = 1000  →  0x19 0x03 0xe8
        let header = make_header();
        let encoded = encode_block_header_body(&header);
        // skip array(10): 1 byte, then block_number (2 bytes) → offset 3
        assert_eq!(encoded[3], 0x19, "slot uint prefix");
        let slot_val = u16::from_be_bytes([encoded[4], encoded[5]]);
        assert_eq!(slot_val, 1000, "slot value");
    }

    // -----------------------------------------------------------------------
    // Regression: prev_hash must encode as CBOR null (0xF6) at genesis
    //
    // In cardano-ledger's `libs/cardano-protocol-tpraos/src/Cardano/Protocol/
    // TPraos/BHeader.hs`, the `PrevHash` type is encoded as:
    //     encCBOR GenesisHash   = encodeNull          -- 0xF6
    //     encCBOR (BlockHash h) = encCBOR h           -- 32-byte bstr
    // Haskell rejects the first block on the chain with a deserialisation
    // error if prev_hash is encoded as a 32-byte all-zero bytestring instead
    // of CBOR null.  Dugite uses the domain sentinel `Hash32::ZERO` to mean
    // "no previous block" and encodes that as CBOR null.
    // -----------------------------------------------------------------------

    #[test]
    fn test_encode_block_header_body_prev_hash_null_at_genesis() {
        // At genesis, prev_hash is Hash32::ZERO → must encode as CBOR null (0xF6).
        let mut header = make_header();
        header.prev_hash = Hash32::ZERO;
        let encoded = encode_block_header_body(&header);

        // Layout up to prev_hash:
        //   byte 0: 0x8a       (array(10) header)
        //   bytes 1..=2: block_number uint(42) = 0x18 0x2a
        //   bytes 3..=5: slot uint(1000) = 0x19 0x03 0xe8
        //   byte 6: prev_hash encoding starts here
        assert_eq!(
            encoded[6], 0xF6,
            "prev_hash at genesis must encode as CBOR null (0xF6), not a 32-byte bstr; \
             Haskell's PrevHash.GenesisHash maps to encodeNull in cardano-ledger \
             BHeader.hs"
        );
    }

    #[test]
    fn test_encode_block_header_body_prev_hash_bstr_when_not_genesis() {
        // After genesis, prev_hash is a concrete 32-byte hash → must encode
        // as a CBOR byte-string of length 32 (major type 2, length 32 = 0x58 0x20).
        let header = make_header(); // make_header uses [0xbb; 32]
        let encoded = encode_block_header_body(&header);

        // byte 6: length prefix for a 32-byte bstr = 0x58 0x20
        assert_eq!(
            encoded[6], 0x58,
            "prev_hash non-genesis must be a bstr(32) — major type 2, 1-byte length"
        );
        assert_eq!(encoded[7], 0x20, "bstr length must be 32");
        // bytes 8..40 must match the prev_hash bytes
        assert_eq!(
            &encoded[8..40],
            &[0xbb; 32],
            "prev_hash bstr contents must match header.prev_hash bytes"
        );
    }

    #[test]
    fn test_encode_block_header_body_roundtrip_genesis() {
        // Sanity: decode the encoded header body via minicbor and confirm the
        // PrevHash field reads back as null (matching cardano-ledger's
        // `instance DecCBOR PrevHash` which peeks TypeNull to reconstruct
        // GenesisHash).
        let mut header = make_header();
        header.prev_hash = Hash32::ZERO;
        let encoded = encode_block_header_body(&header);

        let mut dec = minicbor::Decoder::new(&encoded);
        assert_eq!(dec.array().unwrap(), Some(10), "header body array(10)");
        let _block_number = dec.u64().unwrap();
        let _slot = dec.u64().unwrap();
        // Next token must be CBOR null for PrevHash = GenesisHash.
        assert_eq!(
            dec.datatype().unwrap(),
            minicbor::data::Type::Null,
            "third header body element must be null at genesis"
        );
        dec.null().unwrap(); // consume the null
    }

    // -----------------------------------------------------------------------
    // Test: encode_block_header  →  array(2): [header_body, kes_sig]
    // -----------------------------------------------------------------------

    #[test]
    fn test_encode_block_header_starts_with_array2() {
        let header = make_header();
        let kes_sig = vec![0xff; 448];
        let encoded = encode_block_header(&header, &kes_sig);
        assert_eq!(
            encoded[0], 0x82,
            "block header must start with CBOR array(2) header 0x82"
        );
    }

    #[test]
    fn test_encode_block_header_body_is_first_element() {
        let header = make_header();
        let kes_sig = vec![0xee; 448];
        let encoded = encode_block_header(&header, &kes_sig);

        // Second byte (index 1) should be the start of the header body, which
        // itself starts with array(10) = 0x8a.
        assert_eq!(
            encoded[1], 0x8a,
            "second element of block header must be the header body array(10)"
        );
    }

    #[test]
    fn test_encode_block_header_kes_sig_is_second_element() {
        let header = make_header();
        let kes_sig = vec![0xab; 3];
        let header_body_len = encode_block_header_body(&header).len();

        let encoded = encode_block_header(&header, &kes_sig);

        // After outer array(2) header (1 byte) and the full header body,
        // the next bytes must encode the KES signature as a CBOR byte string.
        let sig_offset = 1 + header_body_len;
        // bytes(3) → 0x43
        assert_eq!(
            encoded[sig_offset], 0x43,
            "KES signature must be encoded as bytes(3)"
        );
        assert_eq!(&encoded[sig_offset + 1..sig_offset + 4], &[0xab; 3]);
    }

    // -----------------------------------------------------------------------
    // Test: encode_block  →  era tags + outer array(2) + inner array(5)
    // -----------------------------------------------------------------------

    fn make_block(era: Era) -> Block {
        Block {
            header: make_header(),
            transactions: vec![],
            era,
            raw_cbor: None,
        }
    }

    /// Helper: decode the era tag from the encoded block (the uint immediately
    /// after the outer array(2) header byte).
    fn decode_era_tag(encoded: &[u8]) -> u64 {
        // encoded[0] = 0x82 (array(2))
        // encoded[1] starts the era tag uint
        if encoded[1] < 0x18 {
            // small uint (0–23) inline
            encoded[1] as u64
        } else if encoded[1] == 0x18 {
            encoded[2] as u64
        } else {
            panic!("unexpected era tag encoding");
        }
    }

    #[test]
    fn test_encode_block_outer_array2() {
        let block = make_block(Era::Conway);
        let kes_sig = vec![];
        let encoded = encode_block(&block, &kes_sig);
        assert_eq!(
            encoded[0], 0x82,
            "block must start with CBOR outer array(2) header 0x82"
        );
    }

    #[test]
    fn test_encode_block_era_tag_shelley() {
        let encoded = encode_block(&make_block(Era::Shelley), &[]);
        assert_eq!(decode_era_tag(&encoded), 2, "Shelley era tag must be 2");
    }

    #[test]
    fn test_encode_block_era_tag_allegra() {
        let encoded = encode_block(&make_block(Era::Allegra), &[]);
        assert_eq!(decode_era_tag(&encoded), 3, "Allegra era tag must be 3");
    }

    #[test]
    fn test_encode_block_era_tag_mary() {
        let encoded = encode_block(&make_block(Era::Mary), &[]);
        assert_eq!(decode_era_tag(&encoded), 4, "Mary era tag must be 4");
    }

    #[test]
    fn test_encode_block_era_tag_alonzo() {
        let encoded = encode_block(&make_block(Era::Alonzo), &[]);
        assert_eq!(decode_era_tag(&encoded), 5, "Alonzo era tag must be 5");
    }

    #[test]
    fn test_encode_block_era_tag_babbage() {
        let encoded = encode_block(&make_block(Era::Babbage), &[]);
        assert_eq!(decode_era_tag(&encoded), 6, "Babbage era tag must be 6");
    }

    #[test]
    fn test_encode_block_era_tag_conway() {
        let encoded = encode_block(&make_block(Era::Conway), &[]);
        assert_eq!(decode_era_tag(&encoded), 7, "Conway era tag must be 7");
    }

    #[test]
    fn test_encode_block_inner_array5() {
        let block = make_block(Era::Conway);
        let encoded = encode_block(&block, &[]);

        // outer array(2)=0x82 | era-tag uint(7)=0x07 | inner starts here
        // era tag 7 is a single-byte uint, so inner array starts at offset 2.
        assert_eq!(
            encoded[2], 0x85,
            "inner block content must be CBOR array(5) header 0x85"
        );
    }

    #[test]
    fn test_encode_block_inner_array5_header_body_at_offset3() {
        let block = make_block(Era::Shelley);
        let encoded = encode_block(&block, &[]);

        // outer array(2)=0x82 | era-tag uint(2)=0x02 | inner array(5)=0x85
        // | block_header array(2)=0x82 | ...
        // Offsets: 0 0x82, 1 0x02, 2 0x85, 3 block_header
        assert_eq!(
            encoded[3], 0x82,
            "first element of inner array must be the block header array(2)"
        );
    }

    // -----------------------------------------------------------------------
    // Test: compute_block_body_hash
    // -----------------------------------------------------------------------

    #[test]
    fn test_compute_block_body_hash_is_deterministic() {
        // Same input must always produce the same hash.
        let txs: Vec<Transaction> = vec![];
        let h1 = compute_block_body_hash(&txs);
        let h2 = compute_block_body_hash(&txs);
        assert_eq!(h1, h2, "body hash must be deterministic");
    }

    #[test]
    fn test_compute_block_body_hash_empty_txs_is_32_bytes() {
        let txs: Vec<Transaction> = vec![];
        let hash = compute_block_body_hash(&txs);
        // Hash32 is always 32 bytes.
        assert_eq!(hash.as_bytes().len(), 32);
    }

    #[test]
    fn test_compute_block_body_hash_differs_for_different_inputs() {
        // Empty vs. one valid transaction must yield different hashes.
        let empty: Vec<Transaction> = vec![];
        let one_tx = vec![Transaction::empty_with_hash(Hash32::ZERO)];

        let hash_empty = compute_block_body_hash(&empty);
        let hash_one = compute_block_body_hash(&one_tx);
        assert_ne!(
            hash_empty, hash_one,
            "body hash must differ when transactions differ"
        );
    }

    #[test]
    fn test_compute_block_body_hash_invalid_tx_index_affects_hash() {
        // A block with one valid tx vs. one invalid tx must produce different hashes
        // because the invalid-tx-indices component (h4) changes.
        let mut valid_tx = Transaction::empty_with_hash(Hash32::ZERO);
        valid_tx.is_valid = true;

        let mut invalid_tx = Transaction::empty_with_hash(Hash32::ZERO);
        invalid_tx.is_valid = false;

        let hash_valid = compute_block_body_hash(&[valid_tx]);
        let hash_invalid = compute_block_body_hash(&[invalid_tx]);
        assert_ne!(
            hash_valid, hash_invalid,
            "invalid tx index must change the body hash"
        );
    }

    #[test]
    fn test_compute_block_body_hash_known_empty_value() {
        // Regression guard: encode the exact CBOR structures for an empty block
        // body and verify the hash matches what we compute ourselves step-by-step.
        //
        // h1 = blake2b_256(CBOR array(0))  i.e. blake2b_256(0x80)
        // h2 = blake2b_256(CBOR array(0))
        // h3 = blake2b_256(CBOR map(0))    i.e. blake2b_256(0xa0)
        // h4 = blake2b_256(CBOR array(0))
        use crate::cbor::encode_map_header;
        use dugite_primitives::hash::blake2b_256;

        let h1 = blake2b_256(&encode_array_header(0));
        let h2 = blake2b_256(&encode_array_header(0));
        let h3 = blake2b_256(&encode_map_header(0));
        let h4 = blake2b_256(&encode_array_header(0));

        let mut combined = Vec::with_capacity(128);
        combined.extend_from_slice(h1.as_bytes());
        combined.extend_from_slice(h2.as_bytes());
        combined.extend_from_slice(h3.as_bytes());
        combined.extend_from_slice(h4.as_bytes());
        let expected = blake2b_256(&combined);

        let actual = compute_block_body_hash(&[]);
        assert_eq!(
            actual, expected,
            "empty block body hash must match step-by-step computation"
        );
    }

    // ── #932: Haskell `encodeFoldableMapEncoder` semantics for the block
    //    auxiliary-data segment ────────────────────────────────────────────
    //
    // The `{tx_index => aux_data}` block-body segment (hashed as h3) is
    // built by Haskell's `encodeFoldableMapEncoder`, which calls
    // `variableMapLenEncoding` — the SAME <= 23-definite / > 23-indefinite
    // threshold as `encodeMap` (oracle-verified 2026-07-31, Shelley/Alonzo
    // `BlockBody.Internal` `txSeqAuxDatas`). Values are the txs' preserved
    // raw aux bytes (`encodePreEncoded`), so only the map framing is
    // synthetic.

    /// A Conway tx whose auxiliary data carries the raw bytes {0 => 1}
    /// (0xa1 0x00 0x01) — spliced verbatim into the aux segment.
    fn aux_tx() -> Transaction {
        use dugite_primitives::transaction::{
            AuxiliaryData, TransactionBody, TransactionWitnessSet,
        };
        Transaction {
            hash: Hash32::ZERO,
            era: Era::Conway,
            body: TransactionBody::default(),
            witness_set: TransactionWitnessSet {
                vkey_witnesses: vec![],
                native_scripts: vec![],
                bootstrap_witnesses: vec![],
                plutus_v1_scripts: vec![],
                plutus_v2_scripts: vec![],
                plutus_v3_scripts: vec![],
                plutus_data: vec![],
                redeemers: vec![],
                raw_redeemers_cbor: None,
                raw_plutus_data_cbor: None,
                original_script_data_hash: None,
            },
            is_valid: true,
            auxiliary_data: Some(AuxiliaryData {
                metadata: std::collections::BTreeMap::new(),
                native_scripts: vec![],
                plutus_v1_scripts: vec![],
                plutus_v2_scripts: vec![],
                plutus_v3_scripts: vec![],
                raw_cbor: Some(vec![0xa1, 0x00, 0x01]),
            }),
            raw_cbor: None,
            raw_body_cbor: None,
            raw_witness_cbor: None,
        }
    }

    /// Explicitly-built aux segment `{i => raw}` in definite/indefinite form.
    fn raw_aux_segment(n: usize, indefinite: bool) -> Vec<u8> {
        let mut seg = if indefinite {
            vec![0xbf]
        } else {
            encode_map_header(n)
        };
        for i in 0..n {
            seg.extend(encode_uint(i as u64));
            seg.extend([0xa1, 0x00, 0x01]);
        }
        if indefinite {
            seg.push(0xff);
        }
        seg
    }

    /// The h3 preimage switches at the 23/24 boundary: compute the expected
    /// block body hash from explicitly-built segments and compare.
    #[test]
    fn block_body_hash_aux_map_23_vs_24_entries_header_switch() {
        for (n, indefinite) in [(23usize, false), (24usize, true)] {
            let txs: Vec<Transaction> = (0..n).map(|_| aux_tx()).collect();

            let body_cbor = encode_transaction_body_for_era(&txs[0].body, Era::Conway);
            let mut bodies = encode_array_header(n);
            for _ in 0..n {
                bodies.extend(&body_cbor);
            }
            let mut wits = encode_array_header(n);
            wits.resize(wits.len() + n, 0xa0);
            let aux = raw_aux_segment(n, indefinite);
            let invalid = encode_array_header(0);

            let mut combined = Vec::with_capacity(128);
            combined.extend_from_slice(blake2b_256(&bodies).as_bytes());
            combined.extend_from_slice(blake2b_256(&wits).as_bytes());
            combined.extend_from_slice(blake2b_256(&aux).as_bytes());
            combined.extend_from_slice(blake2b_256(&invalid).as_bytes());
            let expected = blake2b_256(&combined);

            assert_eq!(
                compute_block_body_hash(&txs),
                expected,
                "aux segment with {n} entries must use {} framing",
                if indefinite { "indefinite" } else { "definite" }
            );
        }
    }

    /// encode_block emits the same indefinite aux segment (the two call
    /// sites share one implementation): the exact 24-entry segment bytes
    /// must appear in the wire block, and a 256-entry segment must be
    /// 1 byte shorter than its definite form.
    #[test]
    fn encode_block_aux_map_24_and_256_entries_indefinite() {
        let txs: Vec<Transaction> = (0..24).map(|_| aux_tx()).collect();
        let block = Block {
            header: make_header(),
            transactions: txs,
            era: Era::Conway,
            raw_cbor: None,
        };
        let enc = encode_block(&block, &[0x07; 448]);
        let segment = raw_aux_segment(24, true);
        assert!(
            enc.windows(segment.len()).any(|w| w == segment),
            "24-entry indefinite aux segment must appear in the encoded block"
        );

        // 256 entries: indefinite (0xbf + entries + 0xff) is exactly 1 byte
        // shorter than definite (0xb9 0x01 0x00 + entries).
        let indefinite = raw_aux_segment(256, true);
        let definite = raw_aux_segment(256, false);
        assert_eq!(indefinite.len() + 1, definite.len());
        let txs256: Vec<Transaction> = (0..256).map(|_| aux_tx()).collect();
        let block256 = Block {
            header: make_header(),
            transactions: txs256,
            era: Era::Conway,
            raw_cbor: None,
        };
        let enc256 = encode_block(&block256, &[0x07; 448]);
        assert!(
            enc256.windows(indefinite.len()).any(|w| w == indefinite),
            "256-entry indefinite aux segment must appear in the encoded block"
        );
    }

    /// Decode-roundtrip: a full Conway block whose aux segment is indefinite
    /// (24 aux-carrying txs) decodes back with all 24 auxiliary-data entries
    /// attached to their transactions.
    #[test]
    fn indefinite_aux_segment_roundtrips_through_block_decoder() {
        let txs: Vec<Transaction> = (0..24).map(|_| aux_tx()).collect();
        let block = Block {
            header: make_header(),
            transactions: txs,
            era: Era::Conway,
            raw_cbor: None,
        };
        let enc = encode_block(&block, &[0x07; 448]);
        let decoded = crate::decode::decode_block(&enc).expect("block must decode");
        assert_eq!(decoded.transactions.len(), 24);
        for (i, tx) in decoded.transactions.iter().enumerate() {
            let aux = tx
                .auxiliary_data
                .as_ref()
                .unwrap_or_else(|| panic!("tx {i} must keep its auxiliary data"));
            assert_eq!(
                aux.raw_cbor.as_deref(),
                Some(&[0xa1, 0x00, 0x01][..]),
                "tx {i} aux raw bytes must round-trip"
            );
        }
    }
}
