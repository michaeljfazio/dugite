//! Builtin denotations.
//!
//! Each entry in [`denote`] implements the Haskell-reference
//! semantics for one builtin. The set is expanded one PR at a time;
//! entries not yet wired surface as a typed
//! [`UplcError::Internal`] so the CEK harness reports a clear gap
//! rather than panicking.
//!
//! This commit lands the integer-arithmetic and bool builtins (no
//! external crypto crates needed). Subsequent commits add bytestring,
//! string, list, pair, data, and crypto suites.

use crate::machine::value::Value;
use crate::term::{BuiltinId, Constant};
use crate::UplcError;
use num_bigint::BigInt;

/// Saturated-application denotation. The CEK dispatcher calls this
/// once both the force count and the value-argument count match the
/// builtin's arity.
pub fn denote(id: BuiltinId, args: Vec<Value>) -> Result<Value, UplcError> {
    use BuiltinId::*;
    match id {
        // ── Integer arithmetic (V1) ─────────────────────────────────
        AddInteger => int_binop(args, id, |a, b| Ok(a + b)),
        SubtractInteger => int_binop(args, id, |a, b| Ok(a - b)),
        MultiplyInteger => int_binop(args, id, |a, b| Ok(a * b)),
        DivideInteger => int_binop(args, id, |a, b| {
            if b.sign() == num_bigint::Sign::NoSign {
                return Err(builtin_failure(id, "divide by zero"));
            }
            Ok(divide_haskell_div(&a, &b))
        }),
        QuotientInteger => int_binop(args, id, |a, b| {
            if b.sign() == num_bigint::Sign::NoSign {
                return Err(builtin_failure(id, "quotient by zero"));
            }
            // Haskell `quot` truncates toward zero.
            Ok(a / b)
        }),
        RemainderInteger => int_binop(args, id, |a, b| {
            if b.sign() == num_bigint::Sign::NoSign {
                return Err(builtin_failure(id, "remainder by zero"));
            }
            // Haskell `rem` matches `quot`: same sign as dividend.
            Ok(a % b)
        }),
        ModInteger => int_binop(args, id, |a, b| {
            if b.sign() == num_bigint::Sign::NoSign {
                return Err(builtin_failure(id, "modulo by zero"));
            }
            Ok(mod_haskell_mod(&a, &b))
        }),
        EqualsInteger => int_cmp(args, id, |a, b| a == b),
        LessThanInteger => int_cmp(args, id, |a, b| a < b),
        LessThanEqualsInteger => int_cmp(args, id, |a, b| a <= b),

        // ── Polymorphic helpers (V1) ────────────────────────────────
        // `IfThenElse` returns the second or third argument depending
        // on the boolean first.
        IfThenElse => {
            let mut it = args.into_iter();
            let cond = it.next().ok_or_else(|| builtin_arity_mismatch(id))?;
            let then_branch = it.next().ok_or_else(|| builtin_arity_mismatch(id))?;
            let else_branch = it.next().ok_or_else(|| builtin_arity_mismatch(id))?;
            match cond {
                Value::Const(Constant::Bool(true)) => Ok(then_branch),
                Value::Const(Constant::Bool(false)) => Ok(else_branch),
                other => Err(UplcError::BuiltinTypeError {
                    builtin: id.name(),
                    reason: format!(
                        "ifThenElse expected Bool condition, got {:?}",
                        std::mem::discriminant(&other)
                    ),
                }),
            }
        }

        // Returns its second argument unchanged after observing the
        // (Unit-typed) first argument. Useful for sequencing.
        ChooseUnit => {
            let mut it = args.into_iter();
            let unit = it.next().ok_or_else(|| builtin_arity_mismatch(id))?;
            let rest = it.next().ok_or_else(|| builtin_arity_mismatch(id))?;
            match unit {
                Value::Const(Constant::Unit) => Ok(rest),
                other => Err(UplcError::BuiltinTypeError {
                    builtin: id.name(),
                    reason: format!(
                        "chooseUnit expected Unit first arg, got {:?}",
                        std::mem::discriminant(&other)
                    ),
                }),
            }
        }

        // Trace: emit a log message (currently discarded — the CEK
        // machine's `EvalResult.logs` will accumulate them once the
        // tracer is wired) and return the second argument.
        Trace => {
            let mut it = args.into_iter();
            let _msg = it.next().ok_or_else(|| builtin_arity_mismatch(id))?;
            let rest = it.next().ok_or_else(|| builtin_arity_mismatch(id))?;
            Ok(rest)
        }

        // ── ByteString operations (V1) ──────────────────────────────
        AppendByteString => {
            let (a, b) = take_two_byte_strings(args, id)?;
            let mut out = a;
            out.extend_from_slice(&b);
            Ok(Value::Const(Constant::ByteString(out)))
        }
        ConsByteString => {
            // First arg is an Integer in 0..=255; second is a ByteString.
            // Haskell: `consByteString : i -> bs -> (i `mod` 256) `BS.cons` bs`
            // The Plutus V2+ semantics CHANGE: range-check (0..=255)
            // and reject out-of-range. We follow V2 semantics by
            // default (which is mainnet).
            let mut it = args.into_iter();
            let i = unwrap_integer(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let bs = unwrap_byte_string(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            // Range check (V2+).
            let i_u8 = bigint_to_u8(&i, id, "cons byte must be 0..=255")?;
            let mut out = Vec::with_capacity(1 + bs.len());
            out.push(i_u8);
            out.extend_from_slice(&bs);
            Ok(Value::Const(Constant::ByteString(out)))
        }
        SliceByteString => {
            // sliceByteString : start -> length -> bs -> sliced
            // Haskell: BS.take length (BS.drop start bs).
            // Negative or out-of-range indices clamp to zero / EOF.
            let mut it = args.into_iter();
            let start = unwrap_integer(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let len = unwrap_integer(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let bs = unwrap_byte_string(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let start = bigint_to_usize_clamped(&start);
            let len = bigint_to_usize_clamped(&len);
            let start_clamped = start.min(bs.len());
            let end_clamped = start_clamped.saturating_add(len).min(bs.len());
            Ok(Value::Const(Constant::ByteString(
                bs[start_clamped..end_clamped].to_vec(),
            )))
        }
        LengthOfByteString => {
            let mut it = args.into_iter();
            let bs = unwrap_byte_string(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            Ok(Value::Const(Constant::Integer(BigInt::from(bs.len()))))
        }
        IndexByteString => {
            // Haskell: indexByteString bs i = BS.index bs (fromInteger i).
            // If i is out of range, this is a BuiltinFailure.
            let mut it = args.into_iter();
            let bs = unwrap_byte_string(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let i = unwrap_integer(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let idx = match usize::try_from(&i) {
                Ok(n) if n < bs.len() => n,
                _ => {
                    return Err(builtin_failure(id, "indexByteString: index out of range"));
                }
            };
            Ok(Value::Const(Constant::Integer(BigInt::from(bs[idx]))))
        }
        EqualsByteString => {
            let (a, b) = take_two_byte_strings(args, id)?;
            Ok(Value::Const(Constant::Bool(a == b)))
        }
        LessThanByteString => {
            let (a, b) = take_two_byte_strings(args, id)?;
            Ok(Value::Const(Constant::Bool(a < b)))
        }
        LessThanEqualsByteString => {
            let (a, b) = take_two_byte_strings(args, id)?;
            Ok(Value::Const(Constant::Bool(a <= b)))
        }

        // ── String operations (V1) ──────────────────────────────────
        AppendString => {
            let (a, b) = take_two_strings(args, id)?;
            let mut out = a;
            out.push_str(&b);
            Ok(Value::Const(Constant::String(out)))
        }
        EqualsString => {
            let (a, b) = take_two_strings(args, id)?;
            Ok(Value::Const(Constant::Bool(a == b)))
        }
        EncodeUtf8 => {
            let mut it = args.into_iter();
            let s = unwrap_string(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            Ok(Value::Const(Constant::ByteString(s.into_bytes())))
        }
        DecodeUtf8 => {
            let mut it = args.into_iter();
            let bs = unwrap_byte_string(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let s = String::from_utf8(bs)
                .map_err(|e| builtin_failure(id, &format!("decodeUtf8: not valid UTF-8: {e}")))?;
            Ok(Value::Const(Constant::String(s)))
        }

        // ── Hash functions (V1 + V2 + V3) ─────────────────────────────
        Sha2_256 => hash_one(args, id, |bs| {
            use sha2::Digest;
            sha2::Sha256::digest(bs).to_vec()
        }),
        Sha3_256 => hash_one(args, id, |bs| {
            use sha3::Digest;
            sha3::Sha3_256::digest(bs).to_vec()
        }),
        Blake2b_256 => hash_one(args, id, |bs| {
            use blake2::Digest;
            blake2::Blake2b::<blake2::digest::consts::U32>::digest(bs).to_vec()
        }),
        Blake2b_224 => hash_one(args, id, |bs| {
            use blake2::Digest;
            blake2::Blake2b::<blake2::digest::consts::U28>::digest(bs).to_vec()
        }),
        Keccak_256 => hash_one(args, id, |bs| {
            use sha3::Digest;
            sha3::Keccak256::digest(bs).to_vec()
        }),
        Ripemd_160 => hash_one(args, id, |bs| {
            use ripemd::Digest;
            ripemd::Ripemd160::digest(bs).to_vec()
        }),

        // ── Ed25519 signature verification (V1) ───────────────────────
        VerifyEd25519Signature => {
            // (pub_key : ByteString(32)) (message : ByteString) (sig : ByteString(64)) -> Bool
            let mut it = args.into_iter();
            let pk = unwrap_byte_string(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let msg = unwrap_byte_string(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let sig = unwrap_byte_string(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            if pk.len() != 32 {
                return Err(builtin_failure(
                    id,
                    &format!("ed25519 public key must be 32 bytes, got {}", pk.len()),
                ));
            }
            if sig.len() != 64 {
                return Err(builtin_failure(
                    id,
                    &format!("ed25519 signature must be 64 bytes, got {}", sig.len()),
                ));
            }
            let pk_bytes: [u8; 32] = pk.try_into().map_err(|_| {
                UplcError::Internal("ed25519 pk length check failed after success".into())
            })?;
            let sig_bytes: [u8; 64] = sig.try_into().map_err(|_| {
                UplcError::Internal("ed25519 sig length check failed after success".into())
            })?;
            let vk = match ed25519_dalek::VerifyingKey::from_bytes(&pk_bytes) {
                Ok(vk) => vk,
                Err(_) => {
                    // Malformed public key: Haskell semantics is to
                    // return `False`, not crash.
                    return Ok(Value::Const(Constant::Bool(false)));
                }
            };
            let sig = ed25519_dalek::Signature::from_bytes(&sig_bytes);
            Ok(Value::Const(Constant::Bool(
                vk.verify_strict(&msg, &sig).is_ok(),
            )))
        }

        // ── List builtins (V1) ────────────────────────────────────────
        HeadList => {
            let v = take_one(args, id)?;
            let (_, elems) = unwrap_proto_list(v, id)?;
            elems
                .into_iter()
                .next()
                .map(Value::Const)
                .ok_or_else(|| builtin_failure(id, "headList on empty list"))
        }
        TailList => {
            let v = take_one(args, id)?;
            let (elem_type, mut elems) = unwrap_proto_list(v, id)?;
            if elems.is_empty() {
                return Err(builtin_failure(id, "tailList on empty list"));
            }
            elems.remove(0);
            Ok(Value::Const(Constant::ProtoList {
                elem_type,
                elements: elems,
            }))
        }
        NullList => {
            let v = take_one(args, id)?;
            let (_, elems) = unwrap_proto_list(v, id)?;
            Ok(Value::Const(Constant::Bool(elems.is_empty())))
        }
        ChooseList => {
            // (list : ProtoList) (nil_case) (cons_case) -> branch
            let mut it = args.into_iter();
            let list = it.next().ok_or_else(|| builtin_arity_mismatch(id))?;
            let nil_case = it.next().ok_or_else(|| builtin_arity_mismatch(id))?;
            let cons_case = it.next().ok_or_else(|| builtin_arity_mismatch(id))?;
            let (_, elems) = unwrap_proto_list(list, id)?;
            Ok(if elems.is_empty() {
                nil_case
            } else {
                cons_case
            })
        }
        MkCons => {
            // (head : Constant) (list : ProtoList) -> ProtoList
            let mut it = args.into_iter();
            let head = it.next().ok_or_else(|| builtin_arity_mismatch(id))?;
            let list = it.next().ok_or_else(|| builtin_arity_mismatch(id))?;
            let (elem_type, mut elems) = unwrap_proto_list(list, id)?;
            // The head must be a plain Const value.
            let head_const = match head {
                Value::Const(c) => c,
                _ => {
                    return Err(UplcError::BuiltinTypeError {
                        builtin: id.name(),
                        reason: "mkCons head must be a Constant".into(),
                    })
                }
            };
            // The head's value type must match the list's static
            // element type — Haskell's `readKnown` unlifting raises
            // an evaluation failure otherwise (#603).
            let head_ty = head_const.type_tag();
            if head_ty != elem_type {
                return Err(UplcError::BuiltinTypeError {
                    builtin: id.name(),
                    reason: format!(
                        "mkCons head type {head_ty:?} disagrees with list element type {elem_type:?}",
                    ),
                });
            }
            elems.insert(0, head_const);
            Ok(Value::Const(Constant::ProtoList {
                elem_type,
                elements: elems,
            }))
        }
        MkNilData => {
            // (unit : Unit) -> ProtoList of Data
            let v = take_one(args, id)?;
            match v {
                Value::Const(Constant::Unit) => Ok(Value::Const(Constant::ProtoList {
                    elem_type: crate::term::TypeTag::Data,
                    elements: Vec::new(),
                })),
                _ => Err(UplcError::BuiltinTypeError {
                    builtin: id.name(),
                    reason: "mkNilData expects Unit".into(),
                }),
            }
        }
        MkNilPairData => {
            // (unit : Unit) -> ProtoList of Pair(Data, Data)
            let v = take_one(args, id)?;
            match v {
                Value::Const(Constant::Unit) => Ok(Value::Const(Constant::ProtoList {
                    elem_type: crate::term::TypeTag::Pair(
                        Box::new(crate::term::TypeTag::Data),
                        Box::new(crate::term::TypeTag::Data),
                    ),
                    elements: Vec::new(),
                })),
                _ => Err(UplcError::BuiltinTypeError {
                    builtin: id.name(),
                    reason: "mkNilPairData expects Unit".into(),
                }),
            }
        }

        // ── Pair builtins (V1) ────────────────────────────────────────
        FstPair => {
            let v = take_one(args, id)?;
            let (a, _b) = unwrap_proto_pair(v, id)?;
            Ok(Value::Const(*a))
        }
        SndPair => {
            let v = take_one(args, id)?;
            let (_a, b) = unwrap_proto_pair(v, id)?;
            Ok(Value::Const(*b))
        }

        // ── Data builtins (V1) ────────────────────────────────────────
        ChooseData => {
            // (data) (constr_case) (map_case) (list_case) (i_case) (b_case) -> branch
            let mut it = args.into_iter();
            let d = unwrap_data(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let constr_case = it.next().ok_or_else(|| builtin_arity_mismatch(id))?;
            let map_case = it.next().ok_or_else(|| builtin_arity_mismatch(id))?;
            let list_case = it.next().ok_or_else(|| builtin_arity_mismatch(id))?;
            let i_case = it.next().ok_or_else(|| builtin_arity_mismatch(id))?;
            let b_case = it.next().ok_or_else(|| builtin_arity_mismatch(id))?;
            Ok(match d {
                crate::data::Data::Constr(_, _) => constr_case,
                crate::data::Data::Map(_) => map_case,
                crate::data::Data::List(_) => list_case,
                crate::data::Data::I(_) => i_case,
                crate::data::Data::B(_) => b_case,
            })
        }
        ConstrData => {
            // (tag : Integer) (args : ProtoList Data) -> Data
            let mut it = args.into_iter();
            let tag = unwrap_integer(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let list = it.next().ok_or_else(|| builtin_arity_mismatch(id))?;
            let (_, elems) = unwrap_proto_list(list, id)?;
            let tag_u64 = match u64::try_from(&tag) {
                Ok(n) => n,
                _ => return Err(builtin_failure(id, "constrData tag out of u64 range")),
            };
            let data_args: Result<Vec<crate::data::Data>, _> = elems
                .into_iter()
                .map(|c| match c {
                    Constant::Data(d) => Ok(d),
                    _ => Err(UplcError::BuiltinTypeError {
                        builtin: id.name(),
                        reason: "constrData args must be Data".into(),
                    }),
                })
                .collect();
            Ok(Value::Const(Constant::Data(crate::data::Data::Constr(
                tag_u64, data_args?,
            ))))
        }
        MapData => {
            // (list : ProtoList (Pair Data Data)) -> Data
            let v = take_one(args, id)?;
            let (_, elems) = unwrap_proto_list(v, id)?;
            let pairs: Result<Vec<(crate::data::Data, crate::data::Data)>, _> = elems
                .into_iter()
                .map(|c| match c {
                    Constant::ProtoPair { a, b, .. } => match (*a, *b) {
                        (Constant::Data(k), Constant::Data(vv)) => Ok((k, vv)),
                        _ => Err(UplcError::BuiltinTypeError {
                            builtin: id.name(),
                            reason: "mapData pair components must be Data".into(),
                        }),
                    },
                    _ => Err(UplcError::BuiltinTypeError {
                        builtin: id.name(),
                        reason: "mapData expects ProtoList of ProtoPair".into(),
                    }),
                })
                .collect();
            Ok(Value::Const(Constant::Data(crate::data::Data::Map(pairs?))))
        }
        ListData => {
            // (list : ProtoList Data) -> Data
            let v = take_one(args, id)?;
            let (_, elems) = unwrap_proto_list(v, id)?;
            let datas: Result<Vec<crate::data::Data>, _> = elems
                .into_iter()
                .map(|c| match c {
                    Constant::Data(d) => Ok(d),
                    _ => Err(UplcError::BuiltinTypeError {
                        builtin: id.name(),
                        reason: "listData args must be Data".into(),
                    }),
                })
                .collect();
            Ok(Value::Const(Constant::Data(crate::data::Data::List(
                datas?,
            ))))
        }
        IData => {
            let v = take_one(args, id)?;
            let i = unwrap_integer(v, id)?;
            Ok(Value::Const(Constant::Data(crate::data::Data::I(i))))
        }
        BData => {
            let v = take_one(args, id)?;
            let b = unwrap_byte_string(v, id)?;
            Ok(Value::Const(Constant::Data(crate::data::Data::B(b))))
        }
        UnConstrData => {
            // Data -> Pair Integer (ProtoList Data)
            let d = unwrap_data(take_one(args, id)?, id)?;
            match d {
                crate::data::Data::Constr(tag, fields) => {
                    let elements: Vec<Constant> = fields.into_iter().map(Constant::Data).collect();
                    Ok(Value::Const(Constant::ProtoPair {
                        a_type: crate::term::TypeTag::Integer,
                        b_type: crate::term::TypeTag::List(Box::new(crate::term::TypeTag::Data)),
                        a: Box::new(Constant::Integer(BigInt::from(tag))),
                        b: Box::new(Constant::ProtoList {
                            elem_type: crate::term::TypeTag::Data,
                            elements,
                        }),
                    }))
                }
                _ => Err(builtin_failure(id, "unConstrData on non-Constr Data")),
            }
        }
        UnMapData => {
            let d = unwrap_data(take_one(args, id)?, id)?;
            match d {
                crate::data::Data::Map(entries) => {
                    let elements: Vec<Constant> = entries
                        .into_iter()
                        .map(|(k, v)| Constant::ProtoPair {
                            a_type: crate::term::TypeTag::Data,
                            b_type: crate::term::TypeTag::Data,
                            a: Box::new(Constant::Data(k)),
                            b: Box::new(Constant::Data(v)),
                        })
                        .collect();
                    Ok(Value::Const(Constant::ProtoList {
                        elem_type: crate::term::TypeTag::Pair(
                            Box::new(crate::term::TypeTag::Data),
                            Box::new(crate::term::TypeTag::Data),
                        ),
                        elements,
                    }))
                }
                _ => Err(builtin_failure(id, "unMapData on non-Map Data")),
            }
        }
        UnListData => {
            let d = unwrap_data(take_one(args, id)?, id)?;
            match d {
                crate::data::Data::List(items) => Ok(Value::Const(Constant::ProtoList {
                    elem_type: crate::term::TypeTag::Data,
                    elements: items.into_iter().map(Constant::Data).collect(),
                })),
                _ => Err(builtin_failure(id, "unListData on non-List Data")),
            }
        }
        UnIData => {
            let d = unwrap_data(take_one(args, id)?, id)?;
            match d {
                crate::data::Data::I(i) => Ok(Value::Const(Constant::Integer(i))),
                _ => Err(builtin_failure(id, "unIData on non-I Data")),
            }
        }
        UnBData => {
            let d = unwrap_data(take_one(args, id)?, id)?;
            match d {
                crate::data::Data::B(b) => Ok(Value::Const(Constant::ByteString(b))),
                _ => Err(builtin_failure(id, "unBData on non-B Data")),
            }
        }
        EqualsData => {
            let mut it = args.into_iter();
            let a = unwrap_data(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let b = unwrap_data(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            Ok(Value::Const(Constant::Bool(a == b)))
        }
        MkPairData => {
            // (a : Data) (b : Data) -> Pair Data Data
            let mut it = args.into_iter();
            let a = unwrap_data(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let b = unwrap_data(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            Ok(Value::Const(Constant::ProtoPair {
                a_type: crate::term::TypeTag::Data,
                b_type: crate::term::TypeTag::Data,
                a: Box::new(Constant::Data(a)),
                b: Box::new(Constant::Data(b)),
            }))
        }
        SerialiseData => {
            // Data -> ByteString (CBOR encoding)
            let d = unwrap_data(take_one(args, id)?, id)?;
            let bytes = d
                .to_cbor()
                .map_err(|e| builtin_failure(id, &format!("serialiseData: {e}")))?;
            Ok(Value::Const(Constant::ByteString(bytes)))
        }

        // ── CIP-0117 Integer ↔ ByteString conversions (V3) ────────────
        IntegerToByteString => {
            // (endianness: Bool, big-endian if True) (width: Integer) (n: Integer) -> ByteString
            // If width = 0, output is the minimal big-endian
            // representation of n. Else output is exactly `width`
            // bytes (zero-padded on the high-order side). Negative n
            // is a BuiltinFailure.
            let mut it = args.into_iter();
            let endian = unwrap_bool(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let width = unwrap_integer(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let n = unwrap_integer(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            if n.sign() == num_bigint::Sign::Minus {
                return Err(builtin_failure(id, "integerToByteString: negative input"));
            }
            let width_u = bigint_to_usize_or_failure(&width, id, "integerToByteString width")?;
            // Reasonable cap to avoid runaway allocation.  Matches the
            // Haskell reference's `integerToByteStringMaximumOutputLength`
            // (= 8192 bytes = 65536 bits); CIP-0117 mandates the limit.
            const MAX_INT_TO_BS_WIDTH: usize = 8192;
            if width_u > MAX_INT_TO_BS_WIDTH {
                return Err(builtin_failure(
                    id,
                    "integerToByteString: width exceeds 8192-byte cap",
                ));
            }
            let mut be_bytes = n.to_bytes_be().1;
            if be_bytes.len() == 1 && be_bytes[0] == 0 && width_u == 0 {
                // n = 0 with auto-width → empty bytestring (matches
                // Haskell's `integerToByteStringBE 0`).
                return Ok(Value::Const(Constant::ByteString(Vec::new())));
            }
            if width_u == 0 {
                // Auto-width: the minimal representation must itself
                // fit within the 8192-byte CIP-0117 cap; n >= 2^65536
                // is rejected as evaluation failure (#603).
                if be_bytes.len() > MAX_INT_TO_BS_WIDTH {
                    return Err(builtin_failure(
                        id,
                        "integerToByteString: value exceeds 8192-byte representable range",
                    ));
                }
            } else {
                if be_bytes.len() > width_u {
                    return Err(builtin_failure(
                        id,
                        "integerToByteString: value doesn't fit in declared width",
                    ));
                }
                let mut padded = vec![0u8; width_u - be_bytes.len()];
                padded.append(&mut be_bytes);
                be_bytes = padded;
            }
            if !endian {
                be_bytes.reverse();
            }
            Ok(Value::Const(Constant::ByteString(be_bytes)))
        }
        ByteStringToInteger => {
            // (endianness: Bool) (bs: ByteString) -> Integer
            let mut it = args.into_iter();
            let endian = unwrap_bool(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let bs = unwrap_byte_string(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let mut be_bytes = bs;
            if !endian {
                be_bytes.reverse();
            }
            Ok(Value::Const(Constant::Integer(BigInt::from_bytes_be(
                num_bigint::Sign::Plus,
                &be_bytes,
            ))))
        }

        // ── CIP-0123 bitwise (V3) ─────────────────────────────────────
        AndByteString => bitwise_byte_string(args, id, |a, b| a & b, 0),
        OrByteString => bitwise_byte_string(args, id, |a, b| a | b, 0),
        XorByteString => bitwise_byte_string(args, id, |a, b| a ^ b, 0),
        ComplementByteString => {
            let v = take_one(args, id)?;
            let bs = unwrap_byte_string(v, id)?;
            let out: Vec<u8> = bs.iter().map(|b| !*b).collect();
            Ok(Value::Const(Constant::ByteString(out)))
        }
        ReadBit => {
            // (bs: ByteString) (i: Integer) -> Bool
            // Bit 0 = LSB of byte 0. (Per CIP-0123 §"Bit ordering".)
            let mut it = args.into_iter();
            let bs = unwrap_byte_string(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let i = unwrap_integer(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let idx = bigint_to_usize_or_failure(&i, id, "readBit index")?;
            if idx >= bs.len().saturating_mul(8) {
                return Err(builtin_failure(id, "readBit: index out of range"));
            }
            // CIP-0123 §"Indexing": bit i is bit (i % 8) of byte (i / 8),
            // with byte 0 the least-significant. We use the "natural"
            // little-endian byte order matching the spec.
            let byte = bs[idx / 8];
            let bit = (byte >> (idx % 8)) & 1;
            Ok(Value::Const(Constant::Bool(bit != 0)))
        }
        ReplicateByte => {
            // (count: Integer) (byte: Integer) -> ByteString
            let mut it = args.into_iter();
            let count = unwrap_integer(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let byte = unwrap_integer(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let count_u = bigint_to_usize_or_failure(&count, id, "replicateByte count")?;
            const MAX_REPLICATE: usize = 8192;
            if count_u > MAX_REPLICATE {
                return Err(builtin_failure(
                    id,
                    "replicateByte: count exceeds 8192-byte cap",
                ));
            }
            let b = bigint_to_u8(&byte, id, "replicateByte byte must be 0..=255")?;
            Ok(Value::Const(Constant::ByteString(vec![b; count_u])))
        }
        CountSetBits => {
            let v = take_one(args, id)?;
            let bs = unwrap_byte_string(v, id)?;
            let count: u32 = bs.iter().map(|b| b.count_ones()).sum();
            Ok(Value::Const(Constant::Integer(BigInt::from(count))))
        }
        FindFirstSetBit => {
            // Returns the index of the first 1 bit, or -1 if none.
            // CIP-0123 §"Indexing": bit 0 is LSB of byte 0.
            let v = take_one(args, id)?;
            let bs = unwrap_byte_string(v, id)?;
            for (byte_idx, &byte) in bs.iter().enumerate() {
                if byte != 0 {
                    let bit_in_byte = byte.trailing_zeros() as usize;
                    let global = byte_idx * 8 + bit_in_byte;
                    return Ok(Value::Const(Constant::Integer(BigInt::from(global))));
                }
            }
            Ok(Value::Const(Constant::Integer(BigInt::from(-1))))
        }

        // ── ExpModInteger (V3) ────────────────────────────────────────
        ExpModInteger => {
            // (base: Integer) (exp: Integer) (modulus: Integer) -> Integer
            // Haskell: expModInteger b e m = b ^ e mod m, requiring
            // m > 0 and e >= 0.
            let mut it = args.into_iter();
            let base = unwrap_integer(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let exp = unwrap_integer(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let modulus = unwrap_integer(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            if modulus.sign() != num_bigint::Sign::Plus {
                return Err(builtin_failure(
                    id,
                    "expModInteger: modulus must be positive",
                ));
            }
            if exp.sign() == num_bigint::Sign::Minus {
                return Err(builtin_failure(
                    id,
                    "expModInteger: exponent must be non-negative",
                ));
            }
            Ok(Value::Const(Constant::Integer(base.modpow(&exp, &modulus))))
        }

        // ── CIP-0123 bitwise (continued) ──────────────────────────────
        ShiftByteString => {
            // (bs: ByteString) (shift: Integer) -> ByteString
            // Positive shift = left (toward high-index bytes); negative
            // = right. Output is the same length as input; bits
            // shifted out are discarded; new bits are 0. Bit ordering
            // follows ReadBit (LSB of byte 0 = bit 0).
            let mut it = args.into_iter();
            let bs = unwrap_byte_string(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let shift_int =
                unwrap_integer(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let len_bits = bs.len().saturating_mul(8);
            if len_bits == 0 {
                return Ok(Value::Const(Constant::ByteString(bs)));
            }
            // The reference reads `shift` as Haskell `Int` (= Int64);
            // values outside that range are an evaluation failure
            // before the shift even runs (#603).
            let shift_i64 = bigint_to_i64_or_failure(&shift_int, id, "shift amount")?;
            let shift = BigInt::from(shift_i64);
            let abs_shift_int = if shift.sign() == num_bigint::Sign::Minus {
                -shift.clone()
            } else {
                shift.clone()
            };
            let abs_shift_u = bigint_to_usize_or_failure(&abs_shift_int, id, "shift amount")?;
            if abs_shift_u >= len_bits {
                return Ok(Value::Const(Constant::ByteString(vec![0u8; bs.len()])));
            }
            let mut out = vec![0u8; bs.len()];
            for target_idx in 0..len_bits {
                // For a left-shift of N: target bit T comes from source bit (T - N).
                // For a right-shift of N: target bit T comes from source bit (T + N).
                let src_signed = if shift.sign() == num_bigint::Sign::Minus {
                    // Right shift: src = target + abs_shift
                    Some(target_idx + abs_shift_u)
                } else {
                    // Left shift: src = target - abs_shift, only if ≥ 0
                    target_idx.checked_sub(abs_shift_u)
                };
                let src = match src_signed {
                    Some(s) if s < len_bits => s,
                    _ => continue, // out-of-range source → 0 (already initialised)
                };
                let src_bit = (bs[src / 8] >> (src % 8)) & 1;
                if src_bit != 0 {
                    out[target_idx / 8] |= 1u8 << (target_idx % 8);
                }
            }
            Ok(Value::Const(Constant::ByteString(out)))
        }
        RotateByteString => {
            // (bs: ByteString) (rotate: Integer) -> ByteString
            // Same direction convention as ShiftByteString; bits that
            // would be lost wrap around.
            let mut it = args.into_iter();
            let bs = unwrap_byte_string(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let rot_int = unwrap_integer(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let len_bits = bs.len().saturating_mul(8);
            if len_bits == 0 {
                return Ok(Value::Const(Constant::ByteString(bs)));
            }
            // The reference reads `rotate` as Haskell `Int` (= Int64);
            // see ShiftByteString above (#603).
            let rot_i64 = bigint_to_i64_or_failure(&rot_int, id, "rotate amount")?;
            // Normalise rotate amount mod len_bits.
            let len_bi = BigInt::from(len_bits);
            let r = ((BigInt::from(rot_i64) % &len_bi) + &len_bi) % &len_bi;
            let r_u = bigint_to_usize_or_failure(&r, id, "rotate amount")?;
            let mut out = vec![0u8; bs.len()];
            for target_idx in 0..len_bits {
                let src = (target_idx + len_bits - r_u) % len_bits;
                let src_bit = (bs[src / 8] >> (src % 8)) & 1;
                if src_bit != 0 {
                    out[target_idx / 8] |= 1u8 << (target_idx % 8);
                }
            }
            Ok(Value::Const(Constant::ByteString(out)))
        }
        WriteBits => {
            // (bs: ByteString) (indices: ProtoList Integer) (value: Bool) -> ByteString
            // For each index in `indices`, set bit `index` to `value`.
            // Out-of-range index → BuiltinFailure.
            let mut it = args.into_iter();
            let bs = unwrap_byte_string(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let indices_val = it.next().ok_or_else(|| builtin_arity_mismatch(id))?;
            let value = unwrap_bool(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let (_, idx_elems) = unwrap_proto_list(indices_val, id)?;
            let len_bits = bs.len().saturating_mul(8);
            let mut out = bs;
            for c in idx_elems {
                let i = match c {
                    Constant::Integer(i) => i,
                    _ => {
                        return Err(UplcError::BuiltinTypeError {
                            builtin: id.name(),
                            reason: "writeBits indices list must contain Integers".into(),
                        });
                    }
                };
                let idx = bigint_to_usize_or_failure(&i, id, "writeBits index")?;
                if idx >= len_bits {
                    return Err(builtin_failure(id, "writeBits: index out of range"));
                }
                let byte = &mut out[idx / 8];
                let mask = 1u8 << (idx % 8);
                if value {
                    *byte |= mask;
                } else {
                    *byte &= !mask;
                }
            }
            Ok(Value::Const(Constant::ByteString(out)))
        }

        // ── secp256k1 sig verify (V2; CIP-49) ─────────────────────────
        VerifyEcdsaSecp256k1Signature => {
            // (pk: ByteString(33)) (msg_hash: ByteString(32)) (sig: ByteString(64)) -> Bool
            //
            // Per Cardano's CIP-49 §"ECDSA signatures":
            //   - pk is a *compressed* SEC-1 public key (33 bytes;
            //     leading 0x02/0x03 tag byte).
            //   - msg_hash is a pre-hashed 32-byte digest (Cardano
            //     does NOT hash the message; the script must hash it).
            //   - sig is a 64-byte (r || s) fixed-size pair, low-S
            //     normalised. Mainnet ECDSA verification REJECTS any
            //     signature with s > n/2.
            //
            // Length mismatches and parse failures are BuiltinFailure;
            // a structurally well-formed but invalid signature returns
            // False.
            let mut it = args.into_iter();
            let pk = unwrap_byte_string(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let msg = unwrap_byte_string(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let sig = unwrap_byte_string(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            if pk.len() != 33 {
                return Err(builtin_failure(
                    id,
                    &format!("ECDSA public key must be 33 bytes, got {}", pk.len()),
                ));
            }
            if msg.len() != 32 {
                return Err(builtin_failure(
                    id,
                    &format!("ECDSA message hash must be 32 bytes, got {}", msg.len()),
                ));
            }
            if sig.len() != 64 {
                return Err(builtin_failure(
                    id,
                    &format!("ECDSA signature must be 64 bytes, got {}", sig.len()),
                ));
            }
            use k256::ecdsa::{signature::hazmat::PrehashVerifier, Signature, VerifyingKey};
            use k256::elliptic_curve::scalar::IsHigh;
            // PK parse failure (uncompressed tag, off-curve x-coord,
            // wrong leading byte, etc.) is an evaluation failure per
            // the Haskell reference — NOT a `False` result (#603).
            let vk = VerifyingKey::from_sec1_bytes(&pk)
                .map_err(|e| builtin_failure(id, &format!("ECDSA public key parse failed: {e}")))?;
            let sig_arr: [u8; 64] = match sig.as_slice().try_into() {
                Ok(a) => a,
                Err(_) => {
                    return Err(UplcError::Internal(
                        "ECDSA sig length check failed after success".into(),
                    ));
                }
            };
            // Same: signature byte parse failures are an evaluation
            // failure, not `False`.
            let sig = Signature::from_bytes(&sig_arr.into())
                .map_err(|e| builtin_failure(id, &format!("ECDSA signature parse failed: {e}")))?;
            // Low-S enforcement: reject high-S signatures explicitly
            // (k256's `from_bytes` parses them but `verify_prehash`
            // accepts either form unless we route through
            // `Signature::normalize_s`).
            if sig.s().is_high().into() {
                return Ok(Value::Const(Constant::Bool(false)));
            }
            Ok(Value::Const(Constant::Bool(
                vk.verify_prehash(&msg, &sig).is_ok(),
            )))
        }
        VerifySchnorrSecp256k1Signature => {
            // (pk: ByteString(32)) (msg: ByteString) (sig: ByteString(64)) -> Bool
            //
            // Per CIP-49 §"Schnorr signatures": BIP-340.
            //   - pk is a 32-byte x-only public key.
            //   - msg is arbitrary-length (BIP-340 does internal
            //     tagged-hash construction).
            //   - sig is 64 bytes (R.x || s).
            let mut it = args.into_iter();
            let pk = unwrap_byte_string(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let msg = unwrap_byte_string(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let sig = unwrap_byte_string(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            if pk.len() != 32 {
                return Err(builtin_failure(
                    id,
                    &format!("Schnorr public key must be 32 bytes, got {}", pk.len()),
                ));
            }
            if sig.len() != 64 {
                return Err(builtin_failure(
                    id,
                    &format!("Schnorr signature must be 64 bytes, got {}", sig.len()),
                ));
            }
            use k256::schnorr::{signature::Verifier, Signature, VerifyingKey};
            let pk_arr: [u8; 32] = match pk.as_slice().try_into() {
                Ok(a) => a,
                Err(_) => {
                    return Err(UplcError::Internal(
                        "Schnorr pk length check failed after success".into(),
                    ));
                }
            };
            // x-only pubkey parse failure (e.g. x-coord not on the
            // curve, BIP-340 §"Public Key Conversion") is an
            // evaluation failure per the Haskell reference, NOT a
            // `False` result (#603).
            let vk = VerifyingKey::from_bytes(&pk_arr).map_err(|e| {
                builtin_failure(id, &format!("Schnorr public key parse failed: {e}"))
            })?;
            let sig_arr: [u8; 64] = match sig.as_slice().try_into() {
                Ok(a) => a,
                Err(_) => {
                    return Err(UplcError::Internal(
                        "Schnorr sig length check failed after success".into(),
                    ));
                }
            };
            // BIP-340 §"Verification": a structurally well-formed 64-byte
            // signature whose R-x component lifts to no curve point (or
            // whose s is out of range) MUST return False, not raise an
            // evaluation failure.  k256's `Signature::try_from` is
            // stricter than BIP-340 here — it rejects R-x = 0 and other
            // degenerate forms at parse — so we treat any such parse
            // failure as a verification failure (False) rather than
            // surfacing it as `BuiltinFailure`.  Public-key parse
            // failures stay as `BuiltinFailure` (those are genuinely
            // malformed inputs that should not be silently accepted).
            let sig = match Signature::try_from(&sig_arr[..]) {
                Ok(s) => s,
                Err(_) => return Ok(Value::Const(Constant::Bool(false))),
            };
            Ok(Value::Const(Constant::Bool(vk.verify(&msg, &sig).is_ok())))
        }

        // ── BLS12-381 (V3; CIP-0381) ──────────────────────────────────
        Bls12_381_G1_Add
        | Bls12_381_G1_Neg
        | Bls12_381_G1_ScalarMul
        | Bls12_381_G1_Equal
        | Bls12_381_G1_HashToGroup
        | Bls12_381_G1_Compress
        | Bls12_381_G1_Uncompress
        | Bls12_381_G2_Add
        | Bls12_381_G2_Neg
        | Bls12_381_G2_ScalarMul
        | Bls12_381_G2_Equal
        | Bls12_381_G2_HashToGroup
        | Bls12_381_G2_Compress
        | Bls12_381_G2_Uncompress
        | Bls12_381_MillerLoop
        | Bls12_381_MulMlResult
        | Bls12_381_FinalVerify => crate::builtin::bls::denote_bls(id, args),
    }
}

/// Helper for single-argument-ByteString → ByteString hash builtins.
fn hash_one<F>(args: Vec<Value>, id: BuiltinId, hash: F) -> Result<Value, UplcError>
where
    F: FnOnce(&[u8]) -> Vec<u8>,
{
    let mut it = args.into_iter();
    let input = unwrap_byte_string(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
    Ok(Value::Const(Constant::ByteString(hash(&input))))
}

fn take_one(args: Vec<Value>, id: BuiltinId) -> Result<Value, UplcError> {
    let mut it = args.into_iter();
    it.next().ok_or_else(|| builtin_arity_mismatch(id))
}

fn unwrap_proto_list(
    v: Value,
    id: BuiltinId,
) -> Result<(crate::term::TypeTag, Vec<Constant>), UplcError> {
    match v {
        Value::Const(Constant::ProtoList {
            elem_type,
            elements,
        }) => Ok((elem_type, elements)),
        other => Err(UplcError::BuiltinTypeError {
            builtin: id.name(),
            reason: format!(
                "expected ProtoList, got {:?}",
                std::mem::discriminant(&other)
            ),
        }),
    }
}

fn unwrap_proto_pair(v: Value, id: BuiltinId) -> Result<(Box<Constant>, Box<Constant>), UplcError> {
    match v {
        Value::Const(Constant::ProtoPair { a, b, .. }) => Ok((a, b)),
        other => Err(UplcError::BuiltinTypeError {
            builtin: id.name(),
            reason: format!(
                "expected ProtoPair, got {:?}",
                std::mem::discriminant(&other)
            ),
        }),
    }
}

fn unwrap_bool(v: Value, id: BuiltinId) -> Result<bool, UplcError> {
    match v {
        Value::Const(Constant::Bool(b)) => Ok(b),
        other => Err(UplcError::BuiltinTypeError {
            builtin: id.name(),
            reason: format!("expected Bool, got {:?}", std::mem::discriminant(&other)),
        }),
    }
}

fn bigint_to_usize_or_failure(i: &BigInt, id: BuiltinId, what: &str) -> Result<usize, UplcError> {
    use num_bigint::Sign;
    if i.sign() == Sign::Minus {
        return Err(builtin_failure(id, &format!("{what}: negative")));
    }
    let digits = i.iter_u64_digits().collect::<Vec<_>>();
    if digits.is_empty() {
        return Ok(0);
    }
    if digits.len() > 1 {
        return Err(builtin_failure(id, &format!("{what}: value exceeds usize")));
    }
    usize::try_from(digits[0])
        .map_err(|_| builtin_failure(id, &format!("{what}: value exceeds usize on this platform")))
}

/// Decode a `BigInt` into Haskell's `Int` (`i64`).  Used for builtin
/// inputs that the reference reads as `Int` — `shiftByteString` and
/// `rotateByteString` shift/rotate amounts, for example.  The Haskell
/// `readKnown` raises an evaluation failure when the integer overflows
/// `Int64`; mirror that exactly so cardano-node's reject semantics
/// match dugite's (#603).
fn bigint_to_i64_or_failure(i: &BigInt, id: BuiltinId, what: &str) -> Result<i64, UplcError> {
    use num_traits::ToPrimitive;
    i.to_i64()
        .ok_or_else(|| builtin_failure(id, &format!("{what}: value does not fit in Int64")))
}

/// Bitwise binary op on two byte strings of equal length.
/// `pad` is the byte to use when extending the shorter input to the
/// longer length. Per CIP-0123: padding semantics are part of the
/// builtin (`andByteString` pads with 0xFF for max-len, `orByteString`
/// and `xorByteString` pad with 0x00). Our current implementation
/// requires equal-length inputs and rejects mismatched lengths as a
/// `BuiltinFailure`; the padding-mode variant arrives with the
/// `padded`-flavour builtins in a follow-on commit.
fn bitwise_byte_string<F>(
    args: Vec<Value>,
    id: BuiltinId,
    op: F,
    _pad: u8,
) -> Result<Value, UplcError>
where
    F: Fn(u8, u8) -> u8,
{
    // The current Haskell signature is (padding: Bool, a: ByteString,
    // b: ByteString) -> ByteString. The first arg controls padding
    // semantics (True = pad with 0xFF, False = pad with 0x00 — or
    // some such; CIP-0123 spec details vary). For now we accept the
    // padding flag but treat both inputs as equal-length (rejecting
    // mismatched lengths).
    let mut it = args.into_iter();
    let _padding = unwrap_bool(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
    let a = unwrap_byte_string(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
    let b = unwrap_byte_string(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
    if a.len() != b.len() {
        return Err(builtin_failure(
            id,
            "bitwise: mismatched byte-string lengths (padding modes wired in follow-on)",
        ));
    }
    let out: Vec<u8> = a.iter().zip(b.iter()).map(|(x, y)| op(*x, *y)).collect();
    Ok(Value::Const(Constant::ByteString(out)))
}

fn unwrap_data(v: Value, id: BuiltinId) -> Result<crate::data::Data, UplcError> {
    match v {
        Value::Const(Constant::Data(d)) => Ok(d),
        other => Err(UplcError::BuiltinTypeError {
            builtin: id.name(),
            reason: format!("expected Data, got {:?}", std::mem::discriminant(&other)),
        }),
    }
}

fn take_two_byte_strings(args: Vec<Value>, id: BuiltinId) -> Result<(Vec<u8>, Vec<u8>), UplcError> {
    let mut it = args.into_iter();
    let a = unwrap_byte_string(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
    let b = unwrap_byte_string(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
    Ok((a, b))
}

fn take_two_strings(args: Vec<Value>, id: BuiltinId) -> Result<(String, String), UplcError> {
    let mut it = args.into_iter();
    let a = unwrap_string(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
    let b = unwrap_string(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
    Ok((a, b))
}

fn unwrap_byte_string(v: Value, id: BuiltinId) -> Result<Vec<u8>, UplcError> {
    match v {
        Value::Const(Constant::ByteString(b)) => Ok(b),
        other => Err(UplcError::BuiltinTypeError {
            builtin: id.name(),
            reason: format!(
                "expected ByteString, got {:?}",
                std::mem::discriminant(&other)
            ),
        }),
    }
}

fn unwrap_string(v: Value, id: BuiltinId) -> Result<String, UplcError> {
    match v {
        Value::Const(Constant::String(s)) => Ok(s),
        other => Err(UplcError::BuiltinTypeError {
            builtin: id.name(),
            reason: format!("expected String, got {:?}", std::mem::discriminant(&other)),
        }),
    }
}

fn bigint_to_u8(i: &BigInt, id: BuiltinId, why: &str) -> Result<u8, UplcError> {
    use num_bigint::Sign;
    if i.sign() == Sign::Minus {
        return Err(builtin_failure(id, why));
    }
    let digits = i.iter_u64_digits().collect::<Vec<_>>();
    if digits.is_empty() {
        return Ok(0);
    }
    if digits.len() > 1 || digits[0] > 255 {
        return Err(builtin_failure(id, why));
    }
    Ok(digits[0] as u8)
}

fn bigint_to_usize_clamped(i: &BigInt) -> usize {
    use num_bigint::Sign;
    if i.sign() == Sign::Minus {
        return 0;
    }
    let digits = i.iter_u64_digits().collect::<Vec<_>>();
    if digits.is_empty() {
        return 0;
    }
    if digits.len() > 1 {
        return usize::MAX;
    }
    usize::try_from(digits[0]).unwrap_or(usize::MAX)
}

fn int_binop<F>(args: Vec<Value>, id: BuiltinId, op: F) -> Result<Value, UplcError>
where
    F: FnOnce(BigInt, BigInt) -> Result<BigInt, UplcError>,
{
    let (a, b) = take_two_integers(args, id)?;
    let result = op(a, b)?;
    Ok(Value::Const(Constant::Integer(result)))
}

fn int_cmp<F>(args: Vec<Value>, id: BuiltinId, op: F) -> Result<Value, UplcError>
where
    F: FnOnce(BigInt, BigInt) -> bool,
{
    let (a, b) = take_two_integers(args, id)?;
    Ok(Value::Const(Constant::Bool(op(a, b))))
}

fn take_two_integers(args: Vec<Value>, id: BuiltinId) -> Result<(BigInt, BigInt), UplcError> {
    let mut it = args.into_iter();
    let a = unwrap_integer(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
    let b = unwrap_integer(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
    Ok((a, b))
}

fn unwrap_integer(v: Value, id: BuiltinId) -> Result<BigInt, UplcError> {
    match v {
        Value::Const(Constant::Integer(i)) => Ok(i),
        other => Err(UplcError::BuiltinTypeError {
            builtin: id.name(),
            reason: format!("expected Integer, got {:?}", std::mem::discriminant(&other)),
        }),
    }
}

fn builtin_arity_mismatch(id: BuiltinId) -> UplcError {
    UplcError::Internal(format!(
        "denotation for {} called with wrong argument count",
        id.name()
    ))
}

fn builtin_failure(id: BuiltinId, reason: &str) -> UplcError {
    UplcError::BuiltinFailure {
        builtin: id.name(),
        reason: reason.into(),
    }
}

/// Haskell `div`: rounds toward negative infinity (floor division).
fn divide_haskell_div(a: &BigInt, b: &BigInt) -> BigInt {
    let (q, r) = (a / b, a % b);
    // If signs differ and the remainder is non-zero, adjust toward
    // -infinity by subtracting 1.
    if r.sign() != num_bigint::Sign::NoSign
        && ((a.sign() == num_bigint::Sign::Minus) != (b.sign() == num_bigint::Sign::Minus))
    {
        q - 1
    } else {
        q
    }
}

/// Haskell `mod`: result has the sign of the divisor.
fn mod_haskell_mod(a: &BigInt, b: &BigInt) -> BigInt {
    let r = a % b;
    if r.sign() != num_bigint::Sign::NoSign
        && ((a.sign() == num_bigint::Sign::Minus) != (b.sign() == num_bigint::Sign::Minus))
    {
        r + b
    } else {
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn int(n: i64) -> Value {
        Value::Const(Constant::Integer(BigInt::from(n)))
    }
    fn b(v: bool) -> Value {
        Value::Const(Constant::Bool(v))
    }

    fn run(id: BuiltinId, args: Vec<Value>) -> Result<Value, UplcError> {
        denote(id, args)
    }

    // ── Integer arithmetic ─────────────────────────────────────────

    #[test]
    fn add_integer() {
        assert_eq!(
            run(BuiltinId::AddInteger, vec![int(1), int(2)]).unwrap(),
            int(3)
        );
        assert_eq!(
            run(BuiltinId::AddInteger, vec![int(-5), int(10)]).unwrap(),
            int(5)
        );
    }

    #[test]
    fn subtract_integer() {
        assert_eq!(
            run(BuiltinId::SubtractInteger, vec![int(5), int(3)]).unwrap(),
            int(2)
        );
    }

    #[test]
    fn multiply_integer() {
        assert_eq!(
            run(BuiltinId::MultiplyInteger, vec![int(7), int(6)]).unwrap(),
            int(42)
        );
    }

    #[test]
    fn divide_integer_haskell_div_floor_toward_neg_inf() {
        // 7 `div` 2 = 3
        assert_eq!(
            run(BuiltinId::DivideInteger, vec![int(7), int(2)]).unwrap(),
            int(3)
        );
        // (-7) `div` 2 = -4  (Haskell `div` floors toward -infinity)
        assert_eq!(
            run(BuiltinId::DivideInteger, vec![int(-7), int(2)]).unwrap(),
            int(-4)
        );
        // 7 `div` (-2) = -4
        assert_eq!(
            run(BuiltinId::DivideInteger, vec![int(7), int(-2)]).unwrap(),
            int(-4)
        );
        // (-7) `div` (-2) = 3
        assert_eq!(
            run(BuiltinId::DivideInteger, vec![int(-7), int(-2)]).unwrap(),
            int(3)
        );
    }

    #[test]
    fn quotient_integer_truncates_toward_zero() {
        // 7 `quot` 2 = 3
        assert_eq!(
            run(BuiltinId::QuotientInteger, vec![int(7), int(2)]).unwrap(),
            int(3)
        );
        // (-7) `quot` 2 = -3   (truncates, not floors)
        assert_eq!(
            run(BuiltinId::QuotientInteger, vec![int(-7), int(2)]).unwrap(),
            int(-3)
        );
    }

    #[test]
    fn remainder_integer_matches_quotient_sign() {
        // 7 `rem` 2 = 1; (-7) `rem` 2 = -1
        assert_eq!(
            run(BuiltinId::RemainderInteger, vec![int(7), int(2)]).unwrap(),
            int(1)
        );
        assert_eq!(
            run(BuiltinId::RemainderInteger, vec![int(-7), int(2)]).unwrap(),
            int(-1)
        );
    }

    #[test]
    fn mod_integer_takes_divisor_sign() {
        // 7 `mod` 2 = 1; (-7) `mod` 2 = 1; 7 `mod` (-2) = -1; (-7) `mod` (-2) = -1.
        assert_eq!(
            run(BuiltinId::ModInteger, vec![int(7), int(2)]).unwrap(),
            int(1)
        );
        assert_eq!(
            run(BuiltinId::ModInteger, vec![int(-7), int(2)]).unwrap(),
            int(1)
        );
        assert_eq!(
            run(BuiltinId::ModInteger, vec![int(7), int(-2)]).unwrap(),
            int(-1)
        );
        assert_eq!(
            run(BuiltinId::ModInteger, vec![int(-7), int(-2)]).unwrap(),
            int(-1)
        );
    }

    #[test]
    fn divide_by_zero_is_builtin_failure() {
        assert!(matches!(
            run(BuiltinId::DivideInteger, vec![int(7), int(0)]),
            Err(UplcError::BuiltinFailure { .. })
        ));
        assert!(matches!(
            run(BuiltinId::QuotientInteger, vec![int(7), int(0)]),
            Err(UplcError::BuiltinFailure { .. })
        ));
        assert!(matches!(
            run(BuiltinId::RemainderInteger, vec![int(7), int(0)]),
            Err(UplcError::BuiltinFailure { .. })
        ));
        assert!(matches!(
            run(BuiltinId::ModInteger, vec![int(7), int(0)]),
            Err(UplcError::BuiltinFailure { .. })
        ));
    }

    #[test]
    fn equals_integer() {
        assert_eq!(
            run(BuiltinId::EqualsInteger, vec![int(5), int(5)]).unwrap(),
            b(true)
        );
        assert_eq!(
            run(BuiltinId::EqualsInteger, vec![int(5), int(6)]).unwrap(),
            b(false)
        );
    }

    #[test]
    fn less_than_integer() {
        assert_eq!(
            run(BuiltinId::LessThanInteger, vec![int(5), int(6)]).unwrap(),
            b(true)
        );
        assert_eq!(
            run(BuiltinId::LessThanInteger, vec![int(5), int(5)]).unwrap(),
            b(false)
        );
    }

    #[test]
    fn less_than_equals_integer() {
        assert_eq!(
            run(BuiltinId::LessThanEqualsInteger, vec![int(5), int(5)]).unwrap(),
            b(true)
        );
        assert_eq!(
            run(BuiltinId::LessThanEqualsInteger, vec![int(6), int(5)]).unwrap(),
            b(false)
        );
    }

    // ── Polymorphic helpers ────────────────────────────────────────

    #[test]
    fn if_then_else_picks_branch() {
        assert_eq!(
            run(BuiltinId::IfThenElse, vec![b(true), int(1), int(2)]).unwrap(),
            int(1)
        );
        assert_eq!(
            run(BuiltinId::IfThenElse, vec![b(false), int(1), int(2)]).unwrap(),
            int(2)
        );
    }

    #[test]
    fn if_then_else_non_bool_errors() {
        let err = run(BuiltinId::IfThenElse, vec![int(0), int(1), int(2)]).unwrap_err();
        assert!(matches!(err, UplcError::BuiltinTypeError { .. }));
    }

    #[test]
    fn choose_unit_returns_second_arg() {
        assert_eq!(
            run(
                BuiltinId::ChooseUnit,
                vec![Value::Const(Constant::Unit), int(7)]
            )
            .unwrap(),
            int(7)
        );
    }

    #[test]
    fn choose_unit_non_unit_errors() {
        let err = run(BuiltinId::ChooseUnit, vec![int(0), int(7)]).unwrap_err();
        assert!(matches!(err, UplcError::BuiltinTypeError { .. }));
    }

    #[test]
    fn trace_returns_second_arg() {
        assert_eq!(
            run(
                BuiltinId::Trace,
                vec![Value::Const(Constant::String("hello".into())), int(99)]
            )
            .unwrap(),
            int(99)
        );
    }

    #[test]
    fn unwired_builtin_returns_internal() {
        // VerifyEcdsaSecp256k1Signature not wired yet.
        let err = run(BuiltinId::VerifyEcdsaSecp256k1Signature, vec![]).unwrap_err();
        assert!(matches!(err, UplcError::Internal(_)));
    }

    // ── List / Pair / Data builtins ─────────────────────────────────

    use crate::data::Data;
    use crate::term::TypeTag;

    fn data_i(n: i64) -> Constant {
        Constant::Data(Data::I(BigInt::from(n)))
    }
    fn list_of_data(items: Vec<Data>) -> Value {
        Value::Const(Constant::ProtoList {
            elem_type: TypeTag::Data,
            elements: items.into_iter().map(Constant::Data).collect(),
        })
    }
    fn data_val(d: Data) -> Value {
        Value::Const(Constant::Data(d))
    }

    #[test]
    fn head_list_returns_first() {
        let l = list_of_data(vec![Data::I(BigInt::from(1)), Data::I(BigInt::from(2))]);
        assert_eq!(
            run(BuiltinId::HeadList, vec![l]).unwrap(),
            Value::Const(data_i(1))
        );
    }

    #[test]
    fn head_list_on_empty_fails() {
        let l = list_of_data(vec![]);
        assert!(matches!(
            run(BuiltinId::HeadList, vec![l]),
            Err(UplcError::BuiltinFailure { .. })
        ));
    }

    #[test]
    fn tail_list_drops_first() {
        let l = list_of_data(vec![Data::I(BigInt::from(1)), Data::I(BigInt::from(2))]);
        let v = run(BuiltinId::TailList, vec![l]).unwrap();
        assert_eq!(v, list_of_data(vec![Data::I(BigInt::from(2))]));
    }

    #[test]
    fn null_list() {
        assert_eq!(
            run(BuiltinId::NullList, vec![list_of_data(vec![])]).unwrap(),
            b(true)
        );
        assert_eq!(
            run(
                BuiltinId::NullList,
                vec![list_of_data(vec![Data::I(BigInt::from(1))])]
            )
            .unwrap(),
            b(false)
        );
    }

    #[test]
    fn choose_list_picks_branch() {
        let nil_l = list_of_data(vec![]);
        assert_eq!(
            run(BuiltinId::ChooseList, vec![nil_l.clone(), int(0), int(1)]).unwrap(),
            int(0)
        );
        let cons_l = list_of_data(vec![Data::I(BigInt::from(7))]);
        assert_eq!(
            run(BuiltinId::ChooseList, vec![cons_l, int(0), int(1)]).unwrap(),
            int(1)
        );
    }

    #[test]
    fn mk_cons_prepends() {
        let l = list_of_data(vec![Data::I(BigInt::from(2))]);
        let head = Value::Const(data_i(1));
        let v = run(BuiltinId::MkCons, vec![head, l]).unwrap();
        assert_eq!(
            v,
            list_of_data(vec![Data::I(BigInt::from(1)), Data::I(BigInt::from(2))])
        );
    }

    #[test]
    fn mk_nil_data_empty() {
        let v = run(BuiltinId::MkNilData, vec![Value::Const(Constant::Unit)]).unwrap();
        assert_eq!(v, list_of_data(vec![]));
    }

    #[test]
    fn fst_snd_pair() {
        let p = Value::Const(Constant::ProtoPair {
            a_type: TypeTag::Integer,
            b_type: TypeTag::ByteString,
            a: Box::new(Constant::Integer(BigInt::from(7))),
            b: Box::new(Constant::ByteString(vec![0xab])),
        });
        assert_eq!(run(BuiltinId::FstPair, vec![p.clone()]).unwrap(), int(7));
        assert_eq!(run(BuiltinId::SndPair, vec![p]).unwrap(), bs(&[0xab]));
    }

    #[test]
    fn data_constructors_and_destructors_round_trip() {
        // IData / UnIData
        let v = run(BuiltinId::IData, vec![int(42)]).unwrap();
        assert_eq!(v, data_val(Data::I(BigInt::from(42))));
        let v2 = run(BuiltinId::UnIData, vec![v]).unwrap();
        assert_eq!(v2, int(42));

        // BData / UnBData
        let v = run(BuiltinId::BData, vec![bs(b"abc")]).unwrap();
        assert_eq!(v, data_val(Data::B(b"abc".to_vec())));
        let v2 = run(BuiltinId::UnBData, vec![v]).unwrap();
        assert_eq!(v2, bs(b"abc"));
    }

    #[test]
    fn constr_data_packs_fields() {
        let fields = list_of_data(vec![Data::I(BigInt::from(1)), Data::I(BigInt::from(2))]);
        let v = run(BuiltinId::ConstrData, vec![int(3), fields]).unwrap();
        assert_eq!(
            v,
            data_val(Data::Constr(
                3,
                vec![Data::I(BigInt::from(1)), Data::I(BigInt::from(2))]
            ))
        );
    }

    #[test]
    fn un_constr_data_unpacks() {
        let d = data_val(Data::Constr(3, vec![Data::I(BigInt::from(1))]));
        let v = run(BuiltinId::UnConstrData, vec![d]).unwrap();
        // result is Pair(Integer, List Data)
        if let Value::Const(Constant::ProtoPair { a, b, .. }) = v {
            assert_eq!(*a, Constant::Integer(BigInt::from(3)));
            if let Constant::ProtoList { elements, .. } = *b {
                assert_eq!(elements.len(), 1);
                assert_eq!(elements[0], Constant::Data(Data::I(BigInt::from(1))));
            } else {
                panic!("expected ProtoList for second pair element");
            }
        } else {
            panic!("expected ProtoPair");
        }
    }

    #[test]
    fn choose_data_picks_by_constructor() {
        let d_constr = data_val(Data::Constr(0, vec![]));
        assert_eq!(
            run(
                BuiltinId::ChooseData,
                vec![d_constr, int(1), int(2), int(3), int(4), int(5)]
            )
            .unwrap(),
            int(1)
        );
        let d_i = data_val(Data::I(BigInt::from(0)));
        assert_eq!(
            run(
                BuiltinId::ChooseData,
                vec![d_i, int(1), int(2), int(3), int(4), int(5)]
            )
            .unwrap(),
            int(4)
        );
    }

    #[test]
    fn equals_data() {
        let a = data_val(Data::I(BigInt::from(5)));
        let b1 = data_val(Data::I(BigInt::from(5)));
        let b2 = data_val(Data::I(BigInt::from(6)));
        assert_eq!(
            run(BuiltinId::EqualsData, vec![a.clone(), b1]).unwrap(),
            b(true)
        );
        assert_eq!(run(BuiltinId::EqualsData, vec![a, b2]).unwrap(), b(false));
    }

    #[test]
    fn serialise_data_round_trips_via_cbor() {
        let d = data_val(Data::Constr(0, vec![Data::I(BigInt::from(42))]));
        let v = run(BuiltinId::SerialiseData, vec![d]).unwrap();
        // The result is the CBOR encoding of Constr 0 [I 42]
        if let Value::Const(Constant::ByteString(bytes)) = v {
            // CBOR: tag 121 + array(1) + 42 (which is 0x18 0x2a)
            assert_eq!(bytes[0..2], [0xd8, 0x79]); // tag 121 (0x79 = 121)
        } else {
            panic!("expected ByteString");
        }
    }

    // ── Ed25519 ─────────────────────────────────────────────────────

    #[test]
    fn verify_ed25519_known_vector() {
        // RFC 8032 §7.1 test vector 1:
        //   secret: 9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60
        //   public: d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a
        //   message: (empty)
        //   signature: e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b
        let pk = hex::decode("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a")
            .unwrap();
        let msg: Vec<u8> = vec![];
        let sig = hex::decode("e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b").unwrap();
        let v = run(
            BuiltinId::VerifyEd25519Signature,
            vec![bs(&pk), bs(&msg), bs(&sig)],
        )
        .unwrap();
        assert_eq!(v, b(true));
    }

    #[test]
    fn verify_ed25519_wrong_sig_returns_false() {
        let pk = hex::decode("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a")
            .unwrap();
        let msg: Vec<u8> = vec![];
        let bad_sig = vec![0u8; 64];
        let v = run(
            BuiltinId::VerifyEd25519Signature,
            vec![bs(&pk), bs(&msg), bs(&bad_sig)],
        )
        .unwrap();
        assert_eq!(v, b(false));
    }

    #[test]
    fn verify_ed25519_wrong_length_fails() {
        // 31-byte pk
        assert!(matches!(
            run(
                BuiltinId::VerifyEd25519Signature,
                vec![bs(&[0u8; 31]), bs(b""), bs(&[0u8; 64])]
            ),
            Err(UplcError::BuiltinFailure { .. })
        ));
        // 63-byte sig
        assert!(matches!(
            run(
                BuiltinId::VerifyEd25519Signature,
                vec![bs(&[0u8; 32]), bs(b""), bs(&[0u8; 63])]
            ),
            Err(UplcError::BuiltinFailure { .. })
        ));
    }

    // ── Hash functions ─────────────────────────────────────────────

    #[test]
    fn sha2_256_known_vectors() {
        // RFC 6234 §8.5 SHA-256 of "abc" =
        //   ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
        let out = run(BuiltinId::Sha2_256, vec![bs(b"abc")]).unwrap();
        let expected =
            hex::decode("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
                .unwrap();
        assert_eq!(out, bs(&expected));
        // Empty input
        let out = run(BuiltinId::Sha2_256, vec![bs(b"")]).unwrap();
        let expected =
            hex::decode("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
                .unwrap();
        assert_eq!(out, bs(&expected));
    }

    #[test]
    fn sha3_256_known_vector() {
        // FIPS 202 SHA3-256 of "abc" =
        //   3a985da74fe225b2045c172d6bd390bd855f086e3e9d525b46bfe24511431532
        let out = run(BuiltinId::Sha3_256, vec![bs(b"abc")]).unwrap();
        let expected =
            hex::decode("3a985da74fe225b2045c172d6bd390bd855f086e3e9d525b46bfe24511431532")
                .unwrap();
        assert_eq!(out, bs(&expected));
    }

    #[test]
    fn keccak_256_known_vector() {
        // Keccak-256 of empty string =
        //   c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470
        let out = run(BuiltinId::Keccak_256, vec![bs(b"")]).unwrap();
        let expected =
            hex::decode("c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470")
                .unwrap();
        assert_eq!(out, bs(&expected));
    }

    #[test]
    fn blake2b_256_known_vector() {
        // BLAKE2b-256 of "abc" (verified against the `blake2` crate's
        // Blake2b<U32>::digest reference output).
        let out = run(BuiltinId::Blake2b_256, vec![bs(b"abc")]).unwrap();
        let expected =
            hex::decode("bddd813c634239723171ef3fee98579b94964e3bb1cb3e427262c8c068d52319")
                .unwrap();
        assert_eq!(out, bs(&expected));
    }

    #[test]
    fn blake2b_224_known_vector() {
        // BLAKE2b-224 of "abc" — verified against `blake2` crate's
        // Blake2b<U28>::digest. Same key as cardano-base's
        // `Crypto.Hash.Blake2b_224`.
        let out = run(BuiltinId::Blake2b_224, vec![bs(b"abc")]).unwrap();
        let expected =
            hex::decode("9bd237b02a29e43bdd6738afa5b53ff0eee178d6210b618e4511aec8").unwrap();
        assert_eq!(out, bs(&expected));
    }

    #[test]
    fn ripemd_160_known_vector() {
        // RIPEMD-160 of "abc" =
        //   8eb208f7e05d987a9b044a8e98c6b087f15a0bfc
        let out = run(BuiltinId::Ripemd_160, vec![bs(b"abc")]).unwrap();
        let expected = hex::decode("8eb208f7e05d987a9b044a8e98c6b087f15a0bfc").unwrap();
        assert_eq!(out, bs(&expected));
    }

    #[test]
    fn hashes_take_arbitrary_input() {
        // 1 KiB input: every hash builtin returns a fixed-size output.
        let input: Vec<u8> = (0..1024u32).map(|i| (i & 0xff) as u8).collect();
        for (id, len) in [
            (BuiltinId::Sha2_256, 32),
            (BuiltinId::Sha3_256, 32),
            (BuiltinId::Blake2b_256, 32),
            (BuiltinId::Blake2b_224, 28),
            (BuiltinId::Keccak_256, 32),
            (BuiltinId::Ripemd_160, 20),
        ] {
            let out = run(id, vec![bs(&input)]).unwrap();
            if let Value::Const(Constant::ByteString(b)) = out {
                assert_eq!(b.len(), len, "{} length", id.name());
            } else {
                panic!("expected ByteString from {}", id.name());
            }
        }
    }

    // ── ByteString operations ─────────────────────────────────────

    fn bs(b: &[u8]) -> Value {
        Value::Const(Constant::ByteString(b.to_vec()))
    }
    fn s(v: &str) -> Value {
        Value::Const(Constant::String(v.into()))
    }

    #[test]
    fn append_byte_string() {
        assert_eq!(
            run(
                BuiltinId::AppendByteString,
                vec![bs(b"hello "), bs(b"world")]
            )
            .unwrap(),
            bs(b"hello world")
        );
    }

    #[test]
    fn cons_byte_string_prepends_byte() {
        assert_eq!(
            run(BuiltinId::ConsByteString, vec![int(0x41), bs(b"bc")]).unwrap(),
            bs(b"Abc")
        );
    }

    #[test]
    fn cons_byte_string_out_of_range_fails() {
        assert!(matches!(
            run(BuiltinId::ConsByteString, vec![int(256), bs(b"")]),
            Err(UplcError::BuiltinFailure { .. })
        ));
        assert!(matches!(
            run(BuiltinId::ConsByteString, vec![int(-1), bs(b"")]),
            Err(UplcError::BuiltinFailure { .. })
        ));
    }

    #[test]
    fn slice_byte_string_clamps_to_bounds() {
        let bs7 = bs(b"abcdefg");
        assert_eq!(
            run(
                BuiltinId::SliceByteString,
                vec![int(1), int(3), bs7.clone()]
            )
            .unwrap(),
            bs(b"bcd")
        );
        // start past end → empty
        assert_eq!(
            run(
                BuiltinId::SliceByteString,
                vec![int(100), int(3), bs7.clone()]
            )
            .unwrap(),
            bs(b"")
        );
        // negative start clamps to 0
        assert_eq!(
            run(BuiltinId::SliceByteString, vec![int(-5), int(3), bs7]).unwrap(),
            bs(b"abc")
        );
    }

    #[test]
    fn length_of_byte_string() {
        assert_eq!(
            run(BuiltinId::LengthOfByteString, vec![bs(b"abc")]).unwrap(),
            int(3)
        );
        assert_eq!(
            run(BuiltinId::LengthOfByteString, vec![bs(b"")]).unwrap(),
            int(0)
        );
    }

    #[test]
    fn index_byte_string_in_range() {
        assert_eq!(
            run(BuiltinId::IndexByteString, vec![bs(b"hello"), int(0)]).unwrap(),
            int(b'h' as i64)
        );
        assert_eq!(
            run(BuiltinId::IndexByteString, vec![bs(b"hello"), int(4)]).unwrap(),
            int(b'o' as i64)
        );
    }

    #[test]
    fn index_byte_string_out_of_range_fails() {
        assert!(matches!(
            run(BuiltinId::IndexByteString, vec![bs(b"abc"), int(5)]),
            Err(UplcError::BuiltinFailure { .. })
        ));
        assert!(matches!(
            run(BuiltinId::IndexByteString, vec![bs(b"abc"), int(-1)]),
            Err(UplcError::BuiltinFailure { .. })
        ));
    }

    #[test]
    fn equals_byte_string() {
        assert_eq!(
            run(BuiltinId::EqualsByteString, vec![bs(b"a"), bs(b"a")]).unwrap(),
            b(true)
        );
        assert_eq!(
            run(BuiltinId::EqualsByteString, vec![bs(b"a"), bs(b"b")]).unwrap(),
            b(false)
        );
    }

    #[test]
    fn lexicographic_byte_string_comparisons() {
        assert_eq!(
            run(BuiltinId::LessThanByteString, vec![bs(b"abc"), bs(b"abd")]).unwrap(),
            b(true)
        );
        assert_eq!(
            run(BuiltinId::LessThanByteString, vec![bs(b"abc"), bs(b"abc")]).unwrap(),
            b(false)
        );
        assert_eq!(
            run(
                BuiltinId::LessThanEqualsByteString,
                vec![bs(b"abc"), bs(b"abc")]
            )
            .unwrap(),
            b(true)
        );
    }

    // ── String operations ─────────────────────────────────────────

    #[test]
    fn append_string() {
        assert_eq!(
            run(BuiltinId::AppendString, vec![s("hello "), s("world")]).unwrap(),
            s("hello world")
        );
    }

    #[test]
    fn equals_string() {
        assert_eq!(
            run(BuiltinId::EqualsString, vec![s("abc"), s("abc")]).unwrap(),
            b(true)
        );
        assert_eq!(
            run(BuiltinId::EqualsString, vec![s("abc"), s("xyz")]).unwrap(),
            b(false)
        );
    }

    #[test]
    fn encode_decode_utf8_round_trip() {
        let s_val = s("héllo 🎉");
        let encoded = run(BuiltinId::EncodeUtf8, vec![s_val.clone()]).unwrap();
        let decoded = run(BuiltinId::DecodeUtf8, vec![encoded]).unwrap();
        assert_eq!(decoded, s_val);
    }

    #[test]
    fn decode_utf8_invalid_fails() {
        // Lone continuation byte 0x80 is invalid UTF-8.
        assert!(matches!(
            run(BuiltinId::DecodeUtf8, vec![bs(&[0x80])]),
            Err(UplcError::BuiltinFailure { .. })
        ));
    }
}
