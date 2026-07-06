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
//!  - Term recursion uses `Rc<Term>` for recursive sub-terms. Cloning a
//!    `Term` is therefore O(1) (refcount bumps only), avoiding the
//!    O(subtree-size) deep-copy that `Box<Term>` would require when the
//!    CEK machine looks up a lambda closure stored in the shared
//!    environment. The CEK machine evaluates by stepping through an
//!    explicit context stack (heap-allocated) so the term tree is never
//!    recursively walked. `Term` is NOT `Send + Sync` (Rc is not
//!    thread-safe), but CEK evaluation is single-threaded by design.
//!
//!  - The `Constant` enum carries discriminants in the order the Haskell
//!    reference's `DefaultUni` enum uses, so flat-tag decoding is a
//!    direct table lookup.
//!
//! Bit-for-bit compatibility with the Haskell reference is required for
//! every variant — round-trip property tests are in
//! `tests/term_roundtrip.rs` (to be added with the flat decoder).

use crate::data::Data;
use crate::redeemer_resolve::ScriptLanguage;
use std::rc::Rc;

/// A UPLC term — a single AST node.
///
/// Recursive sub-terms are wrapped in `Rc<Term>` rather than `Box<Term>`.
/// This makes `Term::clone()` an O(1) operation (a series of reference-count
/// bumps) instead of an O(size-of-subtree) deep copy. The CEK machine exploits
/// this when it looks up a lambda closure from the environment: the body `Rc`
/// has refcount >= 2 (env node + current usage), so `rc_into_term` previously
/// fell back to a full deep clone of the body tree. With `Rc` sub-terms, that
/// fallback clone copies only the discriminant + inner `Rc` pointers — O(1).
///
/// The semantic contract is unchanged: all `Term` values are immutable after
/// construction, so sharing via `Rc` is equivalent to copying (no aliasing
/// hazard). The CEK machine's ExUnit accounting is also unchanged: step counts
/// and builtin costs depend only on the abstract machine's reduction sequence,
/// not on the physical representation.
///
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
    /// additional binder. `Rc<Term>` so that cloning a lambda closure
    /// (e.g. on env lookup) is a refcount bump rather than a deep copy.
    Lam(Rc<Term>),

    /// `App fun arg` — function application. Both sub-terms are `Rc`
    /// so that pushing the argument into a continuation frame is O(1).
    App(Rc<Term>, Rc<Term>),

    /// `Constant c` — a primitive value lifted into a term.
    Const(Constant),

    /// `Delay t` — wraps `t` into a thunk; reduced only by `Force`.
    /// `Rc<Term>` so that cloning a delay thunk is a refcount bump.
    Delay(Rc<Term>),

    /// `Force t` — forces a `Delay`-wrapped thunk.
    Force(Rc<Term>),

    /// `Error` — script failure (the CEK machine raises
    /// [`UplcError::ScriptError`](crate::UplcError::ScriptError)).
    Error,

    /// `Builtin id` — a reference to one of the Plutus-Core builtin
    /// functions, identified by its [`BuiltinId`].
    Builtin(BuiltinId),

    /// `Constr tag args` — Plutus-Core SOP constructor (introduced for
    /// PlutusV3; CIP-0085). Args are `Rc<Term>` so the pending-arg Vec
    /// in the Constr frame can be filled with O(1) refcount bumps.
    Constr { tag: u64, args: Vec<Rc<Term>> },

    /// `Case scrutinee branches` — Plutus-Core SOP case expression
    /// (introduced for PlutusV3; CIP-0085). `Rc` so that picking a
    /// branch from the Cases frame is an O(1) refcount bump.
    Case {
        scrutinee: Rc<Term>,
        branches: Vec<Rc<Term>>,
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
    ///
    /// `Rc`-wrapped (#838 Fix 2): the ScriptContext is a large recursive
    /// `Data` tree bound once to the script's `ctx` parameter and then
    /// referenced repeatedly (each `(var ctx)` occurrence in the
    /// compiled term). Cloning a `Value`/`Constant` on every CEK env
    /// lookup (`machine::step::compute`'s `Term::Var` arm) previously
    /// deep-cloned the whole tree per reference — the `Rc` makes that a
    /// refcount bump instead, matching Haskell's by-reference env. Only
    /// the outer `Constant::Data` wrapper is `Rc`-shared; `Data`'s own
    /// internal recursive fields (`Vec<Data>` / `Vec<(Data, Data)>`)
    /// are unchanged, so a builtin that destructures a *shared* `Data`
    /// (e.g. `unConstrData`) still deep-clones at that point (via
    /// `Rc::try_unwrap`-or-clone, see `builtin::denotations::unwrap_data`)
    /// — no worse than before, and free when the `Rc` is uniquely held.
    Data(Rc<Data>),
    /// BLS12-381 G1 element. Stored compressed (48 bytes) for canonical
    /// equality — there is no decompressed-point cache; every builtin
    /// that consumes a G1 element re-decodes it from these bytes
    /// on demand (see `crates/dugite-uplc/src/builtin/bls.rs`, and
    /// issue #839 for the resulting redundant-work tradeoff). Boxed
    /// to keep the `Constant` enum compact.
    Bls12_381G1Element(Box<[u8; 48]>),
    /// BLS12-381 G2 element. Stored compressed (96 bytes). Boxed.
    Bls12_381G2Element(Box<[u8; 96]>),
    /// BLS12-381 GT (Miller-loop result). The on-chain representation
    /// is canonical-compressed (576 bytes after `final_exponentiation`).
    /// Boxed — the variant would otherwise dominate the enum size.
    Bls12_381MlResult(Box<[u8; 576]>),
    /// PV1.1.0 `(array T)` — an immutable indexed array.  Elements are
    /// all of the same type (recorded in `elem_type` for type-checking).
    Array {
        elem_type: TypeTag,
        elements: Vec<Constant>,
    },
    /// PV1.1.0 `value` — a Cardano multi-asset value
    /// `Map<PolicyId, Map<TokenName, Integer>>` (policy IDs and token
    /// names are byte strings, amounts are i128).
    ///
    /// Canonical form requirements (enforced at parse time, matching
    /// the Haskell reference):
    ///   - Outer and inner maps are lexicographically sorted by key.
    ///   - Entries with a zero amount are removed.
    ///   - Empty inner maps (no tokens for a policy) are removed.
    ///   - Duplicate keys: amounts are summed; if the result is zero the
    ///     entry is removed.
    ///   - Policy IDs and token names must be ≤ 32 bytes.
    Value(std::collections::BTreeMap<Vec<u8>, std::collections::BTreeMap<Vec<u8>, i128>>),
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
            Constant::Array { elem_type, .. } => TypeTag::Array(Box::new(elem_type.clone())),
            Constant::Value(_) => TypeTag::Value,
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
    /// PV1.1.0 immutable array type.
    Array(Box<TypeTag>),
    /// PV1.1.0 multi-asset value type.
    Value,
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
    Bls12_381_G1_Compress = 58,
    Bls12_381_G1_Uncompress = 59,
    Bls12_381_G1_HashToGroup = 60,
    Bls12_381_G2_Add = 61,
    Bls12_381_G2_Neg = 62,
    Bls12_381_G2_ScalarMul = 63,
    Bls12_381_G2_Equal = 64,
    Bls12_381_G2_Compress = 65,
    Bls12_381_G2_Uncompress = 66,
    Bls12_381_G2_HashToGroup = 67,
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
    // ── PV1.1.0 additions ───────────────────────────────────────────────
    // Wire IDs 88-100 per IntersectMBO/plutus DefaultFun ordering
    // (explicit hand-written Flat instance in Builtins.hs, NOT toEnum order).
    /// `dropList : (Integer, list T) -> list T` (PV1.1.0). Wire ID 88.
    DropList = 88,
    /// `lengthOfArray : (array T) -> Integer` (PV1.1.0). Wire ID 89.
    LengthOfArray = 89,
    /// `listToArray : (list T) -> (array T)` (PV1.1.0). Wire ID 90.
    ListToArray = 90,
    /// `indexArray : (array T, Integer) -> T` (PV1.1.0). Wire ID 91.
    IndexArray = 91,
    /// `bls12_381_G1_multiScalarMul : (list Integer) -> (list G1) -> G1` (PV1.1.0). Wire ID 92.
    Bls12_381_G1_MultiScalarMul = 92,
    /// `bls12_381_G2_multiScalarMul : (list Integer) -> (list G2) -> G2` (PV1.1.0). Wire ID 93.
    Bls12_381_G2_MultiScalarMul = 93,
    /// `insertCoin : ByteString -> ByteString -> Integer -> Value -> Value` (PV1.1.0). Wire ID 94.
    InsertCoin = 94,
    /// `lookupCoin : ByteString -> ByteString -> Value -> Integer` (PV1.1.0). Wire ID 95.
    LookupCoin = 95,
    /// `unionValue : Value -> Value -> Value` (PV1.1.0). Wire ID 96.
    UnionValue = 96,
    /// `valueContains : Value -> Value -> Bool` (PV1.1.0). Wire ID 97.
    ValueContains = 97,
    /// `valueData : Value -> Data` (PV1.1.0). Wire ID 98.
    ValueData = 98,
    /// `unValueData : Data -> Value` (PV1.1.0). Wire ID 99.
    UnValueData = 99,
    /// `scaleValue : Integer -> Value -> Value` (PV1.1.0). Wire ID 100.
    ScaleValue = 100,
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
            58 => Ok(BuiltinId::Bls12_381_G1_Compress),
            59 => Ok(BuiltinId::Bls12_381_G1_Uncompress),
            60 => Ok(BuiltinId::Bls12_381_G1_HashToGroup),
            61 => Ok(BuiltinId::Bls12_381_G2_Add),
            62 => Ok(BuiltinId::Bls12_381_G2_Neg),
            63 => Ok(BuiltinId::Bls12_381_G2_ScalarMul),
            64 => Ok(BuiltinId::Bls12_381_G2_Equal),
            65 => Ok(BuiltinId::Bls12_381_G2_Compress),
            66 => Ok(BuiltinId::Bls12_381_G2_Uncompress),
            67 => Ok(BuiltinId::Bls12_381_G2_HashToGroup),
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
            88 => Ok(BuiltinId::DropList),
            89 => Ok(BuiltinId::LengthOfArray),
            90 => Ok(BuiltinId::ListToArray),
            91 => Ok(BuiltinId::IndexArray),
            92 => Ok(BuiltinId::Bls12_381_G1_MultiScalarMul),
            93 => Ok(BuiltinId::Bls12_381_G2_MultiScalarMul),
            94 => Ok(BuiltinId::InsertCoin),
            95 => Ok(BuiltinId::LookupCoin),
            96 => Ok(BuiltinId::UnionValue),
            97 => Ok(BuiltinId::ValueContains),
            98 => Ok(BuiltinId::ValueData),
            99 => Ok(BuiltinId::UnValueData),
            100 => Ok(BuiltinId::ScaleValue),
            _ => Err(crate::UplcError::FlatDecode(format!(
                "unknown builtin id {raw} (max recognised: 100)"
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

    /// Whether this builtin is *available* (exists at all, independent of
    /// its cost/denotation) for `(language, major_pv)`.
    ///
    /// Mirrors Haskell `builtinsAvailableIn :: PlutusLedgerLanguage ->
    /// MajorProtocolVersion -> Set.Set DefaultFun`
    /// (`plutus-ledger-api/src/PlutusLedgerApi/Common/Versions.hs`), which
    /// folds `builtinsIntroducedIn` up to and including `major_pv`. This is
    /// a **distinct axis** from [`crate::builtin::semantics::SemanticsVariant`]
    /// (which governs the *denotation/costing* of a builtin that is already
    /// available) — a builtin can be unavailable (this check fails, decode
    /// must reject) or available-but-differently-costed (semantics variant
    /// applies once evaluation proceeds).
    ///
    /// The six wire-ID batches below are contiguous ranges by construction
    /// (`BuiltinId`'s discriminants are assigned in Haskell `DefaultFun`
    /// declaration order, which is itself batch-ordered), so matching on
    /// `as_u8()` ranges reproduces the authoritative table exactly. Batch
    /// boundaries and the "earliest (language, PV)" cells are verified
    /// against IntersectMBO/plutus tag `1.65.0.0`
    /// (`.claude/agent-memory/cardano-haskell-oracle/plutus-builtin-availability-gate.md`,
    /// section 4's compact table):
    ///
    /// | batch | wire IDs | V1 from | V2 from | V3 from |
    /// |---|---|---|---|---|
    /// | 1 (Alonzo base set)                     | 0–50   | PV5  | PV7  | PV9  |
    /// | 2 (`serialiseData`)                     | 51     | PV11 | PV7  | PV9  |
    /// | 3 (ECDSA/Schnorr secp256k1)              | 52–53  | PV11 | PV8  | PV9  |
    /// | 4a (BLS12-381, `keccak_256`,`blake2b_224`)| 54–72 | PV11 | PV11 | PV9  |
    /// | 4b (`integerToByteString`/`byteStringToInteger`) | 73–74 | PV11 | PV10 | PV9 |
    /// | 5 (bitwise ops, `ripemd_160`)            | 75–86  | PV11 | PV11 | PV10 |
    /// | 6 (`expModInteger`, `dropList`, array/value ops) | 87–100 | PV11 | PV11 | PV11 |
    ///
    /// Notably PlutusV1 is **not** frozen at the Alonzo base set forever:
    /// from PV11 (`vanRossemPV`) a V1 script may reference every later
    /// batch (2 through 6) at once — Haskell's `builtinsIntroducedIn
    /// PlutusV1` has exactly two map entries (`alonzoPV`, `vanRossemPV`).
    pub fn is_available_in(self, language: ScriptLanguage, major_pv: u32) -> bool {
        let earliest_pv: u32 = match self.as_u8() {
            // batch1: Alonzo-era arithmetic/bytestring/string/data/list/pair.
            0..=50 => match language {
                ScriptLanguage::PlutusV1 => 5,
                ScriptLanguage::PlutusV2 => 7,
                ScriptLanguage::PlutusV3 => 9,
            },
            // batch2: SerialiseData.
            51 => match language {
                ScriptLanguage::PlutusV1 => 11,
                ScriptLanguage::PlutusV2 => 7,
                ScriptLanguage::PlutusV3 => 9,
            },
            // batch3: VerifyEcdsaSecp256k1Signature, VerifySchnorrSecp256k1Signature.
            52..=53 => match language {
                ScriptLanguage::PlutusV1 => 11,
                ScriptLanguage::PlutusV2 => 8,
                ScriptLanguage::PlutusV3 => 9,
            },
            // batch4a: BLS12-381 G1/G2/pairing ops, Keccak_256, Blake2b_224.
            54..=72 => match language {
                ScriptLanguage::PlutusV1 => 11,
                ScriptLanguage::PlutusV2 => 11,
                ScriptLanguage::PlutusV3 => 9,
            },
            // batch4b: IntegerToByteString, ByteStringToInteger.
            73..=74 => match language {
                ScriptLanguage::PlutusV1 => 11,
                ScriptLanguage::PlutusV2 => 10,
                ScriptLanguage::PlutusV3 => 9,
            },
            // batch5: bitwise ops, Ripemd_160.
            75..=86 => match language {
                ScriptLanguage::PlutusV1 => 11,
                ScriptLanguage::PlutusV2 => 11,
                ScriptLanguage::PlutusV3 => 10,
            },
            // batch6 (87..=100, and any future addition defaults here too):
            // ExpModInteger, DropList, array ops, value ops.
            _ => match language {
                ScriptLanguage::PlutusV1 => 11,
                ScriptLanguage::PlutusV2 => 11,
                ScriptLanguage::PlutusV3 => 11,
            },
        };
        major_pv >= earliest_pv
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
            BuiltinId::DropList => "dropList",
            BuiltinId::IndexArray => "indexArray",
            BuiltinId::LengthOfArray => "lengthOfArray",
            BuiltinId::ListToArray => "listToArray",
            BuiltinId::InsertCoin => "insertCoin",
            BuiltinId::LookupCoin => "lookupCoin",
            BuiltinId::ScaleValue => "scaleValue",
            BuiltinId::UnValueData => "unValueData",
            BuiltinId::ValueData => "valueData",
            BuiltinId::ValueContains => "valueContains",
            BuiltinId::UnionValue => "unionValue",
            BuiltinId::Bls12_381_G1_MultiScalarMul => "bls12_381_G1_multiScalarMul",
            BuiltinId::Bls12_381_G2_MultiScalarMul => "bls12_381_G2_multiScalarMul",
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
        for raw in 0u8..=100 {
            let id = BuiltinId::from_u8(raw).unwrap_or_else(|e| {
                panic!("from_u8({raw}) failed: {e}");
            });
            assert_eq!(id.as_u8(), raw, "as_u8 round-trip for raw={raw}");
        }
        for raw in [101u8, 127] {
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
        for raw in 0u8..=100 {
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
    fn is_available_in_batch1_base_set_per_language() {
        // batch1 (wire 0-50): V1@5, V2@7, V3@9.
        let id = BuiltinId::AddInteger;
        assert!(!id.is_available_in(ScriptLanguage::PlutusV1, 4));
        assert!(id.is_available_in(ScriptLanguage::PlutusV1, 5));
        assert!(!id.is_available_in(ScriptLanguage::PlutusV2, 6));
        assert!(id.is_available_in(ScriptLanguage::PlutusV2, 7));
        assert!(!id.is_available_in(ScriptLanguage::PlutusV3, 8));
        assert!(id.is_available_in(ScriptLanguage::PlutusV3, 9));
    }

    #[test]
    fn is_available_in_batch2_serialise_data() {
        // batch2 (wire 51): V1@11, V2@7 (bundled with batch1), V3@9.
        let id = BuiltinId::SerialiseData;
        assert!(!id.is_available_in(ScriptLanguage::PlutusV1, 10));
        assert!(id.is_available_in(ScriptLanguage::PlutusV1, 11));
        assert!(!id.is_available_in(ScriptLanguage::PlutusV2, 6));
        assert!(id.is_available_in(ScriptLanguage::PlutusV2, 7));
        assert!(!id.is_available_in(ScriptLanguage::PlutusV3, 8));
        assert!(id.is_available_in(ScriptLanguage::PlutusV3, 9));
    }

    #[test]
    fn is_available_in_batch3_ecdsa_schnorr() {
        // batch3 (wire 52-53): V1@11, V2@8 (valentine intra-Babbage HF), V3@9.
        for id in [
            BuiltinId::VerifyEcdsaSecp256k1Signature,
            BuiltinId::VerifySchnorrSecp256k1Signature,
        ] {
            assert!(!id.is_available_in(ScriptLanguage::PlutusV1, 10));
            assert!(id.is_available_in(ScriptLanguage::PlutusV1, 11));
            assert!(!id.is_available_in(ScriptLanguage::PlutusV2, 7));
            assert!(id.is_available_in(ScriptLanguage::PlutusV2, 8));
            assert!(!id.is_available_in(ScriptLanguage::PlutusV3, 8));
            assert!(id.is_available_in(ScriptLanguage::PlutusV3, 9));
        }
    }

    #[test]
    fn is_available_in_batch4a_bls_keccak_blake2b224() {
        // batch4a (wire 54-72): V1@11, V2@11, V3@9 — a V1/V2 script referencing
        // BLS12-381 before PV11 must be rejected (this is the #821 headline case).
        for id in [
            BuiltinId::Bls12_381_G1_Add,
            BuiltinId::Bls12_381_G2_HashToGroup,
            BuiltinId::Bls12_381_FinalVerify,
            BuiltinId::Keccak_256,
            BuiltinId::Blake2b_224,
        ] {
            assert!(!id.is_available_in(ScriptLanguage::PlutusV1, 10));
            assert!(id.is_available_in(ScriptLanguage::PlutusV1, 11));
            assert!(!id.is_available_in(ScriptLanguage::PlutusV2, 10));
            assert!(id.is_available_in(ScriptLanguage::PlutusV2, 11));
            assert!(!id.is_available_in(ScriptLanguage::PlutusV3, 8));
            assert!(id.is_available_in(ScriptLanguage::PlutusV3, 9));
        }
    }

    #[test]
    fn is_available_in_batch4b_integer_bytestring_conversions() {
        // batch4b (wire 73-74): V1@11, V2@10 (plomin — earlier than the rest of
        // batch4), V3@9.
        for id in [
            BuiltinId::IntegerToByteString,
            BuiltinId::ByteStringToInteger,
        ] {
            assert!(!id.is_available_in(ScriptLanguage::PlutusV1, 10));
            assert!(id.is_available_in(ScriptLanguage::PlutusV1, 11));
            assert!(!id.is_available_in(ScriptLanguage::PlutusV2, 9));
            assert!(id.is_available_in(ScriptLanguage::PlutusV2, 10));
            assert!(!id.is_available_in(ScriptLanguage::PlutusV3, 8));
            assert!(id.is_available_in(ScriptLanguage::PlutusV3, 9));
        }
    }

    #[test]
    fn is_available_in_batch5_bitwise_and_ripemd160() {
        // batch5 (wire 75-86): V1@11, V2@11, V3@10.
        for id in [BuiltinId::AndByteString, BuiltinId::Ripemd_160] {
            assert!(!id.is_available_in(ScriptLanguage::PlutusV1, 10));
            assert!(id.is_available_in(ScriptLanguage::PlutusV1, 11));
            assert!(!id.is_available_in(ScriptLanguage::PlutusV2, 10));
            assert!(id.is_available_in(ScriptLanguage::PlutusV2, 11));
            assert!(!id.is_available_in(ScriptLanguage::PlutusV3, 9));
            assert!(id.is_available_in(ScriptLanguage::PlutusV3, 10));
        }
    }

    #[test]
    fn is_available_in_batch6_expmod_droplist_array_value_ops() {
        // batch6 (wire 87-100): all languages @11 — still-open batch, so the
        // catch-all `_` arm of `is_available_in` covers it.
        for id in [
            BuiltinId::ExpModInteger,
            BuiltinId::DropList,
            BuiltinId::LengthOfArray,
            BuiltinId::UnionValue,
            BuiltinId::ScaleValue,
        ] {
            assert!(!id.is_available_in(ScriptLanguage::PlutusV1, 10));
            assert!(id.is_available_in(ScriptLanguage::PlutusV1, 11));
            assert!(!id.is_available_in(ScriptLanguage::PlutusV2, 10));
            assert!(id.is_available_in(ScriptLanguage::PlutusV2, 11));
            assert!(!id.is_available_in(ScriptLanguage::PlutusV3, 10));
            assert!(id.is_available_in(ScriptLanguage::PlutusV3, 11));
        }
    }

    #[test]
    fn is_available_in_v1_gains_every_later_batch_at_pv11_at_once() {
        // PlutusV1 has exactly two `builtinsIntroducedIn` map entries
        // (alonzoPV, vanRossemPV) — everything from batch2 onward becomes
        // available simultaneously at PV11, not staggered like V2/V3.
        for id in [
            BuiltinId::SerialiseData,
            BuiltinId::VerifyEcdsaSecp256k1Signature,
            BuiltinId::Bls12_381_G1_Add,
            BuiltinId::IntegerToByteString,
            BuiltinId::AndByteString,
            BuiltinId::ExpModInteger,
        ] {
            assert!(!id.is_available_in(ScriptLanguage::PlutusV1, 10));
            assert!(id.is_available_in(ScriptLanguage::PlutusV1, 11));
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
