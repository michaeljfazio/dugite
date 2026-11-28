//! A block whose inner array uses an indefinite or non-minimal head must go through the whole
//! block path: decode, body extraction, body hash and body size.
//!
//! Haskell decodes the block with `decodeRecordNamed "Block" (const blockSize)`
//! (`Block.hs:106`), which accepts `9f ... ff` and any head width (`98`, `99`, `9a`, `9b`), and
//! hashes the ORIGINAL bytes of each of the four body segments (`withSlice` /
//! `hashAlonzoSegWits`), so the block array framing never enters the hash.
//!
//! Before the fix `extract_block_body_components` returned `None` for these heads,
//! `validate_block_body_hash` then failed with `BodyExtractionFailed` and the sync pipeline
//! refused a block Haskell accepts.

use dugite_consensus::praos::{compute_block_body_hash, validate_block_body_hash};
use dugite_serialization::{
    compute_block_body_size_from_cbor, decode_block, decode_block_minimal, extract_block_body_cbor,
    extract_block_body_components,
};

fn load_vector(name: &str) -> Vec<u8> {
    let path = format!(
        "{}/../dugite-serialization/tests/test_vectors/{name}.hex",
        env!("CARGO_MANIFEST_DIR")
    );
    hex::decode(std::fs::read_to_string(&path).unwrap().trim()).unwrap()
}

/// Re-frame the inner block array of `[era, [header, c0..cN]]` with `head`
/// (`Some(bytes)` = a definite head to use, `None` = indefinite `9f ... ff`).
fn reframe(full: &[u8], head: Option<&[u8]>) -> Vec<u8> {
    let mut d = minicbor::Decoder::new(full);
    assert_eq!(d.array().unwrap(), Some(2));
    d.u64().unwrap();
    let inner_start = d.position();
    let n = d.array().unwrap().unwrap();
    let items_start = d.position();
    for _ in 0..n {
        d.skip().unwrap();
    }
    let items_end = d.position();
    let mut v = full[..inner_start].to_vec();
    match head {
        Some(h) => v.extend_from_slice(h),
        None => v.push(0x9f),
    }
    v.extend_from_slice(&full[items_start..items_end]);
    if head.is_none() {
        v.push(0xff);
    }
    v.extend_from_slice(&full[items_end..]);
    v
}

fn heads(n: u8) -> Vec<(&'static str, Option<Vec<u8>>)> {
    vec![
        ("definite", Some(vec![0x80 + n])),
        ("indefinite", None),
        ("98 (1-byte length)", Some(vec![0x98, n])),
        ("99 (2-byte length)", Some(vec![0x99, 0, n])),
        ("9a (4-byte length)", Some(vec![0x9a, 0, 0, 0, n])),
        (
            "9b (8-byte length)",
            Some(vec![0x9b, 0, 0, 0, 0, 0, 0, 0, n]),
        ),
    ]
}

fn check(vector: &str, n_inner: u8, can_decode: bool) {
    let full = load_vector(vector);
    let reference = extract_block_body_components(&full).expect("reference components");
    let reference_vec: Vec<Vec<u8>> = reference.iter().map(|c| c.to_vec()).collect();
    let reference_body = extract_block_body_cbor(&full).unwrap().to_vec();
    let header = decode_block(&full).unwrap().header;
    assert_eq!(compute_block_body_hash(&reference), header.body_hash);

    for (name, head) in heads(n_inner) {
        let blk = reframe(&full, head.as_deref());
        let comps = extract_block_body_components(&blk)
            .unwrap_or_else(|| panic!("{vector} {name}: extraction returned None"));
        let got: Vec<Vec<u8>> = comps.iter().map(|c| c.to_vec()).collect();
        assert_eq!(got, reference_vec, "{vector} {name}: segment bytes");
        assert_eq!(
            extract_block_body_cbor(&blk).unwrap(),
            &reference_body[..],
            "{vector} {name}: body bytes"
        );
        assert_eq!(
            compute_block_body_size_from_cbor(&blk),
            Some(header.body_size),
            "{vector} {name}: body size equals the header claim"
        );
        validate_block_body_hash(&header, &blk)
            .unwrap_or_else(|e| panic!("{vector} {name}: body hash check: {e}"));
        if can_decode {
            let b = decode_block(&blk).unwrap_or_else(|e| panic!("{vector} {name}: {e}"));
            assert_eq!(b.header.body_hash, header.body_hash);
            decode_block_minimal(&blk).unwrap_or_else(|e| panic!("{vector} {name}: minimal {e}"));
        }
    }
}

#[test]
fn conway_block_array_framings_decode_extract_and_hash() {
    check("conway", 5, true);
}

#[test]
fn babbage_block_array_framings_extract_and_hash() {
    // Babbage and earlier decoders are not part of the block-path fix; the body walkers are
    // shared by every Shelley+ era.
    check("babbage", 5, false);
}

/// Malformed framings still yield no components.
#[test]
fn malformed_framings_are_refused() {
    let full = load_vector("conway");
    // unterminated indefinite block array
    let mut blk = reframe(&full, None);
    let last = blk.len() - 1;
    // drop the break that follows the last component (the block array's own `ff`)
    assert_eq!(blk[last], 0xff);
    blk.pop();
    assert!(extract_block_body_components(&blk).is_none());
    // fewer than three body components
    let mut d = minicbor::Decoder::new(&full);
    d.array().unwrap();
    d.u64().unwrap();
    let inner = d.position();
    d.array().unwrap();
    let mut short = full[..inner].to_vec();
    short.push(0x83);
    short.extend_from_slice(&full[inner + 1..]);
    assert!(extract_block_body_components(&short).is_none());
}
