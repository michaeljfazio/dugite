//! Tests that decode real block CBOR from the preview testnet.
//!
//! Each `.hex` file in `test_vectors/` contains the hex-encoded CBOR of a real block.
//! These tests verify that `decode_block()` produces correct results by cross-checking
//! against `pallas_traverse::MultiEraBlock`.

use dugite_primitives::era::Era;
use dugite_serialization::decode_block;
use pallas_traverse::MultiEraBlock as PallasBlock;

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

/// Decode with both dugite and pallas, compare key fields.
fn cross_check_block(name: &str, expected_era: Era) {
    let cbor = load_vector(name);

    // Decode with dugite
    let block = decode_block(&cbor).unwrap_or_else(|e| panic!("{name}: decode_block failed: {e}"));

    // Decode with pallas
    let pallas_block =
        PallasBlock::decode(&cbor).unwrap_or_else(|e| panic!("{name}: pallas decode failed: {e}"));

    // Era
    assert_eq!(block.era, expected_era, "{name}: era mismatch");

    // Slot
    let pallas_slot = pallas_block.slot();
    assert_eq!(block.header.slot.0, pallas_slot, "{name}: slot mismatch");

    // Block number
    let pallas_bn = pallas_block.number();
    assert_eq!(
        block.header.block_number.0, pallas_bn,
        "{name}: block_number mismatch"
    );

    // Block header hash
    let pallas_hash = pallas_block.hash().to_vec();
    assert_eq!(
        block.header.header_hash.as_bytes(),
        &pallas_hash[..],
        "{name}: header_hash mismatch"
    );

    // Transaction count
    let pallas_tx_count = pallas_block.tx_count();
    assert_eq!(
        block.transactions.len(),
        pallas_tx_count,
        "{name}: tx_count mismatch"
    );

    // Transaction hashes (if any)
    for (i, pallas_tx) in pallas_block.txs().iter().enumerate() {
        let pallas_tx_hash = pallas_tx.hash().to_vec();
        let dugite_tx_hash = block.transactions[i].hash.as_bytes();
        assert_eq!(
            dugite_tx_hash,
            &pallas_tx_hash[..],
            "{name}: tx[{i}] hash mismatch"
        );
    }
}

#[test]
fn test_shelley_block() {
    cross_check_block("shelley", Era::Shelley);
}

#[test]
fn test_mary_block() {
    cross_check_block("mary", Era::Mary);
}

#[test]
fn test_alonzo_block() {
    cross_check_block("alonzo", Era::Alonzo);
}

#[test]
fn test_babbage_block() {
    cross_check_block("babbage", Era::Babbage);
}

#[test]
fn test_conway_block() {
    cross_check_block("conway", Era::Conway);
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
// M4c added `era_conway.rs` which decodes Dijkstra (era tag 8) natively via
// `decode_dijkstra_block` — a thin wrapper over the Conway decoder that stamps
// `Era::Dijkstra` on the result.  The old pallas byte-rewrite shim (tag 8→7)
// is kept only in the shadow `multi_era.rs` path for dual-decode comparison.
//
// These tests synthesize a Dijkstra-tagged block from the real Conway test
// vector (same wire structure, different outer era byte) to exercise the
// end-to-end dispatch path.

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
    // produce Era::Dijkstra — not fall through to the old Conway shim.
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
        assert_eq!(block.era, era, "{name}: era must be unaffected by Dijkstra dispatch");
    }
}

/// Deep field-by-field comparison of Babbage in-house vs pallas decode.
/// Validates: all tx body fields, witness set, and all header fields.
#[test]
fn test_babbage_field_by_field_vs_pallas() {
    let cbor = load_vector("babbage");

    // Direct decode bypassing dual_decode (which normalizes raw_cbor fields)
    let inhouse = dugite_serialization::decode::decode_block(&cbor).expect("inhouse decode");
    let pallas = dugite_serialization::multi_era::decode_block_with_byron_epoch_length(&cbor, 0)
        .expect("pallas decode");

    assert_eq!(inhouse.era, pallas.era, "era");
    assert_eq!(inhouse.header.slot, pallas.header.slot, "slot");
    assert_eq!(
        inhouse.header.block_number, pallas.header.block_number,
        "block_number"
    );
    assert_eq!(
        inhouse.transactions.len(),
        pallas.transactions.len(),
        "tx count"
    );

    for (i, (a, b)) in inhouse
        .transactions
        .iter()
        .zip(pallas.transactions.iter())
        .enumerate()
    {
        assert_eq!(a.hash, b.hash, "tx[{i}].hash");
        assert_eq!(a.is_valid, b.is_valid, "tx[{i}].is_valid");
        assert_eq!(a.era, b.era, "tx[{i}].era");
        assert_eq!(a.body.inputs, b.body.inputs, "tx[{i}].inputs");
        assert_eq!(
            a.body.outputs.len(),
            b.body.outputs.len(),
            "tx[{i}].output count"
        );
        for (j, (oa, ob)) in a.body.outputs.iter().zip(b.body.outputs.iter()).enumerate() {
            assert_eq!(oa.address, ob.address, "tx[{i}].out[{j}].address");
            assert_eq!(oa.value, ob.value, "tx[{i}].out[{j}].value");
            assert_eq!(oa.datum, ob.datum, "tx[{i}].out[{j}].datum");
            assert_eq!(oa.is_legacy, ob.is_legacy, "tx[{i}].out[{j}].is_legacy");
            assert_eq!(oa.script_ref, ob.script_ref, "tx[{i}].out[{j}].script_ref");
        }
        assert_eq!(a.body.fee, b.body.fee, "tx[{i}].fee");
        assert_eq!(a.body.collateral, b.body.collateral, "tx[{i}].collateral");
        assert_eq!(
            a.body
                .collateral_return
                .as_ref()
                .map(|o| (&o.address, &o.value)),
            b.body
                .collateral_return
                .as_ref()
                .map(|o| (&o.address, &o.value)),
            "tx[{i}].collateral_return"
        );
        assert_eq!(
            a.body.total_collateral, b.body.total_collateral,
            "tx[{i}].total_collateral"
        );
        assert_eq!(
            a.body.reference_inputs, b.body.reference_inputs,
            "tx[{i}].reference_inputs"
        );
        assert_eq!(a.body.mint, b.body.mint, "tx[{i}].mint");
        assert_eq!(
            a.body.script_data_hash, b.body.script_data_hash,
            "tx[{i}].script_data_hash"
        );
        assert_eq!(
            a.body.required_signers, b.body.required_signers,
            "tx[{i}].required_signers"
        );
        assert_eq!(
            a.body.withdrawals, b.body.withdrawals,
            "tx[{i}].withdrawals"
        );
        assert_eq!(
            a.body.certificates, b.body.certificates,
            "tx[{i}].certificates"
        );
        // Witnesses
        let aws = &a.witness_set;
        let bws = &b.witness_set;
        assert_eq!(aws.vkey_witnesses, bws.vkey_witnesses, "tx[{i}].ws.vkeys");
        assert_eq!(
            aws.native_scripts.len(),
            bws.native_scripts.len(),
            "tx[{i}].ws.native_scripts len"
        );
        assert_eq!(
            aws.plutus_v1_scripts, bws.plutus_v1_scripts,
            "tx[{i}].ws.plutus_v1"
        );
        assert_eq!(
            aws.plutus_v2_scripts, bws.plutus_v2_scripts,
            "tx[{i}].ws.plutus_v2"
        );
        assert_eq!(aws.redeemers, bws.redeemers, "tx[{i}].ws.redeemers");
        assert_eq!(aws.plutus_data, bws.plutus_data, "tx[{i}].ws.plutus_data");
    }
    // Block header
    assert_eq!(
        inhouse.header.prev_hash, pallas.header.prev_hash,
        "prev_hash"
    );
    assert_eq!(
        inhouse.header.body_hash, pallas.header.body_hash,
        "body_hash"
    );
    assert_eq!(
        inhouse.header.body_size, pallas.header.body_size,
        "body_size"
    );
    assert_eq!(
        inhouse.header.protocol_version, pallas.header.protocol_version,
        "protocol_version"
    );
    assert_eq!(
        inhouse.header.operational_cert, pallas.header.operational_cert,
        "opcert"
    );
    assert_eq!(
        inhouse.header.vrf_result, pallas.header.vrf_result,
        "vrf_result"
    );
    assert_eq!(
        inhouse.header.nonce_vrf_output, pallas.header.nonce_vrf_output,
        "nonce_vrf_output"
    );
    assert_eq!(
        inhouse.header.nonce_vrf_proof, pallas.header.nonce_vrf_proof,
        "nonce_vrf_proof"
    );
    assert_eq!(
        inhouse.header.kes_signature, pallas.header.kes_signature,
        "kes_signature"
    );
    assert_eq!(
        inhouse.header.issuer_vkey, pallas.header.issuer_vkey,
        "issuer_vkey"
    );
    assert_eq!(inhouse.header.vrf_vkey, pallas.header.vrf_vkey, "vrf_vkey");
}
