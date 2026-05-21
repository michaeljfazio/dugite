//! Cross-validation of `validate_block_body_hash` against real on-chain blocks.
//!
//! Issue #550 (audit #545 E5). The Haskell `bbHash` algorithm
//! (`Cardano.Ledger.Alonzo.BlockBody.hashAlonzoSegWits`) is:
//!
//! ```text
//! bbHash(c_0, ..., c_{N-1}) =
//!   blake2b_256( blake2b_256(c_0) || ... || blake2b_256(c_{N-1}) )
//! ```
//!
//! where each `c_i` is the raw CBOR byte sequence of one body component as
//! it appears on the wire. This test uses the same `.hex` fixtures as
//! `dugite-serialization::tests::real_blocks` (one block per Shelley+ era)
//! to verify the implementation byte-exactly.

use dugite_consensus::praos::{compute_block_body_hash, validate_block_body_hash};
use dugite_primitives::block::Block;
use dugite_serialization::{decode_block, extract_block_body_components};

/// Load a hex-encoded block fixture from the serialization crate's test_vectors.
fn load_vector(name: &str) -> Vec<u8> {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let path = format!("{manifest_dir}/../dugite-serialization/tests/test_vectors/{name}.hex");
    let hex_str = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("Failed to read fixture {path}: {e}"));
    hex::decode(hex_str.trim()).unwrap_or_else(|e| panic!("Invalid hex in {path}: {e}"))
}

/// Verify that the bbHash we compute from the per-component CBOR slices of a
/// real on-chain block exactly matches the `body_hash` claim in the block's
/// header.
///
/// This is the load-bearing guarantee: any disagreement here would cause
/// dugite to reject legitimate Cardano blocks (which is what the broken
/// algorithm did and why the wire-in was disabled in commit `62f2e9fef`).
fn check_real_block(name: &str) {
    let cbor = load_vector(name);
    let block: Block =
        decode_block(&cbor).unwrap_or_else(|e| panic!("{name}: dugite decode failed: {e}"));

    // 1) Per-component extractor must return Some.
    let components = extract_block_body_components(&cbor)
        .unwrap_or_else(|| panic!("{name}: extract_block_body_components returned None"));

    // 2) Pre-Alonzo blocks have 3 body components; Alonzo+ have 4.
    let expected_n = match name {
        "shelley" | "mary" => 3,
        "alonzo" | "babbage" | "conway" => 4,
        _ => panic!("unknown fixture {name}"),
    };
    assert_eq!(
        components.len(),
        expected_n,
        "{name}: expected {expected_n} body components, got {}",
        components.len()
    );

    // 3) compute_block_body_hash must match the header's claim byte-exactly.
    let computed = compute_block_body_hash(&components);
    assert_eq!(
        computed, block.header.body_hash,
        "{name}: computed bbHash != header.body_hash \
         (computed={computed}, header={})",
        block.header.body_hash
    );

    // 4) validate_block_body_hash on the raw block CBOR must accept it.
    validate_block_body_hash(&block.header, &cbor)
        .unwrap_or_else(|e| panic!("{name}: validate_block_body_hash rejected real block: {e}"));
}

#[test]
fn real_shelley_block_body_hash_matches_header() {
    check_real_block("shelley");
}

#[test]
fn real_mary_block_body_hash_matches_header() {
    check_real_block("mary");
}

#[test]
fn real_alonzo_block_body_hash_matches_header() {
    check_real_block("alonzo");
}

#[test]
fn real_babbage_block_body_hash_matches_header() {
    check_real_block("babbage");
}

#[test]
fn real_conway_block_body_hash_matches_header() {
    check_real_block("conway");
}

/// Negative case: tampering the body of a real block (without updating the
/// header's `body_hash`) must produce a `BodyHashMismatch`. This is the
/// substitution attack the wire-in protects against.
///
/// We do this by *swapping* the contents of two body components — the
/// outer CBOR framing stays valid (each component is still a complete CBOR
/// value at the right offset), but the per-component byte sequences
/// differ from the original, so `bbHash` must change.
#[test]
fn tampered_conway_block_body_is_rejected() {
    use dugite_consensus::praos::ConsensusError;
    let cbor = load_vector("conway");
    let block = decode_block(&cbor).expect("decode");

    let components = extract_block_body_components(&cbor).expect("components");
    assert_eq!(components.len(), 4, "Conway block must have 4 components");

    // Build a tampered buffer where component 2 (aux_data) and component 3
    // (invalid_transactions) are swapped in raw byte form. The structural
    // walker `extract_block_body_components` doesn't care about the *type*
    // of each slot — it only walks complete CBOR values — so the result is
    // still parseable, but the per-component hashes change.
    //
    // For Conway the test vector has aux_data=`a100d90103a080` (7 bytes)
    // and invalid_txs=`80` (1 byte). Swapping yields a different byte
    // sequence at offsets [12183..12184) and [12184..12190), both of which
    // are valid CBOR values, so re-extraction still succeeds.
    let header_end = components[0].as_ptr() as usize - cbor.as_ptr() as usize;
    let aux_offset = components[2].as_ptr() as usize - cbor.as_ptr() as usize;
    let inv_offset = components[3].as_ptr() as usize - cbor.as_ptr() as usize;
    let aux = components[2].to_vec();
    let inv = components[3].to_vec();

    let mut tampered = Vec::with_capacity(cbor.len());
    tampered.extend_from_slice(&cbor[..aux_offset]);
    tampered.extend_from_slice(&inv); // place inv where aux was
    tampered.extend_from_slice(&aux); // place aux where inv was
    tampered.extend_from_slice(&cbor[inv_offset + inv.len()..]);
    assert_eq!(
        tampered.len(),
        cbor.len(),
        "swap preserves total byte count"
    );
    let _ = header_end; // (suppress unused-var hint while keeping the offset visible above)

    let result = validate_block_body_hash(&block.header, &tampered);
    assert!(
        matches!(result, Err(ConsensusError::BodyHashMismatch { .. })),
        "expected BodyHashMismatch for tampered body, got: {result:?}"
    );
}
