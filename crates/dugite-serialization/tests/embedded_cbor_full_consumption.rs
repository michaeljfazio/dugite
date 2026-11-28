//! CBOR-in-CBOR (`#6.24(bytes)`) payloads must be consumed EXACTLY on the block path.
//!
//! cardano-ledger decodes the inline datum with `decodeBinaryData` -> `decodeFull'`, and the
//! script reference with `decodeCIC` -> `decodeFullAnnotator`; both go through
//! `decodeFullDecoder`, which fails with `DecoderErrorLeftover` when any byte follows the
//! embedded item (a trailing byte, a second value, a stray break). The samples named below
//! come from the Haskell-classified cardano-cbor-dataset (`data/manual-invalid`,
//! `babbage_transaction_output/manual-invalid`).

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
    hex::decode(std::fs::read_to_string(path).unwrap().trim()).unwrap()
}

fn addr() -> Vec<u8> {
    let mut a = vec![0x58, 29, 0x61];
    a.extend([0x11u8; 28]);
    a
}

/// `{0: addr, 1: 3 ADA, 2: [1, <embedded>]}` where `embedded` is the full tag-24 item.
fn output_with_inline_datum(embedded: &[u8]) -> Vec<u8> {
    let mut o = vec![0xa3, 0x00];
    o.extend(addr());
    o.extend([0x01, 0x1a, 0x00, 0x2d, 0xc6, 0xc0, 0x02, 0x82, 0x01]);
    o.extend_from_slice(embedded);
    o
}

/// `{0: addr, 1: 3 ADA, 3: <embedded script_ref>}`.
fn output_with_script_ref(embedded: &[u8]) -> Vec<u8> {
    let mut o = vec![0xa3, 0x00];
    o.extend(addr());
    o.extend([0x01, 0x1a, 0x00, 0x2d, 0xc6, 0xc0, 0x03]);
    o.extend_from_slice(embedded);
    o
}

fn tag24(inner: &[u8]) -> Vec<u8> {
    let mut v = vec![0xd8, 0x18];
    assert!(inner.len() < 256);
    if inner.len() < 24 {
        v.push(0x40 + inner.len() as u8);
    } else {
        v.extend([0x58, inner.len() as u8]);
    }
    v.extend_from_slice(inner);
    v
}

fn block_with_output(output: &[u8]) -> Vec<u8> {
    let full = load_vector("conway");
    let mut d = minicbor::Decoder::new(&full);
    d.array().unwrap();
    let era = d.u64().unwrap();
    d.array().unwrap();
    let hs = d.position();
    d.skip().unwrap();
    let header = full[hs..d.position()].to_vec();

    let mut body = vec![0xa3, 0x00, 0x81, 0x82, 0x58, 32];
    body.extend([0xAA; 32]);
    body.extend([0x00, 0x01, 0x81]);
    body.extend_from_slice(output);
    body.extend([0x02, 0x1a, 0x00, 0x02, 0x00, 0x00]);

    let mut blk = vec![0x82, era as u8, 0x85];
    blk.extend(header);
    blk.push(0x81);
    blk.extend(&body);
    blk.extend([0x81, 0xa0, 0xa0, 0x80]);
    blk
}

const INLINE_DATUM_OK: &[(&str, &[u8])] = &[
    // Plutus data `0` (`00`) and `[1, 2]` (`82 01 02`) wrapped in tag 24
    ("integer", &[0x00]),
    ("list", &[0x82, 0x01, 0x02]),
    ("indefinite list", &[0x9f, 0x01, 0x02, 0xff]),
];

const INLINE_DATUM_TRAILING: &[(&str, &[u8])] = &[
    // The three `data/manual-invalid` samples of the dataset, without the tag-24 head:
    // `d8184200ff`, `d818420000`, `d8184482010200`.
    ("trailing-break-after-data", &[0x00, 0xff]),
    ("trailing-byte-after-data", &[0x00, 0x00]),
    ("trailing-byte-after-list", &[0x82, 0x01, 0x02, 0x00]),
    ("two values", &[0x01, 0x02]),
];

#[test]
fn valid_inline_datums_still_decode_on_every_path() {
    for (name, data) in INLINE_DATUM_OK {
        let out = output_with_inline_datum(&tag24(data));
        for era in [5u16, 6, 7] {
            let o = decode_transaction_output(era, &out)
                .unwrap_or_else(|e| panic!("{name} era {era}: {e}"));
            match o.datum {
                dugite_primitives::transaction::OutputDatum::InlineDatum { raw_cbor, .. } => {
                    assert_eq!(raw_cbor.as_deref(), Some(*data), "datum bytes verbatim");
                }
                other => panic!("{name}: expected an inline datum, got {other:?}"),
            }
        }
        let blk = block_with_output(&out);
        assert!(decode_block(&blk).is_ok(), "{name}: decode_block");
        assert!(decode_block_minimal(&blk).is_ok(), "{name}: minimal");
    }
}

#[test]
fn inline_datum_with_trailing_bytes_is_rejected_on_every_path() {
    for (name, data) in INLINE_DATUM_TRAILING {
        let out = output_with_inline_datum(&tag24(data));
        for era in [5u16, 6, 7] {
            assert!(
                decode_transaction_output(era, &out).is_err(),
                "{name}: output era {era} must be rejected"
            );
        }
        let blk = block_with_output(&out);
        assert!(decode_block(&blk).is_err(), "{name}: decode_block");
        assert!(decode_block_minimal(&blk).is_err(), "{name}: minimal");

        let mut body = vec![0xa3, 0x00, 0x80, 0x01, 0x81];
        body.extend_from_slice(&out);
        body.extend([0x02, 0x00]);
        let mut tx = vec![0x84];
        tx.extend(&body);
        tx.extend([0xa0, 0xf5, 0xf6]);
        assert!(decode_transaction(6, &tx).is_err(), "{name}: submission");
    }
}

/// `[0, native_script]` with `native_script = [0, <28-byte key hash>]`
fn native_script_ref_inner() -> Vec<u8> {
    let mut s = vec![0x82, 0x00, 0x82, 0x00, 0x58, 0x1c];
    s.extend([0x33; 28]);
    s
}

#[test]
fn script_ref_trailing_bytes_rejected_valid_forms_kept() {
    let ok = native_script_ref_inner();
    // Definite and indefinite `[type, script]` forms both decode.
    let mut indef = vec![0x9f, 0x00, 0x82, 0x00, 0x58, 0x1c];
    indef.extend([0x33; 28]);
    indef.push(0xff);
    for (name, inner) in [("definite", ok.clone()), ("indefinite", indef)] {
        let out = output_with_script_ref(&tag24(&inner));
        for era in [5u16, 6] {
            let o = decode_transaction_output(era, &out)
                .unwrap_or_else(|e| panic!("{name} era {era}: {e}"));
            assert!(o.script_ref.is_some());
            assert_eq!(o.raw_cbor.as_deref(), Some(&out[..]));
        }
        let blk = block_with_output(&out);
        assert!(decode_block(&blk).is_ok(), "{name}: block");
    }

    // The three `babbage_transaction_output/manual-invalid/script-ref-*` samples.
    let mut trailing_value = ok.clone();
    trailing_value.push(0x00);
    let mut break_after = ok.clone();
    break_after.push(0xff);
    let mut second_script = ok.clone();
    second_script.extend_from_slice(&ok);
    for (name, inner) in [
        ("script-ref-trailing-value", trailing_value),
        ("script-ref-break-after-the-value", break_after),
        ("script-ref-second-script-after-the-value", second_script),
    ] {
        let out = output_with_script_ref(&tag24(&inner));
        for era in [5u16, 6, 7] {
            assert!(
                decode_transaction_output(era, &out).is_err(),
                "{name}: era {era}"
            );
        }
        let blk = block_with_output(&out);
        assert!(decode_block(&blk).is_err(), "{name}: decode_block");
        assert!(decode_block_minimal(&blk).is_err(), "{name}: minimal");
    }
}

/// A script's hash and the tx id depend on the exact bytes: the accepted forms keep them.
#[test]
fn accepted_embedded_payloads_keep_the_original_bytes() {
    let out = output_with_inline_datum(&tag24(&[0x9f, 0x01, 0x02, 0xff]));
    let mut body = vec![0xa3, 0x00, 0x80, 0x01, 0x81];
    body.extend_from_slice(&out);
    body.extend([0x02, 0x00]);
    let mut tx = vec![0x84];
    tx.extend(&body);
    tx.extend([0xa0, 0xf5, 0xf6]);
    let decoded = decode_transaction(6, &tx).unwrap();
    assert_eq!(decoded.hash, blake2b_256(&body));
}
