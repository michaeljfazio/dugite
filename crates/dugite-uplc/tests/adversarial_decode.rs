//! Adversarial decode-rejection tests for the public UPLC decode surface (#845).
//!
//! Complements the existing conformance corpus (999 UPLC cases), builtin-semantics
//! rejection fixtures, and the `fuzz/fuzz_targets/dugite_uplc_{program,data}_decode`
//! fuzzers: these are targeted, named regressions for the specific decode-rejection
//! invariants dugite must uphold byte-for-byte against Haskell's strict decoders.

use dugite_uplc::program::Program;
use dugite_uplc::term::{Constant, Term};

fn minimal_program() -> Program {
    Program {
        version: Program::version_triple(1, 0, 0),
        term: Term::Const(Constant::Integer(0.into())),
    }
}

#[test]
fn from_flat_rejects_trailing_bytes_toomuchspace() {
    // Haskell's flat `strictDecoder` raises `TooMuchSpace` for any bits beyond the
    // program's mandatory trailing filler (#822/#835). A trailing byte after a
    // canonical program must be rejected, not silently ignored.
    let mut flat = minimal_program().to_flat().expect("encode minimal program");
    let canonical_len = flat.len();
    flat.push(0xAB); // extraneous trailing byte
    let err = Program::from_flat(&flat).expect_err("trailing byte must be rejected");
    assert!(
        format!("{err}").to_lowercase().contains("space")
            || format!("{err}").to_lowercase().contains("trailing"),
        "expected a TooMuchSpace-style rejection, got: {err}"
    );
    // The canonical (untampered) bytes still decode.
    assert!(Program::from_flat(&flat[..canonical_len]).is_ok());
}

#[test]
fn from_flat_rejects_empty_and_truncated_input() {
    assert!(Program::from_flat(&[]).is_err(), "empty flat input must be rejected");
    let flat = minimal_program().to_flat().expect("encode");
    // Truncate to half — the term/version bits are incomplete.
    assert!(
        Program::from_flat(&flat[..flat.len() / 2]).is_err(),
        "truncated flat input must be rejected"
    );
}

#[test]
fn from_cbor_rejects_non_bytestring_and_trailing() {
    // On-chain scripts are a CBOR bytestring wrapping the flat program. A bare
    // (non-CBOR-wrapped) or wrong-major-type input must be rejected (#836).
    let flat = minimal_program().to_flat().expect("encode");
    // 0x80 = CBOR array(0) — wrong major type where a bytestring is required.
    assert!(Program::from_cbor(&[0x80]).is_err());
    // Empty input.
    assert!(Program::from_cbor(&[]).is_err());
    // A valid CBOR-wrapped program round-trips (positive control).
    let cbor = minimal_program().to_cbor().expect("cbor encode");
    assert!(Program::from_cbor(&cbor).is_ok());
    let _ = flat;
}

#[test]
fn data_from_cbor_rejects_malformed_input() {
    use dugite_uplc::data::Data;
    // Truncated indefinite-length / bad headers must not panic and must Err.
    for bad in [
        &[0x9f][..],       // indefinite array, never closed
        &[0xd8, 0x7a][..], // tag 122 header then EOF
        &[0xbf][..],       // indefinite map, never closed
        &[0x5f][..],       // indefinite bytes, never closed
    ] {
        assert!(
            Data::from_cbor(bad).is_err(),
            "malformed Data CBOR {bad:?} must be rejected, not accepted"
        );
    }
    // Positive control: a small Constr round-trips.
    let d = Data::constr(0, vec![Data::I(42.into()), Data::B(vec![1, 2, 3])]);
    let bytes = d.to_cbor().expect("encode");
    assert_eq!(Data::from_cbor(&bytes).expect("decode"), d);
}
