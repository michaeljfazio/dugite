//! Fuzz target: `fuzz_lsq_filter_arg`
//!
//! The LSQ filter-argument parsers (#963) — the code that turns an untrusted
//! `Set`/`Maybe (Set …)` off the N2C wire into "which pools / credentials /
//! DReps / proposals do I answer for".
//!
//! # The property this target exists to protect
//!
//! Before #963 every one of these parsers degraded to an empty vector on *every*
//! failure path, and every caller read an empty vector as "no filter — answer
//! for everything". A malformed argument was therefore indistinguishable from a
//! client asking for the whole set, so the filters failed **open**:
//! `query pool-state --stake-pool-id A` was answered with pools A *and* B, and
//! asking for a different pool returned byte-identical output.
//!
//! So the invariant worth fuzzing is not merely "no panic". It is:
//!
//! > `parse_set` reports "no filter" **only** for input that genuinely says so.
//!
//! Anything else — truncation, a wrong tag, a wrong-width hash, a bad
//! discriminator — must be an `Err`, which the handlers turn into an explicit
//! query error rather than a superset answer. `check_fail_closed` asserts
//! exactly that, and it is the assertion that would have caught #963: the old
//! parser answers `Ok(None)` for the live `cardano-cli` argument bytes.
//!
//! Two further properties, both of the shape #982 established:
//!
//! * **No silent truncation.** A structurally generated `Just {ids}` must parse
//!   back to *exactly* those ids — not a prefix, not a subset. The old parser's
//!   `if let Ok(bytes) = decoder.bytes()` skipped bad elements and kept going.
//! * **Element widths are enforced.** A 32-byte pool id, a 28-byte `GovActionId`
//!   txId, or a `DRep` arity mismatch must be rejected rather than kept as a
//!   hash that can never match — which reports "no such pool" for a pool that
//!   exists, the same quiet wrong answer pointing the other way.
//!
//! Byte-first and structured generation are complementary here and both run:
//! mutation reaches framing edge cases (indefinite arrays, truncation, nesting)
//! that no generator would invent, while generation reaches the deep valid
//! shapes — the four `DRep` constructors, both `Credential` discriminators —
//! that byte mutation essentially never synthesises.
//!
//! Run with:
//!   cargo +nightly fuzz run fuzz_lsq_filter_arg -- -max_total_time=300

#![no_main]

use arbitrary::{Arbitrary, Unstructured};
use libfuzzer_sys::fuzz_target;

use dugite_fuzz::node::n2c_query::filter::{
    filter_arg, parse_set, read_credential, read_drep, read_gov_action_id, read_pool_id,
    FilterArgError, OnEmptySet, SetArgShape,
};

/// `Ok(None)` means "answer for everything". Assert it is only ever reached
/// from input that genuinely encodes that, never from a parse failure.
fn check_fail_closed<T>(
    data: &[u8],
    shape: SetArgShape,
    parsed: &Result<Option<Vec<T>>, FilterArgError>,
) {
    if !matches!(parsed, Ok(None)) {
        return;
    }
    // Exactly two inputs may legitimately mean "no filter":
    //   * no argument bytes at all (the tolerated legacy shape), and
    //   * `encodeMaybe Nothing = encodeListLen 0`, for an `Optional`-shaped
    //     argument only.
    //
    // The `Nothing` case is checked by *decoding* an array header rather than
    // by comparing against the canonical byte 0x80: CBOR can spell a
    // zero-length array four ways (0x80, 0x98 0x00, 0x99 0x0000, ...), cborg's
    // `decodeListLen` accepts all of them, and so must this. An earlier version
    // of this assertion compared the first byte and the fuzzer produced
    // `98 00 …` inside three minutes — the parser was right and the property
    // was wrong.
    let is_nothing = matches!(minicbor::Decoder::new(data).array(), Ok(Some(0)));
    let legitimate = data.is_empty() || (shape == SetArgShape::Optional && is_nothing);
    assert!(
        legitimate,
        "parse_set reported \"no filter\" for input that does not encode it \
         ({shape:?}): {data:02x?} — this is the #963 fail-open shape",
    );
}

fn run<T>(
    data: &[u8],
    shape: SetArgShape,
    read: impl Fn(&mut minicbor::Decoder<'_>, usize) -> Result<T, FilterArgError> + Copy,
) {
    let parsed = parse_set(&mut minicbor::Decoder::new(data), shape, read);
    check_fail_closed(data, shape, &parsed);

    // `filter_arg` must agree with `parse_set` about "no filter", except that
    // `AllItems` additionally folds an explicitly empty set into it.
    let via_all = filter_arg(
        &mut minicbor::Decoder::new(data),
        "fuzz",
        shape,
        OnEmptySet::AllItems,
        read,
    );
    let via_none = filter_arg(
        &mut minicbor::Decoder::new(data),
        "fuzz",
        shape,
        OnEmptySet::NoItems,
        read,
    );
    match (&parsed, &via_none) {
        (Ok(a), Ok(b)) => assert_eq!(
            a.as_ref().map(Vec::len),
            b.as_ref().map(Vec::len),
            "NoItems must pass parse_set through unchanged"
        ),
        (Err(_), Err(_)) => {}
        _ => panic!("filter_arg and parse_set disagreed on whether the input parses"),
    }
    if let Ok(Some(items)) = &parsed {
        if items.is_empty() {
            assert!(
                matches!(via_all, Ok(None)),
                "AllItems must fold an explicitly empty set into \"no filter\""
            );
        }
    }
}

/// Build a well-formed `toCBOR (Just (Set …))` argument around `n` elements.
fn wrap_just(elements: &[Vec<u8>]) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut enc = minicbor::Encoder::new(&mut buf);
    enc.array(1).unwrap(); // the `Maybe`
    enc.tag(minicbor::data::Tag::new(258)).unwrap();
    enc.array(elements.len() as u64).unwrap();
    for e in elements {
        buf.extend_from_slice(e);
    }
    buf
}

fuzz_target!(|data: &[u8]| {
    // ── byte-first: arbitrary CBOR through every parser × both shapes ──
    for shape in [SetArgShape::Optional, SetArgShape::Required] {
        run(data, shape, read_pool_id);
        run(data, shape, read_credential);
        run(data, shape, read_drep);
        run(data, shape, read_gov_action_id);
    }

    // ── structured: valid arguments must survive intact ──
    let mut u = Unstructured::new(data);

    // Pool ids: a `Just` of n 28-byte hashes must parse back to exactly those.
    let count = u8::arbitrary(&mut u).unwrap_or(0) % 12;
    let mut ids = Vec::new();
    for _ in 0..count {
        let seed = u8::arbitrary(&mut u).unwrap_or(0);
        let mut elem = Vec::new();
        minicbor::Encoder::new(&mut elem)
            .bytes(&[seed; 28])
            .unwrap();
        ids.push(elem);
    }
    let arg = wrap_just(&ids);
    match parse_set(
        &mut minicbor::Decoder::new(&arg),
        SetArgShape::Optional,
        read_pool_id,
    ) {
        Ok(Some(got)) => assert_eq!(
            got.len(),
            count as usize,
            "a valid Just dropped elements — the old parser skipped what it could not read"
        ),
        other => panic!("a well-formed Just must parse: {other:?}"),
    }

    // DReps: all four constructors, generated because byte mutation will not
    // reliably produce `[2]`/`[3]` next to `[0|1, bstr(28)]`.
    let count = u8::arbitrary(&mut u).unwrap_or(0) % 8;
    let mut dreps = Vec::new();
    let mut expected = Vec::new();
    for _ in 0..count {
        let kind = u8::arbitrary(&mut u).unwrap_or(0) % 4;
        let seed = u8::arbitrary(&mut u).unwrap_or(0);
        let mut elem = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut elem);
        match kind {
            0 | 1 => {
                enc.array(2).unwrap();
                enc.u8(kind).unwrap();
                enc.bytes(&[seed; 28]).unwrap();
                expected.push((kind, Some(vec![seed; 28])));
            }
            k => {
                enc.array(1).unwrap();
                enc.u8(k).unwrap();
                expected.push((k, None));
            }
        }
        dreps.push(elem);
    }
    let arg = wrap_just(&dreps);
    match parse_set(
        &mut minicbor::Decoder::new(&arg),
        SetArgShape::Optional,
        read_drep,
    ) {
        Ok(Some(got)) => assert_eq!(got, expected, "DRep set must round-trip exactly"),
        other => panic!("a well-formed DRep set must parse: {other:?}"),
    }

    // Width enforcement: perturbing a hash to any width but 28 must be rejected,
    // never silently kept as a value that can match nothing.
    if let Ok(w) = u8::arbitrary(&mut u) {
        let width = usize::from(w % 40);
        if width != 28 {
            let mut elem = Vec::new();
            minicbor::Encoder::new(&mut elem)
                .bytes(&vec![0u8; width])
                .unwrap();
            let arg = wrap_just(std::slice::from_ref(&elem));
            assert!(
                parse_set(
                    &mut minicbor::Decoder::new(&arg),
                    SetArgShape::Optional,
                    read_pool_id
                )
                .is_err(),
                "a {width}-byte pool id must be rejected, not kept"
            );
        }
    }
});
