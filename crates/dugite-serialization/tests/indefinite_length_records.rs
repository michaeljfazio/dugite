//! Indefinite-length CBOR in fixed-shape records on the BLOCK and TRANSACTION-SUBMISSION
//! paths, driven through the real entry points (`decode_block`, `decode_block_minimal`,
//! `decode_transaction`, `decode_transaction_output`).
//!
//! cardano-ledger decodes every fixed-shape record with `decodeListLenOrIndef` (via
//! `decodeRecordNamed`/`decodeRecordSum`/`RecD`) and every sparse-keyed map with
//! `decodeMapLenOrIndef`; for transaction outputs `Babbage/TxOut.hs` `decodeBabbageTxOut`
//! matches `TypeMapLenIndef` and `oldTxOut` uses `decodeListLenOrIndef`. A valid block holding
//! such an encoding must therefore decode, and the transaction id (blake2b-256 of the ORIGINAL
//! body bytes) must be the one computed from the indefinite-length bytes.

use dugite_primitives::era::Era;
use dugite_primitives::hash::blake2b_256;
use dugite_serialization::{
    decode_block, decode_block_minimal, decode_transaction, decode_transaction_output,
};

fn load_vector(name: &str) -> Vec<u8> {
    let path = format!(
        "{}/tests/test_vectors/{}.hex",
        env!("CARGO_MANIFEST_DIR"),
        name
    );
    let hex_str = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    hex::decode(hex_str.trim()).unwrap()
}

/// The raw bytes of the header (first element of the inner block array) of a real block
/// vector, and its HFC era index.
fn header_of(vector: &str) -> (u64, Vec<u8>) {
    let full = load_vector(vector);
    let mut d = minicbor::Decoder::new(&full);
    assert_eq!(d.array().unwrap(), Some(2));
    let era_index = d.u64().unwrap();
    assert!(d.array().unwrap().is_some());
    let start = d.position();
    d.skip().unwrap();
    (era_index, full[start..d.position()].to_vec())
}

const ADDR: [u8; 29] = {
    let mut a = [0x11u8; 29];
    a[0] = 0x61; // enterprise address, mainnet, key hash credential
    a
};

fn bytes(b: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    assert!(b.len() < 256 && b.len() > 23);
    v.extend([0x58, b.len() as u8]);
    v.extend_from_slice(b);
    v
}

/// `bf 00 <addr> 01 <coin> ff`
fn indef_map_output() -> Vec<u8> {
    let mut o = vec![0xbf, 0x00];
    o.extend(bytes(&ADDR));
    o.extend([0x01, 0x1a, 0x00, 0x2d, 0xc6, 0xc0]);
    o.push(0xff);
    o
}

/// `9f <addr> <coin> ff` (legacy array form, indefinite)
fn indef_array_output() -> Vec<u8> {
    let mut o = vec![0x9f];
    o.extend(bytes(&ADDR));
    o.extend([0x1a, 0x00, 0x2d, 0xc6, 0xc0]);
    o.push(0xff);
    o
}

/// `9f <addr> <coin> <datum_hash> ff` (legacy array form, indefinite, datum hash)
fn indef_array_output_with_datum_hash() -> Vec<u8> {
    let mut o = vec![0x9f];
    o.extend(bytes(&ADDR));
    o.extend([0x1a, 0x00, 0x2d, 0xc6, 0xc0]);
    o.extend(bytes(&[0x77; 32]));
    o.push(0xff);
    o
}

/// Transaction body `{0: [[txid, 0]], 1: [output], 2: fee}` (+ optional collateral return).
fn body(outputs: &[Vec<u8>], collateral_return: Option<&[u8]>) -> Vec<u8> {
    let mut b = vec![if collateral_return.is_some() {
        0xa4
    } else {
        0xa3
    }];
    b.push(0x00);
    b.push(0x81);
    b.extend([0x82]);
    b.extend(bytes(&[0xAA; 32]));
    b.push(0x00);
    b.push(0x01);
    b.push(0x80 + outputs.len() as u8);
    for o in outputs {
        b.extend_from_slice(o);
    }
    b.extend([0x02, 0x1a, 0x00, 0x02, 0x00, 0x00]);
    if let Some(c) = collateral_return {
        b.extend([0x10]);
        b.extend_from_slice(c);
    }
    b
}

fn block_with(vector: &str, body: &[u8]) -> Vec<u8> {
    let (era_index, header) = header_of(vector);
    let mut inner = vec![0x85];
    inner.extend_from_slice(&header);
    inner.extend([0x81]); // tx_bodies
    inner.extend_from_slice(body);
    inner.extend([0x81, 0xa0]); // witness sets: [{}]
    inner.extend([0xa0]); // aux data map: {}
    inner.extend([0x80]); // invalid txs: []
    let mut full = vec![0x82, era_index as u8];
    full.extend(inner);
    full
}

fn standalone_tx(body: &[u8]) -> Vec<u8> {
    let mut t = vec![0x84];
    t.extend_from_slice(body);
    t.extend([0xa0, 0xf5, 0xf6]); // witnesses {}, is_valid true, no aux data
    t
}

fn check_block(vector: &str, expected_era: Era, outputs: Vec<Vec<u8>>, ret: Option<Vec<u8>>) {
    let body = body(&outputs, ret.as_deref());
    let cbor = block_with(vector, &body);
    for (name, blk) in [
        ("decode_block", decode_block(&cbor)),
        ("decode_block_minimal", decode_block_minimal(&cbor)),
    ] {
        let blk = blk.unwrap_or_else(|e| panic!("{vector}/{name}: {e}"));
        assert_eq!(blk.era, expected_era);
        assert_eq!(blk.transactions.len(), 1);
        let tx = &blk.transactions[0];
        // The id is the hash of the ORIGINAL body bytes, indefinite-length headers included.
        assert_eq!(tx.hash, blake2b_256(&body), "{vector}/{name}: tx id");
        assert_eq!(tx.raw_body_cbor.as_deref(), Some(&body[..]));
        assert_eq!(tx.body.outputs.len(), outputs.len());
        for (decoded, wire) in tx.body.outputs.iter().zip(&outputs) {
            assert_eq!(
                decoded.raw_cbor.as_deref(),
                Some(&wire[..]),
                "output bytes kept verbatim"
            );
            assert_eq!(decoded.value.coin.0, 3_000_000);
            assert_eq!(decoded.address.to_bytes(), ADDR.to_vec());
        }
        if let Some(ret) = &ret {
            let cr = tx
                .body
                .collateral_return
                .as_ref()
                .expect("collateral return");
            assert_eq!(cr.raw_cbor.as_deref(), Some(&ret[..]));
        }
    }
}

#[test]
fn conway_block_with_indefinite_output_map() {
    check_block("conway", Era::Conway, vec![indef_map_output()], None);
}

#[test]
fn conway_block_with_indefinite_legacy_array_output() {
    check_block("conway", Era::Conway, vec![indef_array_output()], None);
}

#[test]
fn conway_block_with_indefinite_collateral_return() {
    check_block(
        "conway",
        Era::Conway,
        vec![indef_map_output()],
        Some(indef_map_output()),
    );
}

#[test]
fn babbage_block_with_indefinite_output_map() {
    check_block("babbage", Era::Babbage, vec![indef_map_output()], None);
}

#[test]
fn babbage_block_with_indefinite_legacy_array_output() {
    check_block(
        "babbage",
        Era::Babbage,
        vec![indef_array_output(), indef_array_output_with_datum_hash()],
        None,
    );
}

#[test]
fn indefinite_legacy_array_with_datum_hash_decodes() {
    let out = indef_array_output_with_datum_hash();
    let decoded = decode_transaction_output(6, &out).unwrap();
    assert!(matches!(
        decoded.datum,
        dugite_primitives::transaction::OutputDatum::DatumHash(_)
    ));
    assert_eq!(decoded.raw_cbor.as_deref(), Some(&out[..]));
    assert!(decoded.is_legacy);
}

/// Transaction submission (the mempool admission decode: `decode_transaction(era, bytes)`).
#[test]
fn tx_submission_accepts_indefinite_outputs_and_keeps_the_tx_id() {
    for out in [
        indef_map_output(),
        indef_array_output(),
        indef_array_output_with_datum_hash(),
    ] {
        let b = body(&[out], None);
        let tx_cbor = standalone_tx(&b);
        let tx = decode_transaction(6, &tx_cbor).expect("Conway submission decode");
        assert_eq!(tx.hash, blake2b_256(&b));
        assert_eq!(tx.raw_cbor.as_deref(), Some(&tx_cbor[..]));
        let tx5 = decode_transaction(5, &tx_cbor).expect("Babbage submission decode");
        assert_eq!(tx5.hash, blake2b_256(&b));
    }
}

/// `Alonzo/Tx.hs` (cardano-ledger `f649f975`) peeks the token after the witnesses: a bool is
/// the `isValid` flag (4 elements), anything else means it was omitted (3 elements,
/// `IsValid True`); the record may be definite or indefinite but its size must match.
#[test]
fn standalone_tx_accepts_the_three_element_form_without_is_valid() {
    let b = body(&[indef_map_output()], None);
    let mut three = vec![0x83];
    three.extend_from_slice(&b);
    three.extend([0xa0, 0xf6]); // wits {}, aux null
    let tx = decode_transaction(6, &three).expect("3-element tx");
    assert!(tx.is_valid);
    assert_eq!(tx.hash, blake2b_256(&b));

    // indefinite framing, with and without the flag
    for (flag, wire) in [
        (true, {
            let mut v = vec![0x9f];
            v.extend_from_slice(&b);
            v.extend([0xa0, 0xf4, 0xf6, 0xff]);
            v
        }),
        (false, {
            let mut v = vec![0x9f];
            v.extend_from_slice(&b);
            v.extend([0xa0, 0xf6, 0xff]);
            v
        }),
    ] {
        let tx = decode_transaction(6, &wire).expect("indefinite tx");
        assert_eq!(tx.is_valid, !flag);
    }

    // sizes that do not match what was read, and a non-bool in the flag position, are refused
    let mut wrong = vec![0x84];
    wrong.extend_from_slice(&b);
    wrong.extend([0xa0, 0xf6]); // array(4) but only three elements follow
    assert!(decode_transaction(6, &wrong).is_err());
    let mut not_bool = vec![0x84];
    not_bool.extend_from_slice(&b);
    not_bool.extend([0xa0, 0x00, 0xf6]);
    assert!(decode_transaction(6, &not_bool).is_err());
    let mut excess = vec![0x9f];
    excess.extend_from_slice(&b);
    excess.extend([0xa0, 0xf5, 0xf6, 0x00, 0xff]);
    assert!(decode_transaction(6, &excess).is_err());
}

#[test]
fn standalone_output_decoders_accept_indefinite_forms_in_every_era() {
    for era in [5u16, 6, 7] {
        for out in [indef_map_output(), indef_array_output()] {
            decode_transaction_output(era, &out).unwrap_or_else(|e| panic!("era {era}: {e}"));
        }
    }
    // Alonzo and earlier only know the array form.
    for era in [2u16, 3, 4] {
        decode_transaction_output(era, &indef_array_output())
            .unwrap_or_else(|e| panic!("era {era}: {e}"));
    }
}

// ── Forms Haskell REJECTS must stay rejected ────────────────────────────────────────────

#[test]
fn rejected_output_forms_stay_rejected() {
    // indefinite legacy array with too many elements: "Excess terms in TxOut"
    let mut too_many = vec![0x9f];
    too_many.extend(bytes(&ADDR));
    too_many.extend([0x01]);
    too_many.extend(bytes(&[0x77; 32]));
    too_many.extend([0x00, 0xff]);
    // indefinite legacy array with one element
    let mut too_few = vec![0x9f];
    too_few.extend(bytes(&ADDR));
    too_few.push(0xff);
    // unterminated indefinite array
    let mut unterminated = indef_array_output();
    unterminated.pop();
    // indefinite map: unknown key 4
    let mut unknown_key = vec![0xbf, 0x00];
    unknown_key.extend(bytes(&ADDR));
    unknown_key.extend([0x01, 0x01, 0x04, 0x00, 0xff]);
    // indefinite map: duplicate key 1
    let mut dup_key = vec![0xbf, 0x00];
    dup_key.extend(bytes(&ADDR));
    dup_key.extend([0x01, 0x01, 0x01, 0x02, 0xff]);
    // 8-byte length map head (cborg `TypeMapLen64`): not matched by `decodeBabbageTxOut`
    let mut wide_head = vec![0xbb, 0, 0, 0, 0, 0, 0, 0, 0x02, 0x00];
    wide_head.extend(bytes(&ADDR));
    wide_head.extend([0x01, 0x01]);
    // a bare unsigned integer is neither form
    let not_an_output = vec![0x01];

    for (name, wire) in [
        ("too many elements", too_many),
        ("too few elements", too_few),
        ("unterminated", unterminated),
        ("unknown key", unknown_key),
        ("duplicate key", dup_key),
        ("0xbb map head", wide_head),
        ("not an output", not_an_output),
    ] {
        assert!(
            decode_transaction_output(6, &wire).is_err(),
            "{name} must be rejected in Conway"
        );
        assert!(
            decode_transaction_output(5, &wire).is_err(),
            "{name} must be rejected in Babbage"
        );
        let b = body(&[wire], None);
        assert!(
            decode_block(&block_with("conway", &b)).is_err(),
            "{name}: the block must not decode"
        );
    }
}
