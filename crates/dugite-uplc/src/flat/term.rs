//! Term-level flat codec (UPLC-2 part 2).
//!
//! Encodes and decodes a [`Term`] / [`Constant`] / [`Program`] to the
//! flat wire format. The encoding rules are the *normative* Haskell
//! `IntersectMBO/plutus:plutus-core/untyped-plutus-core/.../Flat.hs`
//! `encodeTerm` / `decodeTerm` / `encode (Some (ValueOf uni))`.
//!
//! ## Term tags (4 bits, MSB-first)
//!
//! | Tag | Constructor | Wire form (`ann` always omitted: `()` = 0 bits)          |
//! |-----|-------------|-----------------------------------------------------------|
//! | 0   | Var         | `tag` + varint(`db_index`)                                |
//! | 1   | Delay       | `tag` + term                                              |
//! | 2   | Lam         | `tag` + 0-bit binder + term                               |
//! | 3   | App         | `tag` + term + term                                       |
//! | 4   | Constant    | `tag` + universe-tag list + value bytes                   |
//! | 5   | Force       | `tag` + term                                              |
//! | 6   | Error       | `tag`                                                     |
//! | 7   | Builtin     | `tag` + 7-bit builtin id                                  |
//! | 8   | Constr      | `tag` + varint(`constr_tag`) + cons-list(term) (v1.1.0+)  |
//! | 9   | Case        | `tag` + term + cons-list(term) (v1.1.0+)                  |
//!
//! ## Constant universe-tag list (4-bit cons-prefixed)
//!
//! The flat constant encoding is a cons-bit-prefixed list of 4-bit
//! universe tags followed by the encoded value(s). Single-atom tags
//! we currently support:
//!
//! | Tag | Universe                | Value encoding                          |
//! |-----|-------------------------|-----------------------------------------|
//! | 0   | Integer                 | zig-zag varint (arbitrary precision)    |
//! | 1   | ByteString              | filler-aligned chunked byte string      |
//! | 2   | String                  | byte string of UTF-8 bytes              |
//! | 3   | Unit                    | (no bits)                               |
//! | 4   | Bool                    | 1 bit                                   |
//!
//! Compound tags (5=List, 6=Pair, 7=Apply, 8=Data) and the BLS atomic
//! tags (9/10/11) are deferred to a follow-on commit alongside the
//! universe-tag recursion machinery.
//!
//! ## Defensive properties
//!
//! - **Depth-bounded recursion** via an explicit [`FLAT_MAX_DEPTH`]
//!   counter on every term recursion.
//! - **No `unwrap` / `panic!`** on adversarial bytes — every error
//!   surfaces as [`UplcError::FlatDecode`].
//! - **Unknown term tags** (10..=15) and unknown universe tags
//!   (5..=15 for now) return typed errors.
//! - **Pre-allocations clamped** via `safe_alloc_capacity` against the
//!   remaining bit budget of the input.

use super::bits::{BitReader, BitWriter};
use super::{FlatResult, FLAT_MAX_DEPTH};
use crate::term::{BuiltinId, Constant, Term, TypeTag};
use crate::UplcError;

use num_bigint::{BigInt, Sign};

/// Width of the term-constructor tag, in bits. Matches the Haskell
/// `termTagWidth = 4`.
const TERM_TAG_WIDTH: u8 = 4;

/// Width of the builtin discriminant, in bits. Matches the Haskell
/// `builtinTagWidth = 7`.
const BUILTIN_TAG_WIDTH: u8 = 7;

/// Width of an atomic universe-tag, in bits.
const CONST_TAG_WIDTH: u8 = 4;

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Decode a single [`Term`] from a flat bit stream.
pub fn decode_term(r: &mut BitReader<'_>) -> FlatResult<Term> {
    decode_term_depth(r, 0)
}

/// Encode a [`Term`] into a [`BitWriter`].
pub fn encode_term(w: &mut BitWriter, t: &Term) -> FlatResult<()> {
    encode_term_depth(w, t, 0)
}

/// Decode a [`Constant`] from a flat bit stream.
pub fn decode_constant(r: &mut BitReader<'_>) -> FlatResult<Constant> {
    let type_tag = decode_type_tag(r)?;
    decode_constant_value(r, &type_tag)
}

/// Encode a [`Constant`] into a [`BitWriter`].
pub fn encode_constant(w: &mut BitWriter, c: &Constant) -> FlatResult<()> {
    let tag = constant_type_tag(c)?;
    encode_type_tag(w, &tag)?;
    encode_constant_value(w, c)
}

// ---------------------------------------------------------------------------
// Term codec
// ---------------------------------------------------------------------------

fn decode_term_depth(r: &mut BitReader<'_>, depth: usize) -> FlatResult<Term> {
    if depth > FLAT_MAX_DEPTH {
        return Err(UplcError::FlatDecode(format!(
            "term depth limit exceeded ({FLAT_MAX_DEPTH})"
        )));
    }
    let tag = r.read_bits8(TERM_TAG_WIDTH)?;
    match tag {
        0 => {
            let idx = r.read_natural_u64()?;
            Ok(Term::Var(idx))
        }
        1 => {
            let body = decode_term_depth(r, depth + 1)?;
            Ok(Term::Delay(Box::new(body)))
        }
        2 => {
            // Binder is encoded as zero bits for De Bruijn UPLC — see
            // `instance Flat (Binder DeBruijn)` in Haskell. Read
            // nothing and recurse straight into the body.
            let body = decode_term_depth(r, depth + 1)?;
            Ok(Term::Lam(Box::new(body)))
        }
        3 => {
            let fun = decode_term_depth(r, depth + 1)?;
            let arg = decode_term_depth(r, depth + 1)?;
            Ok(Term::App(Box::new(fun), Box::new(arg)))
        }
        4 => {
            let c = decode_constant(r)?;
            Ok(Term::Const(c))
        }
        5 => {
            let body = decode_term_depth(r, depth + 1)?;
            Ok(Term::Force(Box::new(body)))
        }
        6 => Ok(Term::Error),
        7 => {
            let raw = r.read_bits8(BUILTIN_TAG_WIDTH)?;
            // BuiltinId::from_u8 is the placeholder stub for now —
            // it returns `Internal`, which propagates as a typed
            // error to the caller. When UPLC-4 wires the table the
            // happy path here lights up automatically.
            let id = BuiltinId::from_u8(raw)?;
            Ok(Term::Builtin(id))
        }
        8 | 9 => Err(UplcError::FlatDecode(format!(
            "term tag {tag} (Constr/Case) is reserved for UPLC \
             program version 1.1.0+ and not yet wired"
        ))),
        _ => Err(UplcError::FlatDecode(format!(
            "unknown term tag {tag:#06b}"
        ))),
    }
}

fn encode_term_depth(w: &mut BitWriter, t: &Term, depth: usize) -> FlatResult<()> {
    if depth > FLAT_MAX_DEPTH {
        return Err(UplcError::Encode(format!(
            "term depth limit exceeded ({FLAT_MAX_DEPTH})"
        )));
    }
    match t {
        Term::Var(i) => {
            w.write_bits8(0, TERM_TAG_WIDTH)?;
            w.write_natural_u64(*i)?;
        }
        Term::Delay(body) => {
            w.write_bits8(1, TERM_TAG_WIDTH)?;
            encode_term_depth(w, body, depth + 1)?;
        }
        Term::Lam(body) => {
            w.write_bits8(2, TERM_TAG_WIDTH)?;
            // Binder is zero bits for De Bruijn UPLC.
            encode_term_depth(w, body, depth + 1)?;
        }
        Term::App(fun, arg) => {
            w.write_bits8(3, TERM_TAG_WIDTH)?;
            encode_term_depth(w, fun, depth + 1)?;
            encode_term_depth(w, arg, depth + 1)?;
        }
        Term::Const(c) => {
            w.write_bits8(4, TERM_TAG_WIDTH)?;
            encode_constant(w, c)?;
        }
        Term::Force(body) => {
            w.write_bits8(5, TERM_TAG_WIDTH)?;
            encode_term_depth(w, body, depth + 1)?;
        }
        Term::Error => {
            w.write_bits8(6, TERM_TAG_WIDTH)?;
        }
        Term::Builtin(id) => {
            w.write_bits8(7, TERM_TAG_WIDTH)?;
            let raw = id.as_u8();
            if raw >= 128 {
                // 7-bit field overflow guard — should be unreachable
                // given the enum's largest discriminant is 87, but we
                // surface as a typed error rather than truncate.
                return Err(UplcError::Encode(format!(
                    "BuiltinId discriminant {raw} exceeds 7-bit wire field"
                )));
            }
            w.write_bits8(raw, BUILTIN_TAG_WIDTH)?;
        }
        Term::Constr { .. } | Term::Case { .. } => {
            return Err(UplcError::Encode(
                "Term::Constr / Term::Case encoding is reserved for \
                 UPLC program version 1.1.0+ and not yet wired"
                    .into(),
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Constant codec — universe-tag list (single-atom subset only)
// ---------------------------------------------------------------------------

/// Universe-tag list, decoded into a [`TypeTag`]. The Haskell
/// encoding is a cons-bit-prefixed list of 4-bit atomic tags; we
/// currently only accept single-atom shapes (Integer, ByteString,
/// String, Unit, Bool). Compound shapes (List/Pair/Apply/Data) and
/// the BLS atomic shapes are rejected with a typed error and will
/// be wired in a follow-on commit.
fn decode_type_tag(r: &mut BitReader<'_>) -> FlatResult<TypeTag> {
    // First cons bit must be 1 — empty type lists are invalid.
    let cons = r.read_bit()?;
    if !cons {
        return Err(UplcError::FlatDecode("empty universe-tag list".into()));
    }
    let atom = r.read_bits8(CONST_TAG_WIDTH)?;
    let tag = match atom {
        0 => TypeTag::Integer,
        1 => TypeTag::ByteString,
        2 => TypeTag::String,
        3 => TypeTag::Unit,
        4 => TypeTag::Bool,
        5..=8 => {
            return Err(UplcError::FlatDecode(format!(
                "compound universe tag {atom} (List/Pair/Apply/Data) \
                 not yet wired"
            )));
        }
        9..=11 => {
            return Err(UplcError::FlatDecode(format!(
                "BLS12-381 atomic tag {atom} not yet wired (and BLS \
                 constants cannot appear in flat-encoded scripts per \
                 Haskell reference)"
            )));
        }
        _ => {
            return Err(UplcError::FlatDecode(format!(
                "unknown universe tag {atom:#06b}"
            )));
        }
    };
    // Terminator cons bit must be 0 (single-atom shape).
    let term = r.read_bit()?;
    if term {
        return Err(UplcError::FlatDecode(
            "compound universe-tag lists (extra cons bit set) are \
             not yet wired"
                .into(),
        ));
    }
    Ok(tag)
}

fn encode_type_tag(w: &mut BitWriter, t: &TypeTag) -> FlatResult<()> {
    let atom: u8 = match t {
        TypeTag::Integer => 0,
        TypeTag::ByteString => 1,
        TypeTag::String => 2,
        TypeTag::Unit => 3,
        TypeTag::Bool => 4,
        TypeTag::Data => {
            return Err(UplcError::Encode(
                "TypeTag::Data flat encoding not yet wired".into(),
            ));
        }
        TypeTag::List(_) | TypeTag::Pair(_, _) => {
            return Err(UplcError::Encode(
                "compound universe-tag encoding (List/Pair) not yet \
                 wired"
                    .into(),
            ));
        }
        TypeTag::Bls12_381G1Element | TypeTag::Bls12_381G2Element | TypeTag::Bls12_381MlResult => {
            return Err(UplcError::Encode(
                "BLS12-381 atomic tags not allowed in flat constant \
                 encoding (per Haskell reference)"
                    .into(),
            ));
        }
        TypeTag::Array(_) | TypeTag::Value => {
            return Err(UplcError::Encode(
                "Array / Value flat encoding not yet wired".into(),
            ));
        }
    };
    w.write_bit(true); // cons-bit: one atom follows
    w.write_bits8(atom, CONST_TAG_WIDTH)?;
    w.write_bit(false); // terminator
    Ok(())
}

fn constant_type_tag(c: &Constant) -> FlatResult<TypeTag> {
    match c {
        Constant::Integer(_) => Ok(TypeTag::Integer),
        Constant::ByteString(_) => Ok(TypeTag::ByteString),
        Constant::String(_) => Ok(TypeTag::String),
        Constant::Unit => Ok(TypeTag::Unit),
        Constant::Bool(_) => Ok(TypeTag::Bool),
        Constant::ProtoList { .. } | Constant::ProtoPair { .. } => Err(UplcError::Encode(
            "ProtoList / ProtoPair flat encoding not yet wired".into(),
        )),
        Constant::Data(_) => Err(UplcError::Encode("Data flat encoding not yet wired".into())),
        Constant::Bls12_381G1Element(_)
        | Constant::Bls12_381G2Element(_)
        | Constant::Bls12_381MlResult(_) => Err(UplcError::Encode(
            "BLS12-381 constants cannot appear in flat-encoded \
             scripts per Haskell reference"
                .into(),
        )),
        Constant::Array { .. } | Constant::Value(_) => Err(UplcError::Encode(
            "Array / Value constants cannot appear in flat-encoded \
             scripts (flat codec not yet wired for PV1.1.0 types)"
                .into(),
        )),
    }
}

fn decode_constant_value(r: &mut BitReader<'_>, tag: &TypeTag) -> FlatResult<Constant> {
    match tag {
        TypeTag::Integer => {
            // Haskell `Flat Integer` is zig-zag of a Natural with
            // arbitrary-width 7-bit chunks. Our `read_integer_i64` is
            // the i64-bounded subset; arbitrary-precision support
            // arrives with the bignum codec in a follow-on commit.
            let v = r.read_integer_i64()?;
            Ok(Constant::Integer(BigInt::from(v)))
        }
        TypeTag::ByteString => {
            let bs = r.read_bytestring()?;
            Ok(Constant::ByteString(bs))
        }
        TypeTag::String => {
            let bs = r.read_bytestring()?;
            let s = String::from_utf8(bs).map_err(|e| {
                UplcError::FlatDecode(format!("String constant not valid UTF-8: {e}"))
            })?;
            Ok(Constant::String(s))
        }
        TypeTag::Unit => Ok(Constant::Unit),
        TypeTag::Bool => {
            let b = r.read_bit()?;
            Ok(Constant::Bool(b))
        }
        TypeTag::Data
        | TypeTag::List(_)
        | TypeTag::Pair(_, _)
        | TypeTag::Bls12_381G1Element
        | TypeTag::Bls12_381G2Element
        | TypeTag::Bls12_381MlResult
        | TypeTag::Array(_)
        | TypeTag::Value => Err(UplcError::FlatDecode(format!(
            "constant payload for type {tag:?} not yet wired"
        ))),
    }
}

fn encode_constant_value(w: &mut BitWriter, c: &Constant) -> FlatResult<()> {
    match c {
        Constant::Integer(n) => {
            // i64-bounded subset for now. Arbitrary-precision via the
            // `Flat Integer` rule arrives in the follow-on commit.
            let v: i64 = bigint_to_i64(n)?;
            w.write_integer_i64(v)?;
        }
        Constant::ByteString(bs) => {
            w.write_bytestring(bs)?;
        }
        Constant::String(s) => {
            w.write_bytestring(s.as_bytes())?;
        }
        Constant::Unit => {
            // Zero bits.
        }
        Constant::Bool(b) => {
            w.write_bit(*b);
        }
        Constant::ProtoList { .. } | Constant::ProtoPair { .. } | Constant::Data(_) => {
            return Err(UplcError::Encode(
                "ProtoList / ProtoPair / Data flat encoding not yet \
                 wired"
                    .into(),
            ));
        }
        Constant::Bls12_381G1Element(_)
        | Constant::Bls12_381G2Element(_)
        | Constant::Bls12_381MlResult(_) => {
            return Err(UplcError::Encode(
                "BLS12-381 constants cannot appear in flat-encoded \
                 scripts per Haskell reference"
                    .into(),
            ));
        }
        Constant::Array { .. } | Constant::Value(_) => {
            return Err(UplcError::Encode(
                "Array / Value constants flat encoding not yet wired".into(),
            ));
        }
    }
    Ok(())
}

/// Convert a [`BigInt`] to `i64`, rejecting out-of-range values with
/// a typed error. This is the i64-bounded subset of the Haskell
/// `Flat Integer` rule; arbitrary-precision support lives in the
/// follow-on commit.
fn bigint_to_i64(n: &BigInt) -> FlatResult<i64> {
    if n.sign() == Sign::NoSign {
        return Ok(0);
    }
    let digits: Vec<u64> = n.iter_u64_digits().collect();
    if digits.len() > 1 {
        return Err(UplcError::Encode(format!(
            "Integer outside i64 range for current flat encoder \
             (arbitrary-precision support pending): magnitude has \
             {} u64-digits",
            digits.len()
        )));
    }
    let mag = digits.first().copied().unwrap_or(0);
    match n.sign() {
        Sign::Plus => i64::try_from(mag)
            .map_err(|_| UplcError::Encode(format!("positive Integer {mag} exceeds i64::MAX"))),
        Sign::Minus => {
            // Negate carefully: -i64::MIN is i64::MAX + 1, which can't
            // fit. Build via signed conversion of the magnitude.
            if mag <= i64::MAX as u64 {
                Ok(-(mag as i64))
            } else if mag == (i64::MAX as u64) + 1 {
                Ok(i64::MIN)
            } else {
                Err(UplcError::Encode(format!(
                    "negative Integer magnitude {mag} exceeds i64 range"
                )))
            }
        }
        Sign::NoSign => unreachable_zero(),
    }
}

/// Unreachable in `bigint_to_i64` since we short-circuit `NoSign` at
/// the top of the function, but expressed as a typed error rather
/// than `unreachable!` to honour the no-panic invariant.
fn unreachable_zero() -> FlatResult<i64> {
    Err(UplcError::Internal(
        "bigint_to_i64: Sign::NoSign reached the body".into(),
    ))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn rt_term(t: Term) -> Term {
        let mut w = BitWriter::new();
        encode_term(&mut w, &t).expect("encode");
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        decode_term(&mut r).expect("decode")
    }

    #[test]
    fn var_roundtrip() {
        for i in [0u64, 1, 100, u32::MAX as u64, u64::MAX] {
            let t = Term::Var(i);
            assert_eq!(rt_term(t.clone()), t, "var index {i}");
        }
    }

    #[test]
    fn error_roundtrip() {
        assert_eq!(rt_term(Term::Error), Term::Error);
    }

    #[test]
    fn delay_force_roundtrip() {
        let t = Term::Delay(Box::new(Term::Force(Box::new(Term::Error))));
        assert_eq!(rt_term(t.clone()), t);
    }

    #[test]
    fn lam_app_roundtrip() {
        // (lam (var 1)) — innermost binder reference (1-based).
        let t = Term::Lam(Box::new(Term::Var(1)));
        assert_eq!(rt_term(t.clone()), t);

        let app = Term::App(Box::new(t.clone()), Box::new(Term::Var(2)));
        assert_eq!(rt_term(app.clone()), app);
    }

    #[test]
    fn const_int_roundtrip() {
        for n in [0i64, 1, -1, 1_000_000, -1_000_000, i64::MAX, i64::MIN] {
            let t = Term::Const(Constant::Integer(BigInt::from(n)));
            assert_eq!(rt_term(t.clone()), t, "int {n}");
        }
    }

    #[test]
    fn const_bytes_roundtrip() {
        for bs in [
            Vec::<u8>::new(),
            vec![0xab],
            (0..255u8).collect::<Vec<_>>(),
            (0..512u32).map(|i| (i & 0xff) as u8).collect(),
        ] {
            let t = Term::Const(Constant::ByteString(bs.clone()));
            assert_eq!(rt_term(t.clone()), t, "len={}", bs.len());
        }
    }

    #[test]
    fn const_string_roundtrip() {
        for s in ["", "x", "hello", "ünıçødé 🎉"] {
            let t = Term::Const(Constant::String(s.into()));
            assert_eq!(rt_term(t.clone()), t, "s={s:?}");
        }
    }

    #[test]
    fn const_unit_roundtrip() {
        assert_eq!(
            rt_term(Term::Const(Constant::Unit)),
            Term::Const(Constant::Unit)
        );
    }

    #[test]
    fn const_bool_roundtrip() {
        for b in [true, false] {
            let t = Term::Const(Constant::Bool(b));
            assert_eq!(rt_term(t.clone()), t, "b={b}");
        }
    }

    #[test]
    fn nested_structure_roundtrip() {
        // (app (lam (var 1)) (const #t))
        let t = Term::App(
            Box::new(Term::Lam(Box::new(Term::Var(1)))),
            Box::new(Term::Const(Constant::Bool(true))),
        );
        assert_eq!(rt_term(t.clone()), t);
    }

    #[test]
    fn deeply_nested_terms_succeed_under_limit() {
        // Build a moderately deep `Force(Force(...))` chain. The
        // limit isn't testing FLAT_MAX_DEPTH itself (4096) — that
        // would risk a stack overflow from the recursive `Drop` of
        // the resulting `Box<Term>` tree on test thread stacks. We
        // only need to confirm "depth N works" for an N well above
        // any real-world script. The depth-limit *rejection* path
        // is exercised below in `rejects_overdeep_terms`.
        let depth = 128;
        let mut t = Term::Error;
        for _ in 0..depth {
            t = Term::Force(Box::new(t));
        }
        assert_eq!(rt_term(t.clone()), t);
    }

    #[test]
    fn rejects_overdeep_terms_at_decode() {
        // Build a chain of `Force` tags past FLAT_MAX_DEPTH at the
        // bit level. Each `Force` is the 4-bit tag `0101` = 0x5,
        // and the chain terminates with `Error` (0110 = 0x6).
        //
        // For (FLAT_MAX_DEPTH + 4) levels, the encoder will fail
        // first (typed `Encode` error), so we hand-craft bits.
        let levels = FLAT_MAX_DEPTH + 4;
        let mut w = BitWriter::new();
        for _ in 0..levels {
            // Force tag (5 = 0b0101)
            w.write_bits8(5, TERM_TAG_WIDTH).unwrap();
        }
        // Error tag (6 = 0b0110)
        w.write_bits8(6, TERM_TAG_WIDTH).unwrap();
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        let result = decode_term(&mut r);
        assert!(
            matches!(result, Err(UplcError::FlatDecode(_))),
            "expected depth-limit error, got {result:?}"
        );
    }

    #[test]
    fn rejects_unknown_term_tag() {
        // 4-bit tag 10 (0b1010) is reserved-but-unwired (Constr).
        // 12+ is unknown.
        // Build the bytes manually: just 4 bits of 1100 (= 12)
        // followed by filler. 0b1100_0000: tag=12, then 4 padding bits.
        let bytes = vec![0xc0u8];
        let mut r = BitReader::new(&bytes);
        assert!(decode_term(&mut r).is_err());
    }

    #[test]
    fn reads_var_past_end_errors() {
        // tag=0 (Var) — 4 bits, then a Natural varint that's never
        // terminated.
        let bytes = vec![0x00];
        let mut r = BitReader::new(&bytes);
        let r_out = decode_term(&mut r);
        assert!(r_out.is_err(), "got {r_out:?}");
    }

    #[test]
    fn builtin_roundtrip_full_table() {
        for raw in 0u8..=87 {
            let id = BuiltinId::from_u8(raw).expect("from_u8");
            let t = Term::Builtin(id);
            let mut w = BitWriter::new();
            encode_term(&mut w, &t).expect("encode");
            let bytes = w.finish();
            let mut r = BitReader::new(&bytes);
            let decoded = decode_term(&mut r).expect("decode");
            assert_eq!(
                decoded,
                t,
                "builtin round-trip failed for {} (raw={raw})",
                id.name()
            );
        }
    }

    #[test]
    fn rejects_empty_universe_tag_list() {
        // tag=4 (Const), followed by a cons bit of 0 (empty list).
        // Layout: 0b0100_0xxx → 0x40 covers exactly tag=4 in the top
        // 4 bits + a single 0 cons bit at position 4.
        let bytes = vec![0x40, 0x00];
        let mut r = BitReader::new(&bytes);
        assert!(decode_term(&mut r).is_err());
    }
}
