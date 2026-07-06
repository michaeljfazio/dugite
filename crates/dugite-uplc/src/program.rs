//! UPLC `Program` — the outer wrapper around a `Term`.
//!
//! A program is `(program major.minor.patch term)`. The on-chain wire
//! shape is:
//!
//!  1. A CBOR byte-string (major type 2) wrapping the flat-encoded
//!     program bytes.
//!  2. Inside the flat layer: the version triple as three `Natural`s,
//!     followed by the term.
//!
//! This module handles the CBOR ↔ flat-bytes boundary; the flat ↔ AST
//! boundary lives in `crate::flat`.

use crate::flat::bits::{BitReader, BitWriter};
use crate::flat::term::{decode_term, encode_term};
use crate::term::Term;
use crate::UplcError;

/// A complete UPLC program: a language version triple plus a body.
///
/// The version components are arbitrary-precision (`BigUint`), matching
/// Haskell's `Natural`-typed `Version` fields exactly (issue #842
/// residual) — Haskell's flat decoder never rejects or truncates a
/// version component on magnitude, so neither does this one. In
/// practice every real on-chain script declares a tiny version (`1.0.0`
/// or `1.1.0`); the only way to reach a value that wouldn't fit in a
/// `u64` is a deliberately adversarial flat blob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    pub version: (
        num_bigint::BigUint,
        num_bigint::BigUint,
        num_bigint::BigUint,
    ),
    pub term: Term,
}

impl Program {
    /// Decode a CBOR-wrapped flat-encoded UPLC program.
    ///
    /// The wire shape is a single CBOR byte-string (major type 2) whose
    /// payload is the flat-encoded program. The witness-set wire entry
    /// for a Plutus script is exactly these bytes (see
    /// `crate::tx_info_populate::script_ref_hash`).
    pub fn from_cbor(bytes: &[u8]) -> Result<Self, UplcError> {
        use minicbor::Decoder;
        let mut d = Decoder::new(bytes);
        let inner = d
            .bytes()
            .map_err(|e| UplcError::FlatDecode(format!("CBOR-bytes wrapper: {e}")))?;
        Self::from_flat(inner)
    }

    /// Decode a raw flat-encoded UPLC program (no CBOR wrapper).
    ///
    /// Layout: `version_major (Natural), version_minor (Natural),
    /// version_patch (Natural), term, filler`. Returns
    /// `UplcError::FlatDecode` on any truncation / malformedness.
    pub fn from_flat(bytes: &[u8]) -> Result<Self, UplcError> {
        let mut r = BitReader::new(bytes);
        let major = r.read_natural_biguint()?;
        let minor = r.read_natural_biguint()?;
        let patch = r.read_natural_biguint()?;
        let term = decode_term(&mut r)?;
        // The flat encoding pads to a byte boundary with a 1-bit prefix
        // followed by 0-bit fillers. `read_filler` enforces that the
        // trailing bits match this shape.
        r.read_filler()?;
        // Haskell's flat `strictDecoder` unconditionally raises `TooMuchSpace`
        // (oracle-confirmed, issue #822) when the input is not fully consumed
        // to the bit after the mandatory trailing filler. A canonical writer
        // (`to_flat`, below) never leaves bits unconsumed, so any remainder
        // here means the caller handed us `valid_program ‖ trailing_bytes` —
        // e.g. an adversary appending padding to a known script to mint a
        // distinct on-chain script hash that still evaluates identically.
        // Reject rather than silently accept: cardano-node would refuse this
        // script at deserialisation, so accepting it here is a consensus
        // divergence in the "too permissive" direction.
        let remaining = r.bits_remaining();
        if remaining != 0 {
            return Err(UplcError::FlatDecode(format!(
                "TooMuchSpace: {remaining} trailing bit(s) after the program's \
                 mandatory padding"
            )));
        }
        Ok(Program {
            version: (major, minor, patch),
            term,
        })
    }

    /// Encode the program as `(major, minor, patch, flat-encoded term)`
    /// and wrap in a CBOR byte-string.
    pub fn to_cbor(&self) -> Result<Vec<u8>, UplcError> {
        use minicbor::Encoder;
        let flat = self.to_flat()?;
        let mut out = Vec::with_capacity(flat.len() + 4);
        let mut e = Encoder::new(&mut out);
        e.bytes(&flat)
            .map_err(|err| UplcError::Internal(format!("CBOR encode: {err}")))?;
        Ok(out)
    }

    /// Encode the program as raw flat bytes (no CBOR wrapper).
    pub fn to_flat(&self) -> Result<Vec<u8>, UplcError> {
        let mut w = BitWriter::new();
        w.write_natural_biguint(&self.version.0)?;
        w.write_natural_biguint(&self.version.1)?;
        w.write_natural_biguint(&self.version.2)?;
        encode_term(&mut w, &self.term)?;
        // `BitWriter::finish` already appends the single mandatory trailing
        // filler (see its doc comment) — an extra explicit `write_filler()`
        // call here previously wrote it TWICE, appending a spurious sentinel
        // byte (`0000_0001`) after the real one (issue #835). That went
        // unnoticed only because `Program::from_flat` had no
        // fully-consumed-input check (issue #822); now that #822 rejects any
        // trailing bits after the mandatory padding, the double filler must
        // go or every `to_flat`/`to_cbor`-encoded fixture in this crate's own
        // tests would round-trip-fail as "TooMuchSpace".
        Ok(w.finish())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::{Constant, Term};
    use num_bigint::BigUint;
    use std::rc::Rc;

    /// Test-only ergonomic constructor for the version triple, since
    /// `BigUint` doesn't support bare integer-literal tuple syntax.
    fn v(major: u64, minor: u64, patch: u64) -> (BigUint, BigUint, BigUint) {
        (
            BigUint::from(major),
            BigUint::from(minor),
            BigUint::from(patch),
        )
    }

    #[test]
    fn round_trips_through_flat_with_error_body() {
        let p = Program {
            version: v(1, 1, 0),
            term: Term::Error,
        };
        let flat = p.to_flat().unwrap();
        let back = Program::from_flat(&flat).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn round_trips_through_flat_with_const_integer_body() {
        let p = Program {
            version: v(1, 0, 0),
            term: Term::Const(Constant::Integer(num_bigint::BigInt::from(42))),
        };
        let flat = p.to_flat().unwrap();
        let back = Program::from_flat(&flat).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn round_trips_through_flat_with_nested_app() {
        let p = Program {
            version: v(1, 1, 0),
            term: Term::App(
                Rc::new(Term::Lam(Rc::new(Term::Var(0)))),
                Rc::new(Term::Const(Constant::Integer(num_bigint::BigInt::from(7)))),
            ),
        };
        let flat = p.to_flat().unwrap();
        let back = Program::from_flat(&flat).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn round_trips_through_cbor_wrapped_form() {
        let p = Program {
            version: v(1, 1, 0),
            term: Term::Const(Constant::Integer(num_bigint::BigInt::from(123))),
        };
        let cbor = p.to_cbor().unwrap();
        // CBOR-bytes wrapper: first byte's major type must be 2 (binary string).
        assert_eq!(cbor[0] & 0xe0, 0x40);
        let back = Program::from_cbor(&cbor).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn from_cbor_rejects_non_bytes_top_level() {
        // `0x80` = array(0) → not a CBOR byte string.
        let err = Program::from_cbor(&[0x80]).unwrap_err();
        assert!(matches!(err, UplcError::FlatDecode(_)));
    }

    #[test]
    fn from_flat_rejects_empty_input() {
        // No version bytes at all.
        let err = Program::from_flat(&[]).unwrap_err();
        assert!(matches!(err, UplcError::FlatDecode(_)));
    }

    #[test]
    fn from_cbor_rejects_truncated_inner_flat() {
        use minicbor::Encoder;
        // Wrap a single byte that cannot be a complete flat program.
        let mut buf = Vec::new();
        Encoder::new(&mut buf).bytes(&[0x00]).unwrap();
        let err = Program::from_cbor(&buf).unwrap_err();
        assert!(matches!(err, UplcError::FlatDecode(_)));
    }

    #[test]
    fn version_triple_is_preserved_through_roundtrip() {
        let p = Program {
            version: v(3, 4, 5),
            term: Term::Error,
        };
        let back = Program::from_flat(&p.to_flat().unwrap()).unwrap();
        assert_eq!(back.version, v(3, 4, 5));
    }

    /// #842: the version triple decodes via the arbitrary-precision
    /// `BigUint` path (`BitReader::read_natural_biguint`), never the
    /// strict `read_word64_strict` used for the De Bruijn index /
    /// `Constr` tag (`flat/term.rs`). Haskell types `Version`'s three
    /// fields as unbounded `Natural` (`PlutusCore.Version`), which never
    /// rejects on overflow — a value right at the `u64` boundary
    /// (`u64::MAX`) must round-trip cleanly, not be rejected as it would
    /// be under the strict Word64 rule used elsewhere.
    #[test]
    fn version_component_at_u64_boundary_is_not_rejected() {
        let p = Program {
            version: (BigUint::from(u64::MAX), BigUint::ZERO, BigUint::ZERO),
            term: Term::Error,
        };
        let back = Program::from_flat(&p.to_flat().unwrap())
            .expect("version component at the u64 boundary must not be rejected");
        assert_eq!(
            back.version,
            (BigUint::from(u64::MAX), BigUint::ZERO, BigUint::ZERO)
        );
    }

    /// #842 residual: a version component that is genuinely WIDER than a
    /// `u64` (requires an 11th flat chunk) must round-trip to its EXACT
    /// value, not silently truncate to a smaller, wrong `u64` (the prior
    /// lenient-`u64`-path behavior — see
    /// `flat::bits::tests::read_natural_u64_lenient_path_unchanged_for_version_triple`,
    /// which documents that the raw bit-level reader still keeps that
    /// legacy behavior; `Program` itself no longer goes through it).
    /// This is 100% adversarial (no real cardano-node release has ever
    /// emitted anything but `1.0.0`/`1.1.0`), but Haskell's decoder would
    /// still decode such a blob successfully with the exact wide value,
    /// so silently substituting a different (smaller) value here would
    /// be a genuine, if obscure, consensus divergence in
    /// `flat::term::validate_program_availability`'s
    /// `version >= PLC_VERSION_1_1_0` gate.
    #[test]
    fn version_component_wider_than_u64_round_trips_exactly() {
        let huge: BigUint = (BigUint::from(1u8) << 70u32) + BigUint::from(999u32);
        let p = Program {
            version: (huge.clone(), BigUint::ZERO, BigUint::ZERO),
            term: Term::Error,
        };
        let back = Program::from_flat(&p.to_flat().unwrap())
            .expect("a >64-bit version component must not be rejected");
        assert_eq!(back.version, (huge, BigUint::ZERO, BigUint::ZERO));
    }

    /// Canonical IOG always-true V1 validator (vendored by every cardano-node
    /// integration test fixture). cborHex `4e4d01000033222220051200120011`
    /// decomposes as: outer CBOR byte string of 14 bytes → inner CBOR byte
    /// string of 13 bytes → flat-encoded UPLC program of 13 bytes.
    /// Regression: dugite v1.7.0 inverted the filler convention and rejected
    /// this with `filler must start with a 1 bit`.
    #[test]
    fn decodes_canonical_v1_always_true() {
        let flat = [
            0x01, 0x00, 0x00, 0x33, 0x22, 0x22, 0x20, 0x05, 0x12, 0x00, 0x12, 0x00, 0x11,
        ];
        let p = Program::from_flat(&flat).expect("canonical V1 always-true must decode");
        assert_eq!(p.version, v(1, 0, 0));
    }

    /// Canonical IOG always-true V2 validator. cborHex `49480100002221200101`
    /// → outer 9-byte → inner 8-byte flat.
    #[test]
    fn decodes_canonical_v2_always_true() {
        let flat = [0x01, 0x00, 0x00, 0x22, 0x21, 0x20, 0x01, 0x01];
        let p = Program::from_flat(&flat).expect("canonical V2 always-true must decode");
        assert_eq!(p.version, v(1, 0, 0));
    }

    /// Issue #822: a valid flat program with only the mandatory final-byte
    /// padding must still decode. (This is the "don't over-tighten" half of
    /// the trailing-bytes gate — a regression here would false-reject every
    /// legitimate on-chain script.)
    #[test]
    fn from_flat_accepts_program_with_only_mandatory_padding() {
        let p = Program {
            version: v(1, 1, 0),
            term: Term::Error,
        };
        let flat = p.to_flat().unwrap();
        let back = Program::from_flat(&flat).expect("canonical padding must decode");
        assert_eq!(back, p);

        // Canonical IOG fixtures above are the same shape: zero bytes beyond
        // the mandatory filler.
        let flat_v1 = [
            0x01, 0x00, 0x00, 0x33, 0x22, 0x22, 0x20, 0x05, 0x12, 0x00, 0x12, 0x00, 0x11,
        ];
        Program::from_flat(&flat_v1).expect("canonical V1 fixture must still decode");
    }

    /// Issue #822: `Program::from_flat` must reject `valid_program ‖
    /// trailing_bytes` the way Haskell's flat decoder raises `TooMuchSpace`.
    /// An adversary can append arbitrary bytes to a known-good script; if
    /// dugite silently ignored them the resulting bytes would hash to a
    /// DIFFERENT on-chain script than the one Haskell would compute for the
    /// same (rejected) bytes — a genuine consensus divergence, not just a
    /// decode nicety.
    #[test]
    fn from_flat_rejects_trailing_byte_after_valid_program() {
        let p = Program {
            version: v(1, 1, 0),
            term: Term::Error,
        };
        let mut flat = p.to_flat().unwrap();
        flat.push(0x00);
        let err = Program::from_flat(&flat).unwrap_err();
        assert!(matches!(err, UplcError::FlatDecode(_)), "got {err:?}");

        // A non-zero trailing byte (i.e. one that itself looks like it could
        // start a new filler) must be rejected too — the check is "any bits
        // remain after the program's own filler", not "the trailing bytes
        // happen to be zero".
        let mut flat_nonzero = p.to_flat().unwrap();
        flat_nonzero.push(0xff);
        let err = Program::from_flat(&flat_nonzero).unwrap_err();
        assert!(matches!(err, UplcError::FlatDecode(_)), "got {err:?}");

        // Regression guard on the canonical V1 always-true fixture: appending
        // even a single trailing byte must flip it from accept to reject.
        let mut flat_v1 = vec![
            0x01, 0x00, 0x00, 0x33, 0x22, 0x22, 0x20, 0x05, 0x12, 0x00, 0x12, 0x00, 0x11,
        ];
        flat_v1.push(0x00);
        let err = Program::from_flat(&flat_v1).unwrap_err();
        assert!(matches!(err, UplcError::FlatDecode(_)), "got {err:?}");
    }
}
