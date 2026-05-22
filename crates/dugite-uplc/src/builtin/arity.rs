//! Per-builtin force-count + argument arity.
//!
//! Each builtin has a fixed number of *forces* (leading `Force`s the
//! script must apply before the first argument — Plutus's
//! type-instantiation surrogate in the untyped language) and a fixed
//! *arity* (the value-argument count after the forces). Both are
//! normative against the Haskell `DefaultFun` definitions in
//! `IntersectMBO/plutus:plutus-core/.../Default/Builtins.hs`.

use crate::term::BuiltinId;

/// `(forces, arity)` for the given builtin.
pub const fn arity_of(id: BuiltinId) -> (u8, u8) {
    use BuiltinId::*;
    match id {
        // Integer arithmetic — V1.
        AddInteger
        | SubtractInteger
        | MultiplyInteger
        | DivideInteger
        | QuotientInteger
        | RemainderInteger
        | ModInteger
        | EqualsInteger
        | LessThanInteger
        | LessThanEqualsInteger => (0, 2),

        // ByteString operations — V1.
        AppendByteString | ConsByteString => (0, 2),
        SliceByteString => (0, 3),
        LengthOfByteString => (0, 1),
        IndexByteString => (0, 2),
        EqualsByteString | LessThanByteString | LessThanEqualsByteString => (0, 2),

        // Hashes — V1.
        Sha2_256 | Sha3_256 | Blake2b_256 => (0, 1),

        // ed25519 — V1.
        VerifyEd25519Signature => (0, 3),

        // String/UTF-8 — V1.
        AppendString | EqualsString => (0, 2),
        EncodeUtf8 | DecodeUtf8 => (0, 1),

        // Polymorphic helpers — V1.
        IfThenElse => (1, 3),
        ChooseUnit => (1, 2),
        Trace => (1, 2),

        // Pairs/lists — V1.
        FstPair | SndPair => (2, 1),
        ChooseList => (2, 3),
        MkCons => (1, 2),
        HeadList | TailList | NullList => (1, 1),

        // Data — V1.
        ChooseData => (1, 6),
        ConstrData => (0, 2),
        MapData | ListData | IData | BData => (0, 1),
        UnConstrData | UnMapData | UnListData | UnIData | UnBData => (0, 1),
        EqualsData => (0, 2),
        MkPairData => (0, 2),
        MkNilData | MkNilPairData => (0, 1),

        // V2 additions (CIP-0033).
        SerialiseData => (0, 1),
        VerifyEcdsaSecp256k1Signature | VerifySchnorrSecp256k1Signature => (0, 3),

        // V3 BLS12-381 ops (CIP-0381).
        Bls12_381_G1_Add | Bls12_381_G1_Equal => (0, 2),
        Bls12_381_G1_Neg | Bls12_381_G1_Compress | Bls12_381_G1_Uncompress => (0, 1),
        Bls12_381_G1_ScalarMul | Bls12_381_G1_HashToGroup => (0, 2),
        Bls12_381_G2_Add | Bls12_381_G2_Equal => (0, 2),
        Bls12_381_G2_Neg | Bls12_381_G2_Compress | Bls12_381_G2_Uncompress => (0, 1),
        Bls12_381_G2_ScalarMul | Bls12_381_G2_HashToGroup => (0, 2),
        Bls12_381_MillerLoop | Bls12_381_MulMlResult => (0, 2),
        Bls12_381_FinalVerify => (0, 2),

        // V3 hashes (CIP-0127 + CIP-0101).
        Keccak_256 | Blake2b_224 => (0, 1),
        Ripemd_160 => (0, 1),

        // V3 Int↔ByteString (CIP-0117).
        IntegerToByteString => (0, 3),
        ByteStringToInteger => (0, 2),

        // V3 bitwise (CIP-0123).
        AndByteString | OrByteString | XorByteString => (0, 3),
        ComplementByteString => (0, 1),
        ReadBit => (0, 2),
        WriteBits => (0, 3),
        ReplicateByte => (0, 2),
        ShiftByteString | RotateByteString => (0, 2),
        CountSetBits | FindFirstSetBit => (0, 1),

        // V3 modular exponentiation.
        ExpModInteger => (0, 3),

        // PV1.1.0 list / array.
        DropList => (1, 2),      // (Integer, list T) -> list T
        IndexArray => (1, 2),    // (array T, Integer) -> T
        LengthOfArray => (1, 1), // array T -> Integer
        ListToArray => (1, 1),   // list T -> array T

        // PV1.1.0 Value builtins.
        InsertCoin => (0, 4), // ByteString -> ByteString -> Integer -> Value -> Value
        LookupCoin => (0, 3), // ByteString -> ByteString -> Value -> Integer
        ScaleValue => (0, 2), // Integer -> Value -> Value
        UnValueData => (0, 1), // Data -> Value
        ValueData => (0, 1),  // Value -> Data
        ValueContains => (0, 2), // Value -> Value -> Bool
        UnionValue => (0, 2), // Value -> Value -> Value

        // PV1.1.0 BLS multi-scalar multiplication.
        Bls12_381_G1_MultiScalarMul => (0, 2), // list Integer -> list G1 -> G1
        Bls12_381_G2_MultiScalarMul => (0, 2), // list Integer -> list G2 -> G2
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arity_total_for_every_builtin() {
        for raw in 0u8..=100 {
            let id = BuiltinId::from_u8(raw).unwrap();
            let (forces, args) = arity_of(id);
            assert!(
                forces + args > 0,
                "{} (raw={raw}) has zero forces+args: ({forces}, {args})",
                id.name()
            );
        }
    }

    #[test]
    fn arity_examples_match_haskell() {
        assert_eq!(arity_of(BuiltinId::AddInteger), (0, 2));
        assert_eq!(arity_of(BuiltinId::Sha2_256), (0, 1));
        assert_eq!(arity_of(BuiltinId::VerifyEd25519Signature), (0, 3));
        assert_eq!(arity_of(BuiltinId::IfThenElse), (1, 3));
        assert_eq!(arity_of(BuiltinId::FstPair), (2, 1));
        assert_eq!(arity_of(BuiltinId::ChooseData), (1, 6));
        assert_eq!(arity_of(BuiltinId::Bls12_381_MillerLoop), (0, 2));
        assert_eq!(arity_of(BuiltinId::IntegerToByteString), (0, 3));
    }
}
