//! UPLC term and constant AST.
//!
//! This module defines the in-memory representation of Untyped Plutus Core
//! programs after they have been decoded from flat-encoded bytes. The
//! design follows the Haskell reference (`Plutus.Core.Term`) but is
//! adapted to idiomatic Rust:
//!
//!  - Binders use **De Bruijn indices** end-to-end (no `Name`-based
//!    variant). The flat decoder produces `DeBruijn` directly and the
//!    CEK machine consumes it directly; we never carry symbolic names
//!    through the evaluator. This avoids the `NamedDeBruijn` /
//!    `FakeNamedDeBruijn` shuffling that aiken-uplc has to do.
//!
//!  - Term recursion uses `Box<Term>` rather than `Rc`/`Arc`/arena.
//!    The CEK machine evaluates by stepping through an explicit context
//!    stack (heap-allocated) so the term tree is never recursively
//!    walked. This keeps the AST sharing-free and trivially
//!    `Send + Sync + Clone` without arena lifetimes leaking into
//!    consumer APIs.
//!
//!  - The `Constant` enum carries discriminants in the order the Haskell
//!    reference's `DefaultUni` enum uses, so flat-tag decoding is a
//!    direct table lookup.
//!
//! Bit-for-bit compatibility with the Haskell reference is required for
//! every variant — round-trip property tests are in
//! `tests/term_roundtrip.rs` (to be added with the flat decoder).

use crate::data::Data;

/// A UPLC term — a single AST node.
///
/// The `Box` wrapping makes every recursive variant heap-allocated, so
/// the enum size stays a fixed 8 bytes for the discriminant + pointers.
/// Stack overflow on deeply-nested terms is avoided by never recursing
/// over the term tree directly: the CEK machine carries an explicit
/// continuation stack instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Term {
    /// `Var i` — a variable referring to the binder at De Bruijn index
    /// `i` (with `1` being the innermost binder, matching the Haskell
    /// convention).
    Var(u64),

    /// `Lam body` — a lambda abstraction. The body is open under one
    /// additional binder.
    Lam(Box<Term>),

    /// `App fun arg` — function application.
    App(Box<Term>, Box<Term>),

    /// `Constant c` — a primitive value lifted into a term.
    Const(Constant),

    /// `Delay t` — wraps `t` into a thunk; reduced only by `Force`.
    Delay(Box<Term>),

    /// `Force t` — forces a `Delay`-wrapped thunk.
    Force(Box<Term>),

    /// `Error` — script failure (the CEK machine raises
    /// [`UplcError::ScriptError`](crate::UplcError::ScriptError)).
    Error,

    /// `Builtin id` — a reference to one of the Plutus-Core builtin
    /// functions, identified by its [`BuiltinId`].
    Builtin(BuiltinId),

    /// `Constr tag args` — Plutus-Core SOP constructor (introduced for
    /// PlutusV3; CIP-0085).
    Constr { tag: u64, args: Vec<Term> },

    /// `Case scrutinee branches` — Plutus-Core SOP case expression
    /// (introduced for PlutusV3; CIP-0085).
    Case {
        scrutinee: Box<Term>,
        branches: Vec<Term>,
    },
}

/// Primitive constants, in the exact discriminant order used by the
/// Haskell `DefaultUni` enum so flat-tag decoding is a direct mapping.
///
/// The flat encoding tags these via the universe-tag bit sequence
/// described in `crates/dugite-uplc/DESIGN.md` §3.2. New variants for
/// future Plutus versions are appended; we do not reorder.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(non_camel_case_types)]
pub enum Constant {
    /// Arbitrary-precision integer.
    Integer(num_bigint::BigInt),
    /// Byte string (the underlying storage is a `Vec<u8>` so we own the
    /// data; this matches Haskell's `ByteString`).
    ByteString(Vec<u8>),
    /// UTF-8 string. Decoding rejects non-UTF-8 sequences.
    String(String),
    /// The unit value `()`.
    Unit,
    /// Boolean.
    Bool(bool),
    /// `ProtoList element_type elements` — a flat-encoded list whose
    /// element type is recorded for type-checking at evaluation time.
    ///
    /// We store both the element-type sequence (`TypeTag`) and the
    /// elements together so the CEK machine can type-check builtin
    /// applications without re-parsing the wire bytes.
    ProtoList {
        elem_type: TypeTag,
        elements: Vec<Constant>,
    },
    /// `ProtoPair t1 t2 a b` — a flat-encoded pair, parameterised by
    /// the static types of its components.
    ProtoPair {
        a_type: TypeTag,
        b_type: TypeTag,
        a: Box<Constant>,
        b: Box<Constant>,
    },
    /// PlutusData — recursive sum type used by the script context.
    Data(Data),
    /// BLS12-381 G1 element. Stored compressed (48 bytes) for canonical
    /// equality; uncompressed cache is materialised in the CEK machine
    /// state when needed. Boxed to keep the `Constant` enum compact.
    Bls12_381G1Element(Box<[u8; 48]>),
    /// BLS12-381 G2 element. Stored compressed (96 bytes). Boxed.
    Bls12_381G2Element(Box<[u8; 96]>),
    /// BLS12-381 GT (Miller-loop result). The on-chain representation
    /// is canonical-compressed (576 bytes after `final_exponentiation`).
    /// Boxed — the variant would otherwise dominate the enum size.
    Bls12_381MlResult(Box<[u8; 576]>),
}

impl Constant {
    /// Static [`TypeTag`] describing this constant's value type.
    /// Used by builtin denotations that need to enforce type matching
    /// at the value level (e.g. `mkCons` must reject a head whose type
    /// disagrees with the list's element type — see #603).
    pub fn type_tag(&self) -> TypeTag {
        match self {
            Constant::Integer(_) => TypeTag::Integer,
            Constant::ByteString(_) => TypeTag::ByteString,
            Constant::String(_) => TypeTag::String,
            Constant::Unit => TypeTag::Unit,
            Constant::Bool(_) => TypeTag::Bool,
            Constant::ProtoList { elem_type, .. } => TypeTag::List(Box::new(elem_type.clone())),
            Constant::ProtoPair { a_type, b_type, .. } => {
                TypeTag::Pair(Box::new(a_type.clone()), Box::new(b_type.clone()))
            }
            Constant::Data(_) => TypeTag::Data,
            Constant::Bls12_381G1Element(_) => TypeTag::Bls12_381G1Element,
            Constant::Bls12_381G2Element(_) => TypeTag::Bls12_381G2Element,
            Constant::Bls12_381MlResult(_) => TypeTag::Bls12_381MlResult,
        }
    }
}

/// Static type-tags for `ProtoList` / `ProtoPair` element types.
///
/// Flat-encoded universe tags are a *bit sequence* (Haskell:
/// `Encoding`-of-`DefaultUni`), but at the term level we only ever
/// inspect the top-level type, so a flat enum here is sufficient.
/// Nested universes (lists of lists, pairs of pairs) are still
/// representable because the constants themselves recurse.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(non_camel_case_types)]
pub enum TypeTag {
    Integer,
    ByteString,
    String,
    Unit,
    Bool,
    Data,
    List(Box<TypeTag>),
    Pair(Box<TypeTag>, Box<TypeTag>),
    Bls12_381G1Element,
    Bls12_381G2Element,
    Bls12_381MlResult,
}

/// All Plutus Core builtin function identifiers.
///
/// The discriminants match the Haskell `DefaultFun` enum order verbatim
/// — that ordering is **normative** because the flat encoding stores
/// the builtin as a raw `u8` discriminant. Reordering would break wire
/// compatibility with cardano-node. See the formal spec
/// (plutus-core/docs/) for the authoritative list per Plutus version.
///
/// Additions follow protocol-version gating in cardano-ledger:
///
/// - PV1..  : 0–27 (Plutus V1)
/// - PV6..  : 28..52 (V2 additions; CIP-0033)
/// - PV9..  : V3 additions (CIP-0035 — keccak_256, blake2b_224, BLS12-381
///   ops, integerToByteString, byteStringToInteger, andByteString,
///   orByteString, xorByteString, complementByteString, readBit,
///   writeBits, replicateByte, shiftByteString, rotateByteString,
///   countSetBits, findFirstSetBit, ripemd_160, expModInteger, plus the
///   SOP constructors)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
#[non_exhaustive]
// Variant names mirror the Haskell `DefaultFun` spelling 1:1 (which
// uses `Bls12_381_G1_Add`, `Sha2_256`, etc. — underscore-separated by
// design). Suppressing the lint here keeps the spec-aligned spelling.
#[allow(non_camel_case_types)]
pub enum BuiltinId {
    AddInteger = 0,
    SubtractInteger = 1,
    MultiplyInteger = 2,
    DivideInteger = 3,
    QuotientInteger = 4,
    RemainderInteger = 5,
    ModInteger = 6,
    EqualsInteger = 7,
    LessThanInteger = 8,
    LessThanEqualsInteger = 9,
    AppendByteString = 10,
    ConsByteString = 11,
    SliceByteString = 12,
    LengthOfByteString = 13,
    IndexByteString = 14,
    EqualsByteString = 15,
    LessThanByteString = 16,
    LessThanEqualsByteString = 17,
    Sha2_256 = 18,
    Sha3_256 = 19,
    Blake2b_256 = 20,
    VerifyEd25519Signature = 21,
    AppendString = 22,
    EqualsString = 23,
    EncodeUtf8 = 24,
    DecodeUtf8 = 25,
    IfThenElse = 26,
    ChooseUnit = 27,
    Trace = 28,
    FstPair = 29,
    SndPair = 30,
    ChooseList = 31,
    MkCons = 32,
    HeadList = 33,
    TailList = 34,
    NullList = 35,
    ChooseData = 36,
    ConstrData = 37,
    MapData = 38,
    ListData = 39,
    IData = 40,
    BData = 41,
    UnConstrData = 42,
    UnMapData = 43,
    UnListData = 44,
    UnIData = 45,
    UnBData = 46,
    EqualsData = 47,
    MkPairData = 48,
    MkNilData = 49,
    MkNilPairData = 50,
    SerialiseData = 51,
    VerifyEcdsaSecp256k1Signature = 52,
    VerifySchnorrSecp256k1Signature = 53,
    // ── PlutusV3 additions ──────────────────────────────────────────
    Bls12_381_G1_Add = 54,
    Bls12_381_G1_Neg = 55,
    Bls12_381_G1_ScalarMul = 56,
    Bls12_381_G1_Equal = 57,
    Bls12_381_G1_HashToGroup = 58,
    Bls12_381_G1_Compress = 59,
    Bls12_381_G1_Uncompress = 60,
    Bls12_381_G2_Add = 61,
    Bls12_381_G2_Neg = 62,
    Bls12_381_G2_ScalarMul = 63,
    Bls12_381_G2_Equal = 64,
    Bls12_381_G2_HashToGroup = 65,
    Bls12_381_G2_Compress = 66,
    Bls12_381_G2_Uncompress = 67,
    Bls12_381_MillerLoop = 68,
    Bls12_381_MulMlResult = 69,
    Bls12_381_FinalVerify = 70,
    Keccak_256 = 71,
    Blake2b_224 = 72,
    // SOP / case-on-Constr (PlutusV3).
    IntegerToByteString = 73,
    ByteStringToInteger = 74,
    AndByteString = 75,
    OrByteString = 76,
    XorByteString = 77,
    ComplementByteString = 78,
    ReadBit = 79,
    WriteBits = 80,
    ReplicateByte = 81,
    ShiftByteString = 82,
    RotateByteString = 83,
    CountSetBits = 84,
    FindFirstSetBit = 85,
    Ripemd_160 = 86,
    ExpModInteger = 87,
}

impl BuiltinId {
    /// Parse a builtin id from the raw 7-bit wire discriminant.
    ///
    /// Unknown discriminants are an error rather than a panic — the
    /// flat decoder calls this on every `Term::Builtin` we see and
    /// must reject adversarial inputs cleanly. The table is
    /// authoritative against the Haskell `DefaultFun` enum order in
    /// `IntersectMBO/plutus:plutus-core/.../Default/Builtins.hs`.
    pub fn from_u8(raw: u8) -> Result<Self, crate::UplcError> {
        match raw {
            0 => Ok(BuiltinId::AddInteger),
            1 => Ok(BuiltinId::SubtractInteger),
            2 => Ok(BuiltinId::MultiplyInteger),
            3 => Ok(BuiltinId::DivideInteger),
            4 => Ok(BuiltinId::QuotientInteger),
            5 => Ok(BuiltinId::RemainderInteger),
            6 => Ok(BuiltinId::ModInteger),
            7 => Ok(BuiltinId::EqualsInteger),
            8 => Ok(BuiltinId::LessThanInteger),
            9 => Ok(BuiltinId::LessThanEqualsInteger),
            10 => Ok(BuiltinId::AppendByteString),
            11 => Ok(BuiltinId::ConsByteString),
            12 => Ok(BuiltinId::SliceByteString),
            13 => Ok(BuiltinId::LengthOfByteString),
            14 => Ok(BuiltinId::IndexByteString),
            15 => Ok(BuiltinId::EqualsByteString),
            16 => Ok(BuiltinId::LessThanByteString),
            17 => Ok(BuiltinId::LessThanEqualsByteString),
            18 => Ok(BuiltinId::Sha2_256),
            19 => Ok(BuiltinId::Sha3_256),
            20 => Ok(BuiltinId::Blake2b_256),
            21 => Ok(BuiltinId::VerifyEd25519Signature),
            22 => Ok(BuiltinId::AppendString),
            23 => Ok(BuiltinId::EqualsString),
            24 => Ok(BuiltinId::EncodeUtf8),
            25 => Ok(BuiltinId::DecodeUtf8),
            26 => Ok(BuiltinId::IfThenElse),
            27 => Ok(BuiltinId::ChooseUnit),
            28 => Ok(BuiltinId::Trace),
            29 => Ok(BuiltinId::FstPair),
            30 => Ok(BuiltinId::SndPair),
            31 => Ok(BuiltinId::ChooseList),
            32 => Ok(BuiltinId::MkCons),
            33 => Ok(BuiltinId::HeadList),
            34 => Ok(BuiltinId::TailList),
            35 => Ok(BuiltinId::NullList),
            36 => Ok(BuiltinId::ChooseData),
            37 => Ok(BuiltinId::ConstrData),
            38 => Ok(BuiltinId::MapData),
            39 => Ok(BuiltinId::ListData),
            40 => Ok(BuiltinId::IData),
            41 => Ok(BuiltinId::BData),
            42 => Ok(BuiltinId::UnConstrData),
            43 => Ok(BuiltinId::UnMapData),
            44 => Ok(BuiltinId::UnListData),
            45 => Ok(BuiltinId::UnIData),
            46 => Ok(BuiltinId::UnBData),
            47 => Ok(BuiltinId::EqualsData),
            48 => Ok(BuiltinId::MkPairData),
            49 => Ok(BuiltinId::MkNilData),
            50 => Ok(BuiltinId::MkNilPairData),
            51 => Ok(BuiltinId::SerialiseData),
            52 => Ok(BuiltinId::VerifyEcdsaSecp256k1Signature),
            53 => Ok(BuiltinId::VerifySchnorrSecp256k1Signature),
            54 => Ok(BuiltinId::Bls12_381_G1_Add),
            55 => Ok(BuiltinId::Bls12_381_G1_Neg),
            56 => Ok(BuiltinId::Bls12_381_G1_ScalarMul),
            57 => Ok(BuiltinId::Bls12_381_G1_Equal),
            58 => Ok(BuiltinId::Bls12_381_G1_HashToGroup),
            59 => Ok(BuiltinId::Bls12_381_G1_Compress),
            60 => Ok(BuiltinId::Bls12_381_G1_Uncompress),
            61 => Ok(BuiltinId::Bls12_381_G2_Add),
            62 => Ok(BuiltinId::Bls12_381_G2_Neg),
            63 => Ok(BuiltinId::Bls12_381_G2_ScalarMul),
            64 => Ok(BuiltinId::Bls12_381_G2_Equal),
            65 => Ok(BuiltinId::Bls12_381_G2_HashToGroup),
            66 => Ok(BuiltinId::Bls12_381_G2_Compress),
            67 => Ok(BuiltinId::Bls12_381_G2_Uncompress),
            68 => Ok(BuiltinId::Bls12_381_MillerLoop),
            69 => Ok(BuiltinId::Bls12_381_MulMlResult),
            70 => Ok(BuiltinId::Bls12_381_FinalVerify),
            71 => Ok(BuiltinId::Keccak_256),
            72 => Ok(BuiltinId::Blake2b_224),
            73 => Ok(BuiltinId::IntegerToByteString),
            74 => Ok(BuiltinId::ByteStringToInteger),
            75 => Ok(BuiltinId::AndByteString),
            76 => Ok(BuiltinId::OrByteString),
            77 => Ok(BuiltinId::XorByteString),
            78 => Ok(BuiltinId::ComplementByteString),
            79 => Ok(BuiltinId::ReadBit),
            80 => Ok(BuiltinId::WriteBits),
            81 => Ok(BuiltinId::ReplicateByte),
            82 => Ok(BuiltinId::ShiftByteString),
            83 => Ok(BuiltinId::RotateByteString),
            84 => Ok(BuiltinId::CountSetBits),
            85 => Ok(BuiltinId::FindFirstSetBit),
            86 => Ok(BuiltinId::Ripemd_160),
            87 => Ok(BuiltinId::ExpModInteger),
            _ => Err(crate::UplcError::FlatDecode(format!(
                "unknown builtin id {raw} (max recognised: 87)"
            ))),
        }
    }

    /// Wire-discriminant byte for flat encoding (7-bit field). Inverse
    /// of [`Self::from_u8`].
    pub fn as_u8(&self) -> u8 {
        // `#[repr(u8)]` on the enum makes the discriminant accessible
        // via a single cast — no match expression required.
        *self as u8
    }

    /// Lowercase identifier used in the Plutus textual syntax and in
    /// error messages. Names mirror Haskell `DefaultFun` show output
    /// (camelCase) since that's the canonical form quoted in
    /// cardano-node error logs and trace output.
    pub fn name(&self) -> &'static str {
        match self {
            BuiltinId::AddInteger => "addInteger",
            BuiltinId::SubtractInteger => "subtractInteger",
            BuiltinId::MultiplyInteger => "multiplyInteger",
            BuiltinId::DivideInteger => "divideInteger",
            BuiltinId::QuotientInteger => "quotientInteger",
            BuiltinId::RemainderInteger => "remainderInteger",
            BuiltinId::ModInteger => "modInteger",
            BuiltinId::EqualsInteger => "equalsInteger",
            BuiltinId::LessThanInteger => "lessThanInteger",
            BuiltinId::LessThanEqualsInteger => "lessThanEqualsInteger",
            BuiltinId::AppendByteString => "appendByteString",
            BuiltinId::ConsByteString => "consByteString",
            BuiltinId::SliceByteString => "sliceByteString",
            BuiltinId::LengthOfByteString => "lengthOfByteString",
            BuiltinId::IndexByteString => "indexByteString",
            BuiltinId::EqualsByteString => "equalsByteString",
            BuiltinId::LessThanByteString => "lessThanByteString",
            BuiltinId::LessThanEqualsByteString => "lessThanEqualsByteString",
            BuiltinId::Sha2_256 => "sha2_256",
            BuiltinId::Sha3_256 => "sha3_256",
            BuiltinId::Blake2b_256 => "blake2b_256",
            BuiltinId::VerifyEd25519Signature => "verifyEd25519Signature",
            BuiltinId::AppendString => "appendString",
            BuiltinId::EqualsString => "equalsString",
            BuiltinId::EncodeUtf8 => "encodeUtf8",
            BuiltinId::DecodeUtf8 => "decodeUtf8",
            BuiltinId::IfThenElse => "ifThenElse",
            BuiltinId::ChooseUnit => "chooseUnit",
            BuiltinId::Trace => "trace",
            BuiltinId::FstPair => "fstPair",
            BuiltinId::SndPair => "sndPair",
            BuiltinId::ChooseList => "chooseList",
            BuiltinId::MkCons => "mkCons",
            BuiltinId::HeadList => "headList",
            BuiltinId::TailList => "tailList",
            BuiltinId::NullList => "nullList",
            BuiltinId::ChooseData => "chooseData",
            BuiltinId::ConstrData => "constrData",
            BuiltinId::MapData => "mapData",
            BuiltinId::ListData => "listData",
            BuiltinId::IData => "iData",
            BuiltinId::BData => "bData",
            BuiltinId::UnConstrData => "unConstrData",
            BuiltinId::UnMapData => "unMapData",
            BuiltinId::UnListData => "unListData",
            BuiltinId::UnIData => "unIData",
            BuiltinId::UnBData => "unBData",
            BuiltinId::EqualsData => "equalsData",
            BuiltinId::MkPairData => "mkPairData",
            BuiltinId::MkNilData => "mkNilData",
            BuiltinId::MkNilPairData => "mkNilPairData",
            BuiltinId::SerialiseData => "serialiseData",
            BuiltinId::VerifyEcdsaSecp256k1Signature => "verifyEcdsaSecp256k1Signature",
            BuiltinId::VerifySchnorrSecp256k1Signature => "verifySchnorrSecp256k1Signature",
            BuiltinId::Bls12_381_G1_Add => "bls12_381_G1_add",
            BuiltinId::Bls12_381_G1_Neg => "bls12_381_G1_neg",
            BuiltinId::Bls12_381_G1_ScalarMul => "bls12_381_G1_scalarMul",
            BuiltinId::Bls12_381_G1_Equal => "bls12_381_G1_equal",
            BuiltinId::Bls12_381_G1_HashToGroup => "bls12_381_G1_hashToGroup",
            BuiltinId::Bls12_381_G1_Compress => "bls12_381_G1_compress",
            BuiltinId::Bls12_381_G1_Uncompress => "bls12_381_G1_uncompress",
            BuiltinId::Bls12_381_G2_Add => "bls12_381_G2_add",
            BuiltinId::Bls12_381_G2_Neg => "bls12_381_G2_neg",
            BuiltinId::Bls12_381_G2_ScalarMul => "bls12_381_G2_scalarMul",
            BuiltinId::Bls12_381_G2_Equal => "bls12_381_G2_equal",
            BuiltinId::Bls12_381_G2_HashToGroup => "bls12_381_G2_hashToGroup",
            BuiltinId::Bls12_381_G2_Compress => "bls12_381_G2_compress",
            BuiltinId::Bls12_381_G2_Uncompress => "bls12_381_G2_uncompress",
            BuiltinId::Bls12_381_MillerLoop => "bls12_381_millerLoop",
            BuiltinId::Bls12_381_MulMlResult => "bls12_381_mulMlResult",
            BuiltinId::Bls12_381_FinalVerify => "bls12_381_finalVerify",
            BuiltinId::Keccak_256 => "keccak_256",
            BuiltinId::Blake2b_224 => "blake2b_224",
            BuiltinId::IntegerToByteString => "integerToByteString",
            BuiltinId::ByteStringToInteger => "byteStringToInteger",
            BuiltinId::AndByteString => "andByteString",
            BuiltinId::OrByteString => "orByteString",
            BuiltinId::XorByteString => "xorByteString",
            BuiltinId::ComplementByteString => "complementByteString",
            BuiltinId::ReadBit => "readBit",
            BuiltinId::WriteBits => "writeBits",
            BuiltinId::ReplicateByte => "replicateByte",
            BuiltinId::ShiftByteString => "shiftByteString",
            BuiltinId::RotateByteString => "rotateByteString",
            BuiltinId::CountSetBits => "countSetBits",
            BuiltinId::FindFirstSetBit => "findFirstSetBit",
            BuiltinId::Ripemd_160 => "ripemd_160",
            BuiltinId::ExpModInteger => "expModInteger",
        }
    }
}

#[cfg(test)]
mod tests {
    //! Regression tests for the placeholder behaviour of `BuiltinId`.
    //! These stubs are scheduled to be replaced when UPLC-4 lands the
    //! builtin dispatch table; until then the tests guard the
    //! placeholder contract so callers see a typed error rather than
    //! a panic from any half-wired API.
    use super::*;
    use crate::UplcError;

    #[test]
    fn from_u8_round_trip_full_table() {
        // Every defined discriminant must round-trip through
        // `from_u8` → `as_u8`. Anything past 87 must error rather
        // than wrap or panic.
        for raw in 0u8..=87 {
            let id = BuiltinId::from_u8(raw).unwrap_or_else(|e| {
                panic!("from_u8({raw}) failed: {e}");
            });
            assert_eq!(id.as_u8(), raw, "as_u8 round-trip for raw={raw}");
        }
        for raw in [88u8, 100, 127] {
            let err = BuiltinId::from_u8(raw).unwrap_err();
            assert!(
                matches!(err, UplcError::FlatDecode(_)),
                "expected FlatDecode for raw={raw}; got {err:?}"
            );
        }
    }

    #[test]
    fn name_table_complete_and_distinct() {
        // Every variant gets a stable Haskell-style name and no two
        // names collide.
        let mut seen = std::collections::HashSet::new();
        for raw in 0u8..=87 {
            let id = BuiltinId::from_u8(raw).expect("from_u8");
            let name = id.name();
            assert!(!name.is_empty(), "empty name for raw={raw}");
            assert!(
                !name.starts_with('<'),
                "placeholder name for raw={raw}: {name:?}"
            );
            assert!(seen.insert(name), "duplicate name {name:?} at raw={raw}");
        }
    }

    #[test]
    fn name_spot_checks_match_haskell() {
        // Hand-picked names from Haskell's `DefaultFun` show output to
        // protect against transcription drift.
        assert_eq!(BuiltinId::AddInteger.name(), "addInteger");
        assert_eq!(BuiltinId::Sha2_256.name(), "sha2_256");
        assert_eq!(
            BuiltinId::VerifyEd25519Signature.name(),
            "verifyEd25519Signature"
        );
        assert_eq!(
            BuiltinId::Bls12_381_FinalVerify.name(),
            "bls12_381_finalVerify"
        );
        assert_eq!(BuiltinId::Keccak_256.name(), "keccak_256");
        assert_eq!(BuiltinId::Ripemd_160.name(), "ripemd_160");
        assert_eq!(BuiltinId::IntegerToByteString.name(), "integerToByteString");
    }
}
