//! Cross-validation: dugite-uplc PlutusData CBOR codec vs pragma-org/uplc.
//!
//! For each test input we construct a `dugite_uplc::Data`, encode it,
//! feed the bytes through pragma-org/uplc's decoder, re-encode the
//! pragma value, and assert byte-exact equality. This validates that
//! the two implementations agree on every layer (encode, decode,
//! re-encode of a decoded value) for canonical inputs produced by
//! dugite-uplc.
//!
//! Run with `cargo test --features cross-validate`.

#![cfg(feature = "cross-validate")]

use dugite_uplc::Data;
use num_bigint::BigInt;

use amaru_uplc::arena::Arena;
use amaru_uplc::data::PlutusData;

fn dugite_encode(d: &Data) -> Vec<u8> {
    d.to_cbor().expect("dugite-uplc encode")
}

fn pragma_encode(d: &PlutusData<'_>) -> Vec<u8> {
    // amaru-uplc speaks minicbor 0.25; use that version's `to_vec`
    // helper to encode through its `Encode` trait.
    minicbor_025::to_vec(d).expect("amaru-uplc encode")
}

/// Encode via dugite, decode via pragma, re-encode via pragma, compare.
/// Asserts that all three of (a) dugite's encoder, (b) pragma's decoder
/// for canonical input, and (c) pragma's encoder produce mutually
/// consistent bytes.
fn assert_cross(input: &Data) {
    let dbytes = dugite_encode(input);
    let arena = Arena::new();
    let pragma_view = PlutusData::from_cbor(&arena, &dbytes)
        .unwrap_or_else(|e| panic!("pragma decode failed for {input:?}: {e}"));
    let pbytes = pragma_encode(pragma_view);
    assert_eq!(
        dbytes, pbytes,
        "encoders diverge for {input:?}\n  dugite : {dbytes:02x?}\n  pragma : {pbytes:02x?}"
    );

    // Also exercise dugite's decoder on the same bytes, and dugite's
    // own re-encode round-trip.
    let d_view = Data::from_cbor(&dbytes).expect("dugite decode round-trip");
    let d_re = dugite_encode(&d_view);
    assert_eq!(dbytes, d_re, "dugite re-encode not idempotent");
}

// ---------------------------------------------------------------------------
// Integers
// ---------------------------------------------------------------------------

#[test]
fn integer_zero() {
    assert_cross(&Data::I(BigInt::from(0)));
}

#[test]
fn integer_small_positives() {
    for n in [
        1i64,
        23,
        24,
        100,
        255,
        256,
        65_535,
        65_536,
        1_000_000,
        i64::MAX,
    ] {
        assert_cross(&Data::I(BigInt::from(n)));
    }
}

#[test]
fn integer_small_negatives() {
    for n in [-1i64, -23, -24, -100, -256, -1_000_000, i64::MIN] {
        assert_cross(&Data::I(BigInt::from(n)));
    }
}

#[test]
fn integer_positive_bignum() {
    // 2^65 — requires the positive bignum tag.
    let pos: BigInt = BigInt::from(1u64) << 65;
    assert_cross(&Data::I(pos));
}

#[test]
fn integer_negative_bignum() {
    // -(2^65) — requires the negative bignum tag.
    let pos: BigInt = BigInt::from(1u64) << 65;
    let neg: BigInt = -pos;
    assert_cross(&Data::I(neg));
}

// ---------------------------------------------------------------------------
// ByteString
// ---------------------------------------------------------------------------

#[test]
fn bytestring_empty() {
    assert_cross(&Data::B(vec![]));
}

#[test]
fn bytestring_short() {
    assert_cross(&Data::B(vec![0xde, 0xad, 0xbe, 0xef]));
}

#[test]
fn bytestring_at_chunk_boundary() {
    assert_cross(&Data::B((0..64u8).collect()));
}

#[test]
fn bytestring_just_over_chunk_boundary() {
    assert_cross(&Data::B((0..65u8).collect()));
}

#[test]
fn bytestring_two_chunks() {
    assert_cross(&Data::B((0..128u8).collect()));
}

#[test]
fn bytestring_three_chunks() {
    assert_cross(&Data::B((0..200u8).collect()));
}

// ---------------------------------------------------------------------------
// Constr
// ---------------------------------------------------------------------------

#[test]
fn constr_small_tags() {
    for tag in [0u64, 1, 6] {
        assert_cross(&Data::Constr(BigInt::from(tag), vec![]));
        assert_cross(&Data::Constr(
            BigInt::from(tag),
            vec![Data::I(BigInt::from(42))],
        ));
    }
}

#[test]
fn constr_medium_tags() {
    for tag in [7u64, 50, 127] {
        assert_cross(&Data::Constr(BigInt::from(tag), vec![]));
        assert_cross(&Data::Constr(
            BigInt::from(tag),
            vec![Data::I(BigInt::from(42))],
        ));
    }
}

#[test]
fn constr_large_tags() {
    for tag in [128u64, 1000, u32::MAX as u64, u64::MAX] {
        assert_cross(&Data::Constr(BigInt::from(tag), vec![]));
        assert_cross(&Data::Constr(
            BigInt::from(tag),
            vec![Data::I(BigInt::from(1))],
        ));
    }
}

#[test]
fn constr_many_fields() {
    let fields: Vec<Data> = (0..10).map(|i| Data::I(BigInt::from(i))).collect();
    assert_cross(&Data::Constr(BigInt::from(0), fields));
}

// ---------------------------------------------------------------------------
// List
// ---------------------------------------------------------------------------

#[test]
fn list_empty() {
    assert_cross(&Data::List(vec![]));
}

#[test]
fn list_short() {
    assert_cross(&Data::List(vec![
        Data::I(BigInt::from(1)),
        Data::I(BigInt::from(2)),
    ]));
}

#[test]
fn list_at_chunk_boundary() {
    let items: Vec<Data> = (0..64).map(|i| Data::I(BigInt::from(i))).collect();
    assert_cross(&Data::List(items));
}

#[test]
fn list_indefinite_length() {
    let items: Vec<Data> = (0..100).map(|i| Data::I(BigInt::from(i))).collect();
    assert_cross(&Data::List(items));
}

// ---------------------------------------------------------------------------
// Map
// ---------------------------------------------------------------------------

#[test]
fn map_empty() {
    assert_cross(&Data::Map(vec![]));
}

#[test]
fn map_short() {
    assert_cross(&Data::Map(vec![
        (Data::B(vec![0x01]), Data::I(BigInt::from(10))),
        (Data::B(vec![0x02]), Data::I(BigInt::from(20))),
    ]));
}

// Note: pragma's decoder may treat duplicates differently; we only assert
// dugite's behaviour for that case in unit tests, not here.

// ---------------------------------------------------------------------------
// Nested
// ---------------------------------------------------------------------------

#[test]
fn nested_constr_inside_list() {
    assert_cross(&Data::List(vec![
        Data::Constr(BigInt::from(0), vec![Data::I(BigInt::from(1))]),
        Data::Constr(BigInt::from(1), vec![Data::I(BigInt::from(2))]),
    ]));
}

#[test]
fn nested_map_inside_constr() {
    assert_cross(&Data::Constr(
        BigInt::from(3),
        vec![Data::Map(vec![(
            Data::B(b"k".to_vec()),
            Data::List(vec![Data::I(BigInt::from(7))]),
        )])],
    ));
}

#[test]
fn deeply_nested() {
    let mut inner = Data::I(BigInt::from(99));
    for i in 0..20 {
        inner = Data::Constr(BigInt::from(i % 7), vec![inner]);
    }
    assert_cross(&inner);
}
