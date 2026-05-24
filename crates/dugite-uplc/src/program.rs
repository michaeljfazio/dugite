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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    pub version: (u64, u64, u64),
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
        let major = r.read_natural_u64()?;
        let minor = r.read_natural_u64()?;
        let patch = r.read_natural_u64()?;
        let term = decode_term(&mut r)?;
        // The flat encoding pads to a byte boundary with a 1-bit prefix
        // followed by 0-bit fillers. `read_filler` enforces that the
        // trailing bits match this shape.
        r.read_filler()?;
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
        w.write_natural_u64(self.version.0)?;
        w.write_natural_u64(self.version.1)?;
        w.write_natural_u64(self.version.2)?;
        encode_term(&mut w, &self.term)?;
        w.write_filler();
        Ok(w.finish())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::{Constant, Term};

    #[test]
    fn round_trips_through_flat_with_error_body() {
        let p = Program {
            version: (1, 1, 0),
            term: Term::Error,
        };
        let flat = p.to_flat().unwrap();
        let back = Program::from_flat(&flat).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn round_trips_through_flat_with_const_integer_body() {
        let p = Program {
            version: (1, 0, 0),
            term: Term::Const(Constant::Integer(num_bigint::BigInt::from(42))),
        };
        let flat = p.to_flat().unwrap();
        let back = Program::from_flat(&flat).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn round_trips_through_flat_with_nested_app() {
        let p = Program {
            version: (1, 1, 0),
            term: Term::App(
                Box::new(Term::Lam(Box::new(Term::Var(0)))),
                Box::new(Term::Const(Constant::Integer(num_bigint::BigInt::from(7)))),
            ),
        };
        let flat = p.to_flat().unwrap();
        let back = Program::from_flat(&flat).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn round_trips_through_cbor_wrapped_form() {
        let p = Program {
            version: (1, 1, 0),
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
            version: (3, 4, 5),
            term: Term::Error,
        };
        let back = Program::from_flat(&p.to_flat().unwrap()).unwrap();
        assert_eq!(back.version, (3, 4, 5));
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
        assert_eq!(p.version, (1, 0, 0));
    }

    /// Canonical IOG always-true V2 validator. cborHex `49480100002221200101`
    /// → outer 9-byte → inner 8-byte flat.
    #[test]
    fn decodes_canonical_v2_always_true() {
        let flat = [0x01, 0x00, 0x00, 0x22, 0x21, 0x20, 0x01, 0x01];
        let p = Program::from_flat(&flat).expect("canonical V2 always-true must decode");
        assert_eq!(p.version, (1, 0, 0));
    }
}
