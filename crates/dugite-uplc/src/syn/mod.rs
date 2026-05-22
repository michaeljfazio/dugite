//! Textual UPLC parser — the s-expression form documented in the
//! Plutus Core formal specification.
//!
//! ## Scope
//!
//! This parser exists to consume the official Plutus conformance test
//! corpus (`IntersectMBO/plutus:plutus-conformance/test-cases/uplc/`).
//! It is **not** on any production code path — Cardano nodes never
//! exchange textual UPLC at runtime; the on-chain wire format is the
//! flat encoding handled by [`crate::flat`].
//!
//! Built behind the `parser` feature so the default build stays slim:
//!
//! ```sh
//! cargo build -p dugite-uplc --features parser
//! ```
//!
//! ## Grammar (informal)
//!
//! ```text
//! program  := '(' "program" version term ')'
//! version  := DIGIT+ '.' DIGIT+ '.' DIGIT+
//! term     := var                                    -- bare identifier
//!           | '(' "lam"   name term ')'
//!           | '(' "delay"      term ')'
//!           | '(' "force"      term ')'
//!           | '(' "error"           ')'
//!           | '(' "builtin"    name ')'              -- name = camelCase ident
//!           | '(' "constr"     uint term* ')'
//!           | '(' "case"       term term* ')'
//!           | '(' "con"   type literal ')'
//!           | '[' term term+ ']'                     -- left-associative apply
//! type     := "integer" | "bytestring" | "string"
//!           | "unit" | "bool" | "data"
//!           | "bls12_381_G1_element" | "bls12_381_G2_element"
//!           | "bls12_381_mlresult"
//!           | '(' "list" type ')'
//!           | '(' "pair" type type ')'
//! literal  := INT | "#" HEX* | "0x" HEX*             -- bytestring | BLS element
//!           | '"' ESCAPED-CHAR* '"'                  -- string
//!           | "True" | "False"                       -- bool
//!           | '(' ')'                                -- unit
//!           | '[' literal (',' literal)* ']'         -- list (may be empty)
//!           | '(' literal ',' literal ')'            -- pair
//!           | dataExpr                               -- when type = data
//! dataExpr := '(' "I" INT ')'
//!           | '(' "B" "#" HEX* ')'
//!           | '(' "List" '[' dataExpr* ']' ')'
//!           | '(' "Map"  '[' '(' dataExpr ',' dataExpr ')' (',' ...)* ']' ')'
//!           | '(' "Constr" UINT '[' dataExpr* ']' ')'
//! name     := IDENT-START IDENT-CONT*
//! comment  := "--" ANY-CHAR* '\n'                    -- line comment
//! ```
//!
//! Names in the source are converted to De Bruijn indices during
//! parsing (the binder stack is pushed in `lam` and popped on exit).
//! The output is a fully-De-Bruijn [`crate::term::Term`] / [`crate::program::Program`].
//!
//! Errors carry an offset into the input plus a human-readable message;
//! callers can render `(line, column)` with [`ParseError::line_col`].

#![allow(clippy::result_large_err)]

use crate::data::Data;
use crate::program::Program;
use crate::term::{BuiltinId, Constant, Term, TypeTag};
use num_bigint::BigInt;
use num_traits::Num;

mod parser;

/// Parse a complete `(program M.m.p term)` source.
pub fn parse_program(src: &str) -> Result<Program, ParseError> {
    let mut p = parser::Parser::new(src);
    let prog = p.parse_program_top()?;
    p.finish()?;
    Ok(prog)
}

/// Parse just a term (no surrounding program header). Useful for tests
/// that produce raw term expected values.
pub fn parse_term(src: &str) -> Result<Term, ParseError> {
    let mut p = parser::Parser::new(src);
    let t = p.parse_term_top()?;
    p.finish()?;
    Ok(t)
}

/// Parse a typed constant — the right-hand side of `(con TYPE LIT)` —
/// returning the parsed `Constant` along with its declared `TypeTag`.
/// Exposed for tests; ordinary callers use [`parse_program`].
pub fn parse_constant(src: &str) -> Result<(TypeTag, Constant), ParseError> {
    let mut p = parser::Parser::new(src);
    let c = p.parse_typed_constant()?;
    p.finish()?;
    Ok(c)
}

/// Parse a `Data` literal expressed in the textual form used inside
/// `(con data EXPR)`.
pub fn parse_data(src: &str) -> Result<Data, ParseError> {
    let mut p = parser::Parser::new(src);
    let d = p.parse_data_expr()?;
    p.finish()?;
    Ok(d)
}

/// Convert a textual builtin name (`addInteger`, `sha2_256`, ...) into
/// the corresponding [`BuiltinId`]. Returns `None` for unknown names.
///
/// The name table is the authoritative inverse of
/// [`BuiltinId::name`] and is shared between the parser, conformance
/// harness, and any future CLI surface.
pub fn builtin_from_name(name: &str) -> Option<BuiltinId> {
    BUILTIN_NAME_TABLE
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, id)| *id)
}

/// Parse the textual representation of a `BigInt` literal — used by
/// the conformance harness for `(con integer N)`.
pub(crate) fn parse_signed_bigint(src: &str) -> Result<BigInt, ParseError> {
    // BigInt::from_str_radix doesn't accept leading '+' or whitespace;
    // handle the leading sign manually.
    let s = src.trim();
    if s.is_empty() {
        return Err(ParseError::at(0, "empty integer literal".into()));
    }
    let (neg, body) = if let Some(rest) = s.strip_prefix('-') {
        (true, rest)
    } else if let Some(rest) = s.strip_prefix('+') {
        (false, rest)
    } else {
        (false, s)
    };
    if body.is_empty() || !body.chars().all(|c| c.is_ascii_digit()) {
        return Err(ParseError::at(
            0,
            format!("invalid integer literal {src:?}"),
        ));
    }
    let mag = BigInt::from_str_radix(body, 10)
        .map_err(|e| ParseError::at(0, format!("integer parse failed: {e}")))?;
    Ok(if neg { -mag } else { mag })
}

/// A parse error with an absolute byte offset into the source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// 0-based byte offset into the input where the error was detected.
    pub offset: usize,
    /// Human-readable description.
    pub message: String,
}

impl ParseError {
    pub(crate) fn at(offset: usize, message: String) -> Self {
        Self { offset, message }
    }

    /// Compute the 1-based `(line, column)` of [`Self::offset`] in `src`.
    /// Returns `(1, 1)` for an offset past the end of the input.
    pub fn line_col(&self, src: &str) -> (usize, usize) {
        let mut line = 1usize;
        let mut col = 1usize;
        for (i, ch) in src.char_indices() {
            if i >= self.offset {
                break;
            }
            if ch == '\n' {
                line += 1;
                col = 1;
            } else {
                col += 1;
            }
        }
        (line, col)
    }
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (at byte offset {})", self.message, self.offset)
    }
}

impl std::error::Error for ParseError {}

/// Authoritative `(name, id)` pairs for every builtin recognised by
/// dugite-uplc. Order doesn't matter (linear scan); kept in
/// discriminant order for readability.
const BUILTIN_NAME_TABLE: &[(&str, BuiltinId)] = &[
    ("addInteger", BuiltinId::AddInteger),
    ("subtractInteger", BuiltinId::SubtractInteger),
    ("multiplyInteger", BuiltinId::MultiplyInteger),
    ("divideInteger", BuiltinId::DivideInteger),
    ("quotientInteger", BuiltinId::QuotientInteger),
    ("remainderInteger", BuiltinId::RemainderInteger),
    ("modInteger", BuiltinId::ModInteger),
    ("equalsInteger", BuiltinId::EqualsInteger),
    ("lessThanInteger", BuiltinId::LessThanInteger),
    ("lessThanEqualsInteger", BuiltinId::LessThanEqualsInteger),
    ("appendByteString", BuiltinId::AppendByteString),
    ("consByteString", BuiltinId::ConsByteString),
    ("sliceByteString", BuiltinId::SliceByteString),
    ("lengthOfByteString", BuiltinId::LengthOfByteString),
    ("indexByteString", BuiltinId::IndexByteString),
    ("equalsByteString", BuiltinId::EqualsByteString),
    ("lessThanByteString", BuiltinId::LessThanByteString),
    (
        "lessThanEqualsByteString",
        BuiltinId::LessThanEqualsByteString,
    ),
    ("sha2_256", BuiltinId::Sha2_256),
    ("sha3_256", BuiltinId::Sha3_256),
    ("blake2b_256", BuiltinId::Blake2b_256),
    ("verifyEd25519Signature", BuiltinId::VerifyEd25519Signature),
    ("appendString", BuiltinId::AppendString),
    ("equalsString", BuiltinId::EqualsString),
    ("encodeUtf8", BuiltinId::EncodeUtf8),
    ("decodeUtf8", BuiltinId::DecodeUtf8),
    ("ifThenElse", BuiltinId::IfThenElse),
    ("chooseUnit", BuiltinId::ChooseUnit),
    ("trace", BuiltinId::Trace),
    ("fstPair", BuiltinId::FstPair),
    ("sndPair", BuiltinId::SndPair),
    ("chooseList", BuiltinId::ChooseList),
    ("mkCons", BuiltinId::MkCons),
    ("headList", BuiltinId::HeadList),
    ("tailList", BuiltinId::TailList),
    ("nullList", BuiltinId::NullList),
    ("chooseData", BuiltinId::ChooseData),
    ("constrData", BuiltinId::ConstrData),
    ("mapData", BuiltinId::MapData),
    ("listData", BuiltinId::ListData),
    ("iData", BuiltinId::IData),
    ("bData", BuiltinId::BData),
    ("unConstrData", BuiltinId::UnConstrData),
    ("unMapData", BuiltinId::UnMapData),
    ("unListData", BuiltinId::UnListData),
    ("unIData", BuiltinId::UnIData),
    ("unBData", BuiltinId::UnBData),
    ("equalsData", BuiltinId::EqualsData),
    ("mkPairData", BuiltinId::MkPairData),
    ("mkNilData", BuiltinId::MkNilData),
    ("mkNilPairData", BuiltinId::MkNilPairData),
    ("serialiseData", BuiltinId::SerialiseData),
    (
        "verifyEcdsaSecp256k1Signature",
        BuiltinId::VerifyEcdsaSecp256k1Signature,
    ),
    (
        "verifySchnorrSecp256k1Signature",
        BuiltinId::VerifySchnorrSecp256k1Signature,
    ),
    ("bls12_381_G1_add", BuiltinId::Bls12_381_G1_Add),
    ("bls12_381_G1_neg", BuiltinId::Bls12_381_G1_Neg),
    ("bls12_381_G1_scalarMul", BuiltinId::Bls12_381_G1_ScalarMul),
    ("bls12_381_G1_equal", BuiltinId::Bls12_381_G1_Equal),
    (
        "bls12_381_G1_hashToGroup",
        BuiltinId::Bls12_381_G1_HashToGroup,
    ),
    ("bls12_381_G1_compress", BuiltinId::Bls12_381_G1_Compress),
    (
        "bls12_381_G1_uncompress",
        BuiltinId::Bls12_381_G1_Uncompress,
    ),
    ("bls12_381_G2_add", BuiltinId::Bls12_381_G2_Add),
    ("bls12_381_G2_neg", BuiltinId::Bls12_381_G2_Neg),
    ("bls12_381_G2_scalarMul", BuiltinId::Bls12_381_G2_ScalarMul),
    ("bls12_381_G2_equal", BuiltinId::Bls12_381_G2_Equal),
    (
        "bls12_381_G2_hashToGroup",
        BuiltinId::Bls12_381_G2_HashToGroup,
    ),
    ("bls12_381_G2_compress", BuiltinId::Bls12_381_G2_Compress),
    (
        "bls12_381_G2_uncompress",
        BuiltinId::Bls12_381_G2_Uncompress,
    ),
    ("bls12_381_millerLoop", BuiltinId::Bls12_381_MillerLoop),
    ("bls12_381_mulMlResult", BuiltinId::Bls12_381_MulMlResult),
    ("bls12_381_finalVerify", BuiltinId::Bls12_381_FinalVerify),
    ("keccak_256", BuiltinId::Keccak_256),
    ("blake2b_224", BuiltinId::Blake2b_224),
    ("integerToByteString", BuiltinId::IntegerToByteString),
    ("byteStringToInteger", BuiltinId::ByteStringToInteger),
    ("andByteString", BuiltinId::AndByteString),
    ("orByteString", BuiltinId::OrByteString),
    ("xorByteString", BuiltinId::XorByteString),
    ("complementByteString", BuiltinId::ComplementByteString),
    ("readBit", BuiltinId::ReadBit),
    ("writeBits", BuiltinId::WriteBits),
    ("replicateByte", BuiltinId::ReplicateByte),
    ("shiftByteString", BuiltinId::ShiftByteString),
    ("rotateByteString", BuiltinId::RotateByteString),
    ("countSetBits", BuiltinId::CountSetBits),
    ("findFirstSetBit", BuiltinId::FindFirstSetBit),
    ("ripemd_160", BuiltinId::Ripemd_160),
    ("expModInteger", BuiltinId::ExpModInteger),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_table_inverse_of_builtinid_name() {
        // Every variant exposed by `BuiltinId::name()` must be present
        // in the parser table, and the two must agree.
        for (n, id) in BUILTIN_NAME_TABLE {
            assert_eq!(*n, id.name(), "table entry for {id:?} disagrees");
            assert_eq!(builtin_from_name(n), Some(*id));
        }
        assert_eq!(builtin_from_name("not_a_real_builtin"), None);
    }

    #[test]
    fn parse_signed_bigint_basic() {
        assert_eq!(parse_signed_bigint("0").unwrap(), BigInt::from(0));
        assert_eq!(parse_signed_bigint("123").unwrap(), BigInt::from(123));
        assert_eq!(parse_signed_bigint("-456").unwrap(), BigInt::from(-456));
        assert_eq!(parse_signed_bigint("+7").unwrap(), BigInt::from(7));
        assert!(parse_signed_bigint("").is_err());
        assert!(parse_signed_bigint("12a").is_err());
        assert!(parse_signed_bigint("--3").is_err());
    }
}
