//! Tests that decode real block CBOR from the preview testnet.
//!
//! Each `.hex` file in `test_vectors/` contains the hex-encoded CBOR of a real
//! block. These tests verify that the in-house [`decode_block`] succeeds on
//! production traffic and that the parsed era + structural fields are
//! consistent with the wire format.

use dugite_primitives::era::Era;
use dugite_serialization::decode_block;

/// Load a test vector hex file and return raw CBOR bytes.
fn load_vector(name: &str) -> Vec<u8> {
    let path = format!(
        "{}/tests/test_vectors/{}.hex",
        env!("CARGO_MANIFEST_DIR"),
        name
    );
    let hex_str = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("Failed to read test vector {path}: {e}"));
    hex::decode(hex_str.trim()).unwrap_or_else(|e| panic!("Invalid hex in {path}: {e}"))
}

/// Decode a real-chain test vector and assert the parsed era. Any future
/// regression in the in-house decoder that produces a different era or
/// fails to decode will fail this test.
fn smoke_test_block(name: &str, expected_era: Era) {
    let cbor = load_vector(name);
    let block = decode_block(&cbor).unwrap_or_else(|e| panic!("{name}: decode_block failed: {e}"));
    assert_eq!(block.era, expected_era, "{name}: era mismatch");

    // Every test vector has a non-zero header hash (i.e. blake2b actually ran).
    assert_ne!(
        block.header.header_hash,
        dugite_primitives::hash::Hash32::default(),
        "{name}: header_hash must not be all-zeroes"
    );
}

#[test]
fn test_shelley_block() {
    smoke_test_block("shelley", Era::Shelley);
}

#[test]
fn test_mary_block() {
    smoke_test_block("mary", Era::Mary);
}

#[test]
fn test_alonzo_block() {
    smoke_test_block("alonzo", Era::Alonzo);
}

#[test]
fn test_babbage_block() {
    smoke_test_block("babbage", Era::Babbage);
}

#[test]
fn test_conway_block() {
    smoke_test_block("conway", Era::Conway);
}

#[test]
fn test_decode_block_invalid_cbor() {
    let bad_cbor = vec![0xff, 0xfe, 0xfd, 0xfc];
    assert!(decode_block(&bad_cbor).is_err());
}

#[test]
fn test_decode_block_empty() {
    assert!(decode_block(&[]).is_err());
}

#[test]
fn test_decode_block_truncated() {
    let cbor = load_vector("conway");
    // Truncate to half — should fail gracefully
    let truncated = &cbor[..cbor.len() / 2];
    assert!(decode_block(truncated).is_err());
}

// ── BBODY body-size-from-cbor tests ──────────────────────────────────────────

/// For each Shelley+ test vector, verify that `compute_block_body_size_from_cbor`
/// returns the same value as the header's `body_size` field (the block producer's
/// claim).  This confirms byte-exact agreement on real chain data.
#[test]
fn test_body_size_from_cbor_matches_header_shelley() {
    body_size_cbor_check("shelley");
}

#[test]
fn test_body_size_from_cbor_matches_header_mary() {
    body_size_cbor_check("mary");
}

#[test]
fn test_body_size_from_cbor_matches_header_alonzo() {
    body_size_cbor_check("alonzo");
}

#[test]
fn test_body_size_from_cbor_matches_header_babbage() {
    body_size_cbor_check("babbage");
}

#[test]
fn test_body_size_from_cbor_matches_header_conway() {
    body_size_cbor_check("conway");
}

fn body_size_cbor_check(name: &str) {
    let cbor = load_vector(name);
    let block = decode_block(&cbor).unwrap();

    let actual = dugite_serialization::compute_block_body_size_from_cbor(&cbor)
        .unwrap_or_else(|| panic!("{name}: compute_block_body_size_from_cbor returned None"));

    assert_eq!(
        actual, block.header.body_size,
        "{name}: actual body size from CBOR ({actual}) != header claim ({})",
        block.header.body_size
    );
}

#[test]
fn test_body_size_from_cbor_returns_none_for_invalid() {
    // Empty input
    assert!(dugite_serialization::compute_block_body_size_from_cbor(&[]).is_none());
    // Garbage
    assert!(dugite_serialization::compute_block_body_size_from_cbor(&[0xff, 0xfe]).is_none());
}

// ── Dijkstra native dispatch tests (M4c, #466) ──────────────────────────────
//
// M4c added native Dijkstra (era tag 8) dispatch — a thin wrapper over the
// Conway decoder that stamps `Era::Dijkstra` on the result. These tests
// synthesize a Dijkstra-tagged block from the real Conway test vector (same
// wire structure, different outer era byte) to exercise the end-to-end
// dispatch path.

/// Conway block CBOR with the outer era tag flipped from 7 to 8 so it looks
/// like a Dijkstra block on the wire.
fn dijkstra_synthetic_from_conway() -> Vec<u8> {
    let mut cbor = load_vector("conway");
    assert_eq!(
        cbor[0], 0x82,
        "conway vector must start with array(2) header"
    );
    assert_eq!(cbor[1], 0x07, "conway vector must have era tag 7");
    cbor[1] = 0x08;
    cbor
}

#[test]
fn test_decode_block_dijkstra_native_dispatch() {
    // Era tag 8 must be routed to the in-house Dijkstra decoder (M4c) and
    // produce Era::Dijkstra.
    let cbor = dijkstra_synthetic_from_conway();
    let block = decode_block(&cbor).expect("Dijkstra-tagged block must decode via native dispatch");

    assert_eq!(block.era, Era::Dijkstra, "era must be Dijkstra");
    // raw_cbor MUST preserve the original tag-8 bytes so ChainDB serves the
    // wire-faithful block on N2N BlockFetch.
    let raw = block.raw_cbor.as_deref().expect("raw_cbor preserved");
    assert_eq!(raw[1], 0x08, "raw_cbor must retain the Dijkstra era tag");
}

#[test]
fn test_dijkstra_body_size_matches_header_for_conway_compatible() {
    // The header's claimed body_size must agree with the byte size we measure
    // from the raw CBOR.  Conway-compatible Dijkstra blocks (same structure,
    // different era tag) must round-trip this field correctly under the native
    // in-house decoder.
    let cbor = dijkstra_synthetic_from_conway();
    let block = decode_block(&cbor).expect("decode");
    let measured =
        dugite_serialization::compute_block_body_size_from_cbor(&cbor).expect("measured body size");
    assert_eq!(
        measured, block.header.body_size,
        "Conway-compatible Dijkstra block must round-trip body_size cleanly"
    );
}

#[test]
fn test_non_dijkstra_blocks_dispatch_correctly() {
    // Regression guard: the Dijkstra dispatch branch (era tag 8) must not
    // affect Conway (tag 7) or any earlier era.
    for (name, era) in [
        ("shelley", Era::Shelley),
        ("mary", Era::Mary),
        ("alonzo", Era::Alonzo),
        ("babbage", Era::Babbage),
        ("conway", Era::Conway),
    ] {
        let cbor = load_vector(name);
        let block = decode_block(&cbor).unwrap_or_else(|e| panic!("{name}: decode failed: {e}"));
        assert_eq!(
            block.era, era,
            "{name}: era must be unaffected by Dijkstra dispatch"
        );
    }
}
