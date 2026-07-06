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

use crate::builtin::semantics::SemanticsVariant;
use crate::machine::value::Value;
use crate::term::{BuiltinId, Constant};
use crate::UplcError;
use num_bigint::BigInt;
use std::collections::BTreeMap;

/// Inner map type for the `value` constant: policy → token → amount.
type ValueMap = BTreeMap<Vec<u8>, BTreeMap<Vec<u8>, i128>>;

/// Saturated-application denotation. The CEK dispatcher calls this
/// once both the force count and the value-argument count match the
/// builtin's arity.
///
/// `trace_log` is an optional mutable reference to the caller's trace
/// accumulator. When `Some`, the `Trace` builtin appends its first
/// (string) argument here before returning the second. This mirrors
/// the Haskell CEK `emit` mechanism (`traceDenotation text a = a <$
/// emit text`, see `PlutusCore.Default.Builtins`).
///
/// `variant` is the script's [`SemanticsVariant`], which selects between
/// alternative result-level denotations for the small set of builtins whose
/// observable result changed across protocol versions. Today the only such
/// builtin is `consByteString` (lenient `fromIntegral` for V1/V2 vs strict
/// `Word8` for V3); every other denotation is variant-insensitive at the
/// result level (their per-variant difference, if any, is cost-only and is
/// handled in [`crate::cost_apply`]).
pub fn denote(
    id: BuiltinId,
    args: Vec<Value>,
    trace_log: Option<&mut Vec<String>>,
    variant: SemanticsVariant,
) -> Result<Value, UplcError> {
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

        // Trace: emit a log message and return the second argument unchanged.
        //
        // Haskell reference: `traceDenotation text a = a <$ emit text`
        // (`PlutusCore.Default.Builtins`, `PlutusCore.Builtin.Emitter`).
        // The `Trace` builtin has Plutus type `all a. text -> a -> a`;
        // the `emit` call appends to the CEK machine's internal log list.
        // Logs are UTF-8 `Text` values, collected in emission order (FIFO).
        //
        // The caller threads `trace_log` in so the CEK dispatch layer can
        // accumulate strings without needing a global side-channel.
        Trace => {
            let mut it = args.into_iter();
            let msg = it.next().ok_or_else(|| builtin_arity_mismatch(id))?;
            let rest = it.next().ok_or_else(|| builtin_arity_mismatch(id))?;
            // `trace : Text -> a -> a`. Haskell defers unlifting to full
            // saturation; since `trace` takes exactly 2 args, saturation
            // unlifts the first arg as `Text` IMMEDIATELY. A non-Text
            // constant fails with `BuiltinUnliftingEvaluationError` — a
            // genuine evaluation FAILURE, not a silently-ignored no-op that
            // falls through to returning the second argument (#828.1,
            // oracle-confirmed against `PlutusCore/Default/Builtins.hs`
            // `traceDenotation` + the unlifting machinery in
            // `PlutusCore/Builtin/KnownType.hs`). Not semvar-gated — single
            // impl at every protocol version.
            let Value::Const(Constant::String(s)) = &msg else {
                return Err(UplcError::BuiltinTypeError {
                    builtin: id.name(),
                    reason: format!(
                        "trace expected Text first arg, got {:?}",
                        std::mem::discriminant(&msg)
                    ),
                });
            };
            if let Some(log) = trace_log {
                log.push(s.clone());
            }
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
            // `consByteString : Integer -> ByteString -> ByteString`.
            //
            // Two denotations, selected by the script's `SemanticsVariant`
            // (`variant.cons_byte_string_strict()`):
            //
            //   * LENIENT (variants A/B/D = PlutusV1 & PlutusV2 at EVERY
            //     protocol version): the meaning is `BS.cons . fromIntegral`
            //     where `fromIntegral :: Integer -> Word8` is modular /
            //     Euclidean — the integer is reduced mod 256 to a byte and
            //     NEVER errors (256 → 0x00, 257 → 0x01, -1 → 0xFF,
            //     -256 → 0x00).
            //   * STRICT (variants C/E = PlutusV3): the argument is a `Word8`,
            //     so an integer outside `0..=255` is a `BuiltinFailure`.
            //
            // Net rule: STRICT iff the script language is PlutusV3.
            //
            // Source: IntersectMBO/plutus `Builtins.hs`
            // `consByteStringMeaning_V1` / `consByteStringMeaning_V2`
            // (`BS.cons . fromIntegral`, lenient) vs the V3 `Word8`-typed
            // meaning (strict). Commit d3c8d752:
            // https://github.com/IntersectMBO/plutus/blob/d3c8d752/plutus-core/plutus-core/src/PlutusCore/Default/Builtins.hs
            let mut it = args.into_iter();
            let i = unwrap_integer(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let bs = unwrap_byte_string(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let byte = if variant.cons_byte_string_strict() {
                // STRICT (V3): range-check, error on <0 or >255.
                bigint_to_u8(&i, id, "cons byte must be 0..=255")?
            } else {
                // LENIENT (V1/V2): `fromIntegral :: Integer -> Word8`, i.e.
                // reduce mod 256 to a byte. Use EUCLIDEAN remainder (not
                // num-bigint `%`, whose sign follows the dividend, e.g.
                // `-1 % 256 == -1`). `rem_euclid` is guaranteed in 0..=255,
                // so `to_u8` cannot be `None`; map the impossible `None` to a
                // typed error rather than panicking (no-panic invariant).
                use num_traits::{Euclid, ToPrimitive};
                let m = i.rem_euclid(&BigInt::from(256u16));
                m.to_u8()
                    .ok_or_else(|| builtin_failure(id, "cons byte mod 256 out of range"))?
            };
            let mut out = Vec::with_capacity(1 + bs.len());
            out.push(byte);
            out.extend_from_slice(&bs);
            Ok(Value::Const(Constant::ByteString(out)))
        }
        SliceByteString => {
            // sliceByteString : Int -> Int -> bs -> sliced
            // Haskell denotation: `BS.take len (BS.drop start bs)` — a pure,
            // non-failing function once its `Int` args are in hand. BUT the
            // PLC-visible argument type is `integer`; `Int` is unlifted from
            // it via a bounds-check against `Int64` (`[-2^63, 2^63-1]`),
            // throwing `operationalUnliftingError` OUTSIDE that range — this
            // happens BEFORE `take`/`drop` ever run, so an out-of-Int64
            // start/length is an evaluation FAILURE, not a silent clamp
            // (#828.2, oracle-confirmed against
            // `PlutusCore/Default/Universe.hs` `readKnownAsInteger` +
            // `PlutusCore/Default/Builtins.hs`). Values that survive the
            // Int64 unlift then clamp via `BS.take`/`BS.drop`'s own
            // saturating semantics (negative → 0, past-EOF → end).
            let mut it = args.into_iter();
            let start = unwrap_integer(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let len = unwrap_integer(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let bs = unwrap_byte_string(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let start = bigint_to_i64_or_failure(&start, id, "sliceByteString start")?;
            let len = bigint_to_i64_or_failure(&len, id, "sliceByteString length")?;
            let start = i64_to_usize_clamped(start);
            let len = i64_to_usize_clamped(len);
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
            if !pk_is_canonical(&pk_bytes) {
                // libsodium's non-COMPAT ref10 verifier
                // (`ge25519_is_canonical`, called unconditionally from
                // `_crypto_sign_ed25519_verify_detached` before point
                // decompression) rejects any public key whose 255-bit
                // magnitude is >= p = 2^255-19. `ed25519_dalek`'s
                // ZIP-215-permissive `CompressedEdwardsY::decompress`
                // silently reduces such "p+k" aliased encodings mod p
                // instead of rejecting them, so this check must be
                // applied explicitly to match cardano-node byte-for-byte
                // (issue #825).
                return Ok(Value::Const(Constant::Bool(false)));
            }
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
            //
            // #828.5 / #859 (oracle: `PlutusCore/Default/Builtins.hs`
            // ~L1737-1751): genuinely PV-gated, not just a perf/
            // representation swap. At D/E (PV >= `VAN_ROSSEM_PV`) the
            // argument type is `Word64` — unlifting itself rejects a tag
            // outside `0..=2^64-1` as an evaluation failure. At A/B/C
            // (PV < `VAN_ROSSEM_PV`) the argument type is plain `Integer` —
            // Haskell accepts ANY tag (negative or arbitrarily large) and
            // builds `Constr tag args` with it (Haskell's `Data.Constr`
            // field is `Integer`, not `Word64` — the CBOR wire format's
            // Word64-tag requirement is a SEPARATE, PV-independent
            // decode-time constraint that applies only to on-chain
            // datums/redeemers, never to a value computed transiently
            // inside a running script).
            //
            // Since #859, `Data::Constr`'s tag is an arbitrary-precision
            // `BigInt` (matching Haskell's `Integer` exactly), so the A/B/C
            // branch is no longer a representational best-effort
            // approximation: it holds precisely the same domain as
            // Haskell. Only the D/E branch performs a range check, and an
            // out-of-range tag there is a genuine evaluation/unlifting
            // failure (`BuiltinFailure`), never `UplcError::Internal`.
            let mut it = args.into_iter();
            let tag = unwrap_integer(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let list = it.next().ok_or_else(|| builtin_arity_mismatch(id))?;
            let (_, elems) = unwrap_proto_list(list, id)?;
            if variant.constr_data_requires_word64() && u64::try_from(&tag).is_err() {
                return Err(builtin_failure(id, "constrData tag out of u64 range"));
            }
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
                tag, data_args?,
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
            match d.into_constr() {
                Ok((tag, fields)) => {
                    let elements: Vec<Constant> = fields.into_iter().map(Constant::Data).collect();
                    Ok(Value::Const(Constant::ProtoPair {
                        a_type: crate::term::TypeTag::Integer,
                        b_type: crate::term::TypeTag::List(Box::new(crate::term::TypeTag::Data)),
                        a: Box::new(Constant::Integer(tag)),
                        b: Box::new(Constant::ProtoList {
                            elem_type: crate::term::TypeTag::Data,
                            elements,
                        }),
                    }))
                }
                Err(_) => Err(builtin_failure(id, "unConstrData on non-Constr Data")),
            }
        }
        UnMapData => {
            let d = unwrap_data(take_one(args, id)?, id)?;
            match d.into_map() {
                Ok(entries) => {
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
                Err(_) => Err(builtin_failure(id, "unMapData on non-Map Data")),
            }
        }
        UnListData => {
            let d = unwrap_data(take_one(args, id)?, id)?;
            match d.into_list() {
                Ok(items) => Ok(Value::Const(Constant::ProtoList {
                    elem_type: crate::term::TypeTag::Data,
                    elements: items.into_iter().map(Constant::Data).collect(),
                })),
                Err(_) => Err(builtin_failure(id, "unListData on non-List Data")),
            }
        }
        UnIData => {
            let d = unwrap_data(take_one(args, id)?, id)?;
            match d.into_integer() {
                Ok(i) => Ok(Value::Const(Constant::Integer(i))),
                Err(_) => Err(builtin_failure(id, "unIData on non-I Data")),
            }
        }
        UnBData => {
            let d = unwrap_data(take_one(args, id)?, id)?;
            match d.into_bytes() {
                Ok(b) => Ok(Value::Const(Constant::ByteString(b))),
                Err(_) => Err(builtin_failure(id, "unBData on non-B Data")),
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
        AndByteString => bitwise_byte_string(args, id, |a, b| a & b),
        OrByteString => bitwise_byte_string(args, id, |a, b| a | b),
        XorByteString => bitwise_byte_string(args, id, |a, b| a ^ b),
        ComplementByteString => {
            let v = take_one(args, id)?;
            let bs = unwrap_byte_string(v, id)?;
            let out: Vec<u8> = bs.iter().map(|b| !*b).collect();
            Ok(Value::Const(Constant::ByteString(out)))
        }
        ReadBit => {
            // (bs: ByteString) (i: Integer) -> Bool
            //
            // CIP-122 §"Bit ordering": treat the bytestring as a single
            // bit-stream whose LAST byte holds bits 0..7 (with bit 0
            // = LSB), second-to-last byte holds bits 8..15, etc.  So
            // bit `i`'s byte index is `len - 1 - i/8`, and its bit
            // position within that byte is `i % 8`.
            let mut it = args.into_iter();
            let bs = unwrap_byte_string(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let i = unwrap_integer(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let idx = bigint_to_usize_or_failure(&i, id, "readBit index")?;
            if idx >= bs.len().saturating_mul(8) {
                return Err(builtin_failure(id, "readBit: index out of range"));
            }
            let byte_idx = bs.len() - 1 - idx / 8;
            let bit = (bs[byte_idx] >> (idx % 8)) & 1;
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
            // `u64` (#844): a `u32` accumulator overflows for a bytestring
            // over 512 MiB (`u32::MAX / 8` bytes, all-ones), wrapping in
            // release / panicking in debug. Haskell's `Integer` result is
            // arbitrary-precision; `u64` is the widest cheap accumulator
            // that cannot overflow under any real memory budget.
            let count: u64 = bs.iter().map(|b| u64::from(b.count_ones())).sum();
            Ok(Value::Const(Constant::Integer(BigInt::from(count))))
        }
        FindFirstSetBit => {
            // Returns the index of the lowest-position 1 bit, or -1 if
            // none.  Bit ordering per CIP-122: the LAST byte holds the
            // lowest-numbered bits.  Scan from the last byte forward.
            let v = take_one(args, id)?;
            let bs = unwrap_byte_string(v, id)?;
            let n = bs.len();
            for offset in 0..n {
                let byte = bs[n - 1 - offset];
                if byte != 0 {
                    let bit_in_byte = byte.trailing_zeros() as usize;
                    let global = offset * 8 + bit_in_byte;
                    return Ok(Value::Const(Constant::Integer(BigInt::from(global))));
                }
            }
            Ok(Value::Const(Constant::Integer(BigInt::from(-1))))
        }

        // ── ExpModInteger (V3) ────────────────────────────────────────
        ExpModInteger => {
            // (base: Integer) (exp: Integer) (modulus: Integer) -> Integer
            //
            // Full two-layer contract, oracle-confirmed against
            // `PlutusCore/Default/Builtins.hs` (outer `m < 0` guard) +
            // `PlutusCore/Crypto/ExpMod.hs` (`expMod`, inner guards),
            // `maxBoundN = 2^8191 - 1`, `[minBoundI, maxBoundI] =
            // [-2^8191, 2^8191-1]` (#828.4):
            //   * m <= 0 → fail ("invalid modulus")
            //   * m > 2^8191 - 1 → fail ("invalid modulus")
            //   * m == 1 → 0 (special-cased BEFORE the b/e bounds check)
            //   * b == 0 && e < 0 → fail ("not invertible")
            //   * b or e outside [-2^8191, 2^8191-1] (ASYMMETRIC inclusive
            //     range — note `-2^8191` itself is VALID, only `+2^8191`
            //     and beyond are rejected) → fail ("out of bounds")
            //   * otherwise → b^e mod m, where negative e uses the
            //     modular inverse of b mod m (fails when none exists).
            let mut it = args.into_iter();
            let base = unwrap_integer(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let exp = unwrap_integer(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let modulus = unwrap_integer(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            // `bound = 2^8191`. Modulus must be `0 < m <= bound - 1`;
            // base/exp must be `-bound <= x <= bound - 1`.
            let bound = BigInt::from(1u32) << 8191u32;
            if modulus.sign() != num_bigint::Sign::Plus {
                return Err(builtin_failure(id, "expModInteger: invalid modulus"));
            }
            if modulus >= bound {
                return Err(builtin_failure(id, "expModInteger: invalid modulus"));
            }
            if modulus == BigInt::from(1u32) {
                return Ok(Value::Const(Constant::Integer(BigInt::from(0u32))));
            }
            let neg_bound = -&bound;
            if base < neg_bound || base >= bound || exp < neg_bound || exp >= bound {
                return Err(builtin_failure(id, "expModInteger: out of bounds"));
            }
            if exp.sign() == num_bigint::Sign::Minus {
                if base.sign() == num_bigint::Sign::NoSign {
                    return Err(builtin_failure(id, "expModInteger: 0 is not invertible"));
                }
                // For e < 0: compute modular inverse of base, then
                // raise to |e|.  `BigInt::modinv` gives the inverse if
                // gcd(base, m) == 1; else there is no inverse.
                let abs_exp = -&exp;
                match base.modinv(&modulus) {
                    Some(inv) => Ok(Value::Const(Constant::Integer(
                        inv.modpow(&abs_exp, &modulus),
                    ))),
                    None => Err(builtin_failure(
                        id,
                        "expModInteger: base is not invertible modulo the modulus",
                    )),
                }
            } else {
                Ok(Value::Const(Constant::Integer(base.modpow(&exp, &modulus))))
            }
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
            // The `>= len_bits` early-out (#844) MUST run on the wide
            // `BigInt`/`i64` value BEFORE narrowing to `usize`: on a
            // 32-bit host, `abs_shift_int` can exceed `usize::MAX` (e.g. a
            // shift of 3_000_000_000, still a valid `i64`) even though
            // Haskell's `>= len_bits` comparison (done on the wider `Int`)
            // would simply return the all-zeros result here. Doing the
            // narrowing first would fail a shift Haskell accepts.
            // Irrelevant on the 64-bit targets dugite ships today, but
            // this keeps the code correct independent of `usize` width.
            if abs_shift_int >= BigInt::from(len_bits) {
                return Ok(Value::Const(Constant::ByteString(vec![0u8; bs.len()])));
            }
            let abs_shift_u = bigint_to_usize_or_failure(&abs_shift_int, id, "shift amount")?;
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
                // CIP-122 bit ordering: bit i lives in byte
                // `len - 1 - i/8`, bit position `i % 8`.
                let src_byte = bs.len() - 1 - src / 8;
                let dst_byte = bs.len() - 1 - target_idx / 8;
                let src_bit = (bs[src_byte] >> (src % 8)) & 1;
                if src_bit != 0 {
                    out[dst_byte] |= 1u8 << (target_idx % 8);
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
                // CIP-122 bit ordering, as ShiftByteString above.
                let src_byte = bs.len() - 1 - src / 8;
                let dst_byte = bs.len() - 1 - target_idx / 8;
                let src_bit = (bs[src_byte] >> (src % 8)) & 1;
                if src_bit != 0 {
                    out[dst_byte] |= 1u8 << (target_idx % 8);
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
            // CIP-122/CIP-123 `ensurable` gate: variants D/E cap the input
            // bytestring at maximumInputLength = 4096 bytes (Bitwise.hs:82,
            // Builtins.hs:2199). A/B/C (incl. V3 at plominPV) impose no cap.
            // Fires on input length BEFORE the per-index checks, matching the
            // Haskell guard order (over-length fails even with empty/in-range
            // indices).
            const WRITE_BITS_MAX_INPUT_LENGTH: usize = 4096;
            if variant.bitwise_max_input_enforced() && bs.len() > WRITE_BITS_MAX_INPUT_LENGTH {
                return Err(builtin_failure(
                    id,
                    "writeBits: input too long (maximum is 4096 bytes)",
                ));
            }
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
                // Same CIP-122 bit ordering as ReadBit — the LAST
                // byte holds bits 0..7.
                let byte_idx = out.len() - 1 - idx / 8;
                let mask = 1u8 << (idx % 8);
                if value {
                    out[byte_idx] |= mask;
                } else {
                    out[byte_idx] &= !mask;
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
            // #828.3: `secp256k1_ecdsa_signature_parse_compact` (the C
            // library Haskell's `cardano-crypto-class` wraps) only rejects
            // scalar OVERFLOW (r or s >= curve order n) at parse time —
            // zero is not an overflow, so a zero r or s PARSES successfully
            // there. The zero-scalar rejection happens later, inside
            // `secp256k1_ecdsa_sig_verify`, and produces a successful
            // `False` result — NOT a parse/evaluation failure. k256's
            // `Signature::from_bytes` is stricter and rejects a zero scalar
            // at parse, which would otherwise surface as a `BuiltinFailure`
            // here and diverge from Haskell (oracle-confirmed against
            // bitcoin-core/secp256k1 `src/{secp256k1.c,ecdsa_impl.h}`).
            // Detect a zero r or s half directly on the raw bytes and
            // short-circuit to `False` before handing off to k256.
            let (r_half, s_half) = sig_arr.split_at(32);
            if r_half.iter().all(|&byte| byte == 0) || s_half.iter().all(|&byte| byte == 0) {
                return Ok(Value::Const(Constant::Bool(false)));
            }
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
            use k256::schnorr::{Signature, VerifyingKey};
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
            // `verify_raw` matches BIP-340 exactly: the message is fed
            // straight into the tagged challenge hash without an outer
            // SHA-256 wrap.  k256's `Verifier::verify` would call
            // `Sha256::new_with_prefix(msg).finalize()` first, which
            // produces a different point on the curve and therefore
            // rejects every valid Plutus test vector.
            Ok(Value::Const(Constant::Bool(
                vk.verify_raw(&msg, &sig).is_ok(),
            )))
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

        // ── PV1.1.0 list builtin ──────────────────────────────────────
        DropList => {
            // (count: Integer, list: ProtoList T) -> ProtoList T
            // Drops the first `count` elements from the list. If count
            // is negative or larger than the list length, returns the
            // empty list (matching the Haskell reference's clamp-at-
            // zero / clamp-at-length semantics).
            let mut it = args.into_iter();
            let count = unwrap_integer(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let list = it.next().ok_or_else(|| builtin_arity_mismatch(id))?;
            let (elem_type, mut elems) = unwrap_proto_list(list, id)?;
            let n = bigint_to_usize_clamped(&count);
            if n >= elems.len() {
                elems.clear();
            } else {
                elems.drain(..n);
            }
            Ok(Value::Const(Constant::ProtoList {
                elem_type,
                elements: elems,
            }))
        }

        // ── PV1.1.0 array builtins ────────────────────────────────────
        IndexArray => {
            // (arr: Array T, i: Integer) -> T
            // Returns the element at index `i`. Out-of-range → BuiltinFailure.
            let mut it = args.into_iter();
            let arr_val = it.next().ok_or_else(|| builtin_arity_mismatch(id))?;
            let i_val = it.next().ok_or_else(|| builtin_arity_mismatch(id))?;
            let (_, elems) = unwrap_array(arr_val, id)?;
            let i = unwrap_integer(i_val, id)?;
            let idx = match usize::try_from(&i) {
                Ok(n) if n < elems.len() => n,
                _ => return Err(builtin_failure(id, "indexArray: index out of range")),
            };
            Ok(Value::Const(elems.into_iter().nth(idx).ok_or_else(
                || builtin_failure(id, "indexArray: index out of range (internal)"),
            )?))
        }
        LengthOfArray => {
            // (arr: Array T) -> Integer
            let v = take_one(args, id)?;
            let (_, elems) = unwrap_array(v, id)?;
            Ok(Value::Const(Constant::Integer(BigInt::from(elems.len()))))
        }
        ListToArray => {
            // (list: ProtoList T) -> Array T
            let v = take_one(args, id)?;
            let (elem_type, elements) = unwrap_proto_list(v, id)?;
            Ok(Value::Const(Constant::Array {
                elem_type,
                elements,
            }))
        }

        // ── PV1.1.0 Value builtins ────────────────────────────────────
        InsertCoin => {
            // (policy: ByteString) (token: ByteString) (amount: Integer)
            // (value: Value) -> Value
            //
            // Mirrors the Haskell `insertCoin` semantics exactly:
            //   - amount == 0  → deleteCoin (remove entry if present; no key checks)
            //   - amount != 0  → validate keys (≤32 bytes) and amount (i128 range),
            //                    then SET (not add) the entry to `amount`.
            // Keys longer than 32 bytes with non-zero amount → BuiltinFailure.
            // Amount outside i128 range → BuiltinFailure.
            let mut it = args.into_iter();
            let policy =
                unwrap_byte_string(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let token =
                unwrap_byte_string(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let amount_bi =
                unwrap_integer(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let mut val = unwrap_value(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;

            // amount == 0 → deleteCoin: remove entry if present; bypass key checks.
            use num_traits::{ToPrimitive, Zero};
            if amount_bi.is_zero() {
                if let Some(inner) = val.get_mut(&policy) {
                    inner.remove(&token);
                    if inner.is_empty() {
                        val.remove(&policy);
                    }
                }
                return Ok(Value::Const(Constant::Value(val)));
            }

            // Non-zero amount: validate key lengths (32-byte limit per Plutus ledger rules).
            const MAX_KEY: usize = 32;
            if policy.len() > MAX_KEY {
                return Err(builtin_failure(
                    id,
                    "insertCoin: policy-id exceeds 32 bytes",
                ));
            }
            if token.len() > MAX_KEY {
                return Err(builtin_failure(
                    id,
                    "insertCoin: token-name exceeds 32 bytes",
                ));
            }

            // Validate amount is within signed 128-bit range.
            let amount = amount_bi
                .to_i128()
                .ok_or_else(|| builtin_failure(id, "insertCoin: amount out of i128 range"))?;

            // SET semantics: insert or overwrite the (policy, token) entry.
            val.entry(policy.clone()).or_default().insert(token, amount);
            Ok(Value::Const(Constant::Value(val)))
        }

        LookupCoin => {
            // (policy: ByteString) (token: ByteString) (value: Value) -> Integer
            // Returns 0 if not present.
            let mut it = args.into_iter();
            let policy =
                unwrap_byte_string(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let token =
                unwrap_byte_string(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let val = unwrap_value(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let amt = val
                .get(&policy)
                .and_then(|inner| inner.get(&token))
                .copied()
                .unwrap_or(0);
            Ok(Value::Const(Constant::Integer(BigInt::from(amt))))
        }

        ScaleValue => {
            // (factor: Integer) (value: Value) -> Value
            // Multiplies every coin amount by `factor`. If a resulting
            // amount is zero, the entry is removed.  Overflow → BuiltinFailure.
            let mut it = args.into_iter();
            let factor_bi =
                unwrap_integer(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let val = unwrap_value(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            use num_traits::ToPrimitive;
            let factor = factor_bi
                .to_i128()
                .ok_or_else(|| builtin_failure(id, "scaleValue: factor out of i128 range"))?;
            let mut out: ValueMap = BTreeMap::new();
            for (policy, inner) in val {
                let mut new_inner: BTreeMap<Vec<u8>, i128> = BTreeMap::new();
                for (token, amt) in inner {
                    let new_amt = amt.checked_mul(factor).ok_or_else(|| {
                        builtin_failure(id, "scaleValue: multiplication overflow")
                    })?;
                    if new_amt != 0 {
                        new_inner.insert(token, new_amt);
                    }
                }
                if !new_inner.is_empty() {
                    out.insert(policy, new_inner);
                }
            }
            Ok(Value::Const(Constant::Value(out)))
        }

        UnValueData => {
            // (data: Data) -> Value
            //
            // Decodes a Data value of the form
            //   Map [(B policy, Map [(B token, I amount), ...]), ...]
            // into a canonical Value.
            //
            // The Haskell reference (`PlutusCore.Value.buildValueWith`) validates:
            //   1. The outer and inner maps must be Map data constructors.
            //   2. Currency/token keys must be B constructors with ≤ 32 bytes.
            //   3. Amounts must be I constructors within signed 128-bit range.
            //   4. Amounts must NOT be zero (zero quantity → evaluation failure).
            //   5. Currency symbols must be STRICTLY ASCENDING (no duplicates,
            //      no out-of-order entries).
            //   6. Token names within each currency must be STRICTLY ASCENDING.
            //   7. Inner maps must NOT be empty.
            // All of these are failure conditions — unValueData does NOT merge
            // duplicates or normalise zeros.
            let d = unwrap_data(take_one(args, id)?, id)?;
            let outer_pairs = match d.into_map() {
                Ok(pairs) => pairs,
                Err(_) => return Err(builtin_failure(id, "unValueData: expected outer Map")),
            };
            const MAX_KEY: usize = 32;
            use num_traits::ToPrimitive;
            let mut result: ValueMap = BTreeMap::new();
            let mut prev_policy: Option<Vec<u8>> = None;
            for (k, v) in outer_pairs {
                let policy = match k.into_bytes() {
                    Ok(b) => b,
                    Err(_) => return Err(builtin_failure(id, "unValueData: policy key must be B")),
                };
                if policy.len() > MAX_KEY {
                    return Err(builtin_failure(
                        id,
                        "unValueData: policy-id exceeds 32 bytes",
                    ));
                }
                // Strictly ascending check for currency symbols.
                if let Some(ref prev) = prev_policy {
                    if policy <= *prev {
                        return Err(builtin_failure(
                            id,
                            "unValueData: currency symbols not strictly ascending",
                        ));
                    }
                }
                prev_policy = Some(policy.clone());

                let inner_pairs = match v.into_map() {
                    Ok(pairs) => pairs,
                    Err(_) => {
                        return Err(builtin_failure(id, "unValueData: expected inner Map"));
                    }
                };
                // Empty inner map is a failure.
                if inner_pairs.is_empty() {
                    return Err(builtin_failure(id, "unValueData: empty inner map"));
                }
                let inner_map = result.entry(policy).or_default();
                let mut prev_token: Option<Vec<u8>> = None;
                for (tk, tv) in inner_pairs {
                    let token = match tk.into_bytes() {
                        Ok(b) => b,
                        Err(_) => {
                            return Err(builtin_failure(id, "unValueData: token key must be B"))
                        }
                    };
                    if token.len() > MAX_KEY {
                        return Err(builtin_failure(
                            id,
                            "unValueData: token-name exceeds 32 bytes",
                        ));
                    }
                    // Strictly ascending check for token names.
                    if let Some(ref prev) = prev_token {
                        if token <= *prev {
                            return Err(builtin_failure(
                                id,
                                "unValueData: token names not strictly ascending",
                            ));
                        }
                    }
                    prev_token = Some(token.clone());

                    let amount_bi = match tv.into_integer() {
                        Ok(i) => i,
                        Err(_) => {
                            return Err(builtin_failure(id, "unValueData: token amount must be I"))
                        }
                    };
                    let amount = amount_bi.to_i128().ok_or_else(|| {
                        builtin_failure(id, "unValueData: amount outside i128 range")
                    })?;
                    // Zero quantity is not allowed.
                    if amount == 0 {
                        return Err(builtin_failure(id, "unValueData: zero quantity"));
                    }
                    inner_map.insert(token, amount);
                }
            }
            Ok(Value::Const(Constant::Value(result)))
        }

        ValueData => {
            // (value: Value) -> Data
            // Encodes as Map [(B policy, Map [(B token, I amount), ...]), ...].
            let val = unwrap_value(take_one(args, id)?, id)?;
            use crate::data::Data;
            let outer: Vec<(Data, Data)> = val
                .into_iter()
                .map(|(policy, inner)| {
                    let inner_data: Vec<(Data, Data)> = inner
                        .into_iter()
                        .map(|(token, amt)| (Data::B(token), Data::I(BigInt::from(amt))))
                        .collect();
                    (Data::B(policy), Data::Map(inner_data))
                })
                .collect();
            Ok(Value::Const(Constant::Data(Data::Map(outer))))
        }

        ValueContains => {
            // (super_val: Value) (sub_val: Value) -> Bool
            //
            // Mirrors the Haskell `valueContains` semantics:
            //   - Fails (BuiltinFailure) if EITHER value contains negative amounts.
            //   - Returns True iff sub_val is a sub-map-by-≤ of super_val.
            let mut it = args.into_iter();
            let sup = unwrap_value(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let sub = unwrap_value(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            // Check for negative amounts in either value.
            let has_negative = |map: &BTreeMap<Vec<u8>, BTreeMap<Vec<u8>, i128>>| {
                map.values()
                    .flat_map(|inner| inner.values())
                    .any(|&amt| amt < 0)
            };
            if has_negative(&sup) {
                return Err(builtin_failure(
                    id,
                    "valueContains: first value contains negative amounts",
                ));
            }
            if has_negative(&sub) {
                return Err(builtin_failure(
                    id,
                    "valueContains: second value contains negative amounts",
                ));
            }
            // sub must be a sub-map-by-≤ of sup.
            let mut ok = true;
            'outer: for (policy, inner_sub) in &sub {
                let inner_sup = match sup.get(policy) {
                    Some(m) => m,
                    None => {
                        ok = false;
                        break 'outer;
                    }
                };
                for (token, &sub_amt) in inner_sub {
                    let sup_amt = inner_sup.get(token).copied().unwrap_or(0);
                    if sup_amt < sub_amt {
                        ok = false;
                        break 'outer;
                    }
                }
            }
            Ok(Value::Const(Constant::Bool(ok)))
        }

        UnionValue => {
            // (a: Value) (b: Value) -> Value
            // Merges two values by summing amounts; zero entries are removed.
            // Overflow/underflow beyond i128 range → BuiltinFailure.
            let mut it = args.into_iter();
            let mut a = unwrap_value(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            let b = unwrap_value(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
            for (policy, inner_b) in b {
                let inner_a = a.entry(policy).or_default();
                for (token, amt_b) in inner_b {
                    let slot = inner_a.entry(token).or_insert(0);
                    *slot = slot.checked_add(amt_b).ok_or_else(|| {
                        builtin_failure(
                            id,
                            "unionValue: quantity is out of the signed 128-bit integer bounds",
                        )
                    })?;
                }
            }
            // Remove zeros and empty inner maps.
            for inner in a.values_mut() {
                inner.retain(|_, v| *v != 0);
            }
            a.retain(|_, inner| !inner.is_empty());
            Ok(Value::Const(Constant::Value(a)))
        }

        // ── PV1.1.0 BLS multi-scalar multiplication ──────────────────
        Bls12_381_G1_MultiScalarMul | Bls12_381_G2_MultiScalarMul => {
            crate::builtin::bls::denote_multi_scalar_mul(id, args)
        }
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

/// Mirrors libsodium's `ge25519_is_canonical` (called unconditionally on
/// the public key by `_crypto_sign_ed25519_verify_detached`'s non-COMPAT
/// `ref10` path, which is what `cardano-base`/`cardano-node` always
/// compiles): returns `true` iff the 32-byte little-endian encoding's
/// 255-bit magnitude — i.e. the value with the sign bit (top bit of byte
/// 31) masked off — is strictly less than `p = 2^255 - 19`.
///
/// `ed25519-dalek`'s `CompressedEdwardsY::decompress` has no equivalent
/// check (it silently reduces the field element mod `p`, per ZIP-215),
/// so a public key encoding `p + k` for small `k` decompresses to the
/// same curve point as the canonical encoding of `k` — an aliasing
/// surface libsodium/cardano-node categorically reject. See #825.
fn pk_is_canonical(pk: &[u8; 32]) -> bool {
    // p = 2^255 - 19 as 32 little-endian bytes, with the sign-bit slot
    // (top bit of byte 31) cleared (p < 2^255, so that bit is 0).
    const P: [u8; 32] = {
        let mut p = [0xffu8; 32];
        p[0] = 0xed;
        p[31] = 0x7f;
        p
    };
    let mut magnitude = *pk;
    magnitude[31] &= 0x7f;
    // Big-endian-order byte comparison (MSB first) of two 255-bit
    // magnitudes stored little-endian: canonical iff magnitude < p.
    for i in (0..32).rev() {
        match magnitude[i].cmp(&P[i]) {
            std::cmp::Ordering::Less => return true,
            std::cmp::Ordering::Greater => return false,
            std::cmp::Ordering::Equal => {}
        }
    }
    // magnitude == p exactly => >= p => non-canonical.
    false
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
fn bitwise_byte_string<F>(args: Vec<Value>, id: BuiltinId, op: F) -> Result<Value, UplcError>
where
    F: Fn(u8, u8) -> u8,
{
    // CIP-122/123 (`andByteString` / `orByteString` / `xorByteString`)
    // take a `padded: Bool` flag plus two bytestrings.
    //
    // Reference (`PlutusCore.Bitwise`): sort the inputs into
    // (shorter, longer); if `padded` then output starts as a copy of
    // `longer` (max-length); else output starts as a copy of `shorter`
    // (min-length).  The op is applied bytewise for the first
    // `len(shorter)` positions of the output; bytes beyond that are
    // left as-is (they came from the longer side and have no
    // counterpart in the shorter).
    let mut it = args.into_iter();
    let padded = unwrap_bool(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
    let a = unwrap_byte_string(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
    let b = unwrap_byte_string(it.next().ok_or_else(|| builtin_arity_mismatch(id))?, id)?;
    let (shorter, longer) = if a.len() <= b.len() {
        (&a, &b)
    } else {
        (&b, &a)
    };
    let traverse_len = shorter.len();
    let mut out: Vec<u8> = if padded {
        longer.clone()
    } else {
        shorter.clone()
    };
    for i in 0..traverse_len {
        let x = shorter[i];
        let y = longer[i];
        out[i] = op(x, y);
    }
    Ok(Value::Const(Constant::ByteString(out)))
}

fn unwrap_array(
    v: Value,
    id: BuiltinId,
) -> Result<(crate::term::TypeTag, Vec<Constant>), UplcError> {
    match v {
        Value::Const(Constant::Array {
            elem_type,
            elements,
        }) => Ok((elem_type, elements)),
        other => Err(UplcError::BuiltinTypeError {
            builtin: id.name(),
            reason: format!("expected Array, got {:?}", std::mem::discriminant(&other)),
        }),
    }
}

fn unwrap_value(v: Value, id: BuiltinId) -> Result<ValueMap, UplcError> {
    match v {
        Value::Const(Constant::Value(val)) => Ok(val),
        other => Err(UplcError::BuiltinTypeError {
            builtin: id.name(),
            reason: format!("expected Value, got {:?}", std::mem::discriminant(&other)),
        }),
    }
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

/// Clamp an ALREADY-Int64-validated value to `usize` for `take`/`drop`-style
/// indexing (#828.2): negative values clamp to 0 (mirrors `BS.drop`/
/// `BS.take`'s own saturating semantics for in-Int64-but-out-of-bounds
/// values); non-negative `i64` values always fit `usize` on the 64-bit
/// platforms dugite targets. Callers MUST bounds-check the source `BigInt`
/// against `Int64` first (via [`bigint_to_i64_or_failure`]) — this helper
/// performs no such check itself and must never be fed a raw `BigInt`
/// directly (that would silently reintroduce the #828.2 bug).
fn i64_to_usize_clamped(n: i64) -> usize {
    n.max(0) as usize
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
        // Default to the STRICT / latest variant (matches existing behaviour
        // for every variant-insensitive builtin).
        denote(id, args, None, SemanticsVariant::LATEST)
    }

    /// Run a denotation under an explicit [`SemanticsVariant`] — for the
    /// `consByteString` lenient/strict tests.
    fn run_variant(
        id: BuiltinId,
        args: Vec<Value>,
        variant: SemanticsVariant,
    ) -> Result<Value, UplcError> {
        denote(id, args, None, variant)
    }

    fn run_with_log(
        id: BuiltinId,
        args: Vec<Value>,
        log: &mut Vec<String>,
    ) -> Result<Value, UplcError> {
        denote(id, args, Some(log), SemanticsVariant::LATEST)
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
        // Trace still returns its second arg regardless of whether a log is provided.
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
    fn trace_appends_string_to_log() {
        let mut log: Vec<String> = Vec::new();
        let result = run_with_log(
            BuiltinId::Trace,
            vec![
                Value::Const(Constant::String("hello world".into())),
                int(42),
            ],
            &mut log,
        )
        .unwrap();
        assert_eq!(result, int(42), "trace must return its second argument");
        assert_eq!(
            log,
            vec!["hello world"],
            "trace must append the string to the log"
        );
    }

    #[test]
    fn trace_multiple_calls_ordered_fifo() {
        // Simulate two sequential trace calls — logs must be in emission order.
        let mut log: Vec<String> = Vec::new();
        run_with_log(
            BuiltinId::Trace,
            vec![Value::Const(Constant::String("first".into())), int(1)],
            &mut log,
        )
        .unwrap();
        run_with_log(
            BuiltinId::Trace,
            vec![Value::Const(Constant::String("second".into())), int(2)],
            &mut log,
        )
        .unwrap();
        run_with_log(
            BuiltinId::Trace,
            vec![Value::Const(Constant::String("third".into())), int(3)],
            &mut log,
        )
        .unwrap();
        assert_eq!(log, vec!["first", "second", "third"]);
    }

    #[test]
    fn trace_without_log_does_not_capture() {
        // When trace_log is None the builtin must still succeed and return arg2,
        // but nothing is captured — no panic, no error.
        let result = run(
            BuiltinId::Trace,
            vec![Value::Const(Constant::String("discarded".into())), int(7)],
        )
        .unwrap();
        assert_eq!(result, int(7));
    }

    #[test]
    fn trace_non_string_first_arg_fails_evaluation() {
        // #828.1: Haskell unlifts `trace`'s first arg as `Text` at
        // saturation; a non-Text constant raises
        // `BuiltinUnliftingEvaluationError` — a genuine evaluation FAILURE.
        // It must NOT silently fall through and return the second arg.
        let mut log: Vec<String> = Vec::new();
        let err = run_with_log(BuiltinId::Trace, vec![int(999), int(42)], &mut log).unwrap_err();
        assert!(
            matches!(err, UplcError::BuiltinTypeError { .. }),
            "expected BuiltinTypeError, got {err:?}"
        );
        assert!(log.is_empty(), "a failed trace must not write to log");
    }

    #[test]
    fn builtin_wrong_arity_returns_internal() {
        // VerifyEcdsaSecp256k1Signature IS wired (see the `run_ecdsa`
        // dispatch arm above) — calling it with zero args instead hits the
        // denotation's own arity guard (`builtin_arity_mismatch`), which is
        // a genuine dugite-uplc invariant violation (the dispatch layer is
        // supposed to guarantee the right argument count before invoking
        // any denotation), hence `UplcError::Internal` here IS the correct
        // classification — unlike the adversary-reachable machine errors
        // reclassified in #840.
        let err = run(BuiltinId::VerifyEcdsaSecp256k1Signature, vec![]).unwrap_err();
        assert!(matches!(err, UplcError::Internal(_)));
    }

    #[test]
    fn verify_ecdsa_zero_r_or_s_returns_false_not_failure() {
        // #828.3: `secp256k1_ecdsa_signature_parse_compact` only rejects
        // scalar OVERFLOW at parse; a zero r or s parses fine and fails
        // verification with a successful `False` result, not an evaluation
        // failure. Build a real valid pubkey so the length/parse gates
        // upstream of the zero-scalar check pass cleanly.
        use k256::ecdsa::SigningKey;

        let sk = SigningKey::from_bytes(&[7u8; 32].into()).expect("valid scalar");
        let pk_bytes = sk
            .verifying_key()
            .to_encoded_point(true)
            .as_bytes()
            .to_vec();
        assert_eq!(pk_bytes.len(), 33);
        let msg_hash = vec![0x11u8; 32];

        // r = 0 (first 32 bytes), s = arbitrary non-zero.
        let mut sig_zero_r = vec![0u8; 32];
        sig_zero_r.extend(std::iter::repeat_n(0x01u8, 32));
        assert_eq!(
            run(
                BuiltinId::VerifyEcdsaSecp256k1Signature,
                vec![bs(&pk_bytes), bs(&msg_hash), bs(&sig_zero_r)],
            )
            .unwrap(),
            b(false),
            "zero-r signature must return False, not fail"
        );

        // s = 0 (last 32 bytes), r = arbitrary non-zero.
        let mut sig_zero_s = vec![0x01u8; 32];
        sig_zero_s.extend(std::iter::repeat_n(0u8, 32));
        assert_eq!(
            run(
                BuiltinId::VerifyEcdsaSecp256k1Signature,
                vec![bs(&pk_bytes), bs(&msg_hash), bs(&sig_zero_s)],
            )
            .unwrap(),
            b(false),
            "zero-s signature must return False, not fail"
        );

        // Both-zero also returns False (not evaluated as a special case,
        // just both halves passing the all-zero check).
        let sig_all_zero = vec![0u8; 64];
        assert_eq!(
            run(
                BuiltinId::VerifyEcdsaSecp256k1Signature,
                vec![bs(&pk_bytes), bs(&msg_hash), bs(&sig_all_zero)],
            )
            .unwrap(),
            b(false)
        );
    }

    #[test]
    fn exp_mod_integer_bounds_and_special_cases() {
        // #828.4: full contract per `PlutusCore.Crypto.ExpMod`.
        let bound = || BigInt::from(1u32) << 8191u32; // 2^8191

        // m <= 0 → fail.
        assert!(matches!(
            run(BuiltinId::ExpModInteger, vec![int(2), int(3), int(0)]),
            Err(UplcError::BuiltinFailure { .. })
        ));
        assert!(matches!(
            run(BuiltinId::ExpModInteger, vec![int(2), int(3), int(-5)]),
            Err(UplcError::BuiltinFailure { .. })
        ));

        // m == 1 → always 0, regardless of base/exp.
        assert_eq!(
            run(BuiltinId::ExpModInteger, vec![int(999), int(999), int(1)]).unwrap(),
            int(0)
        );

        // m > 2^8191 - 1 → fail (the missing upper-bound check, #828.4a).
        let modulus_too_big = Value::Const(Constant::Integer(bound()));
        assert!(
            matches!(
                run(
                    BuiltinId::ExpModInteger,
                    vec![int(2), int(1), modulus_too_big]
                ),
                Err(UplcError::BuiltinFailure { .. })
            ),
            "modulus == 2^8191 must fail (valid range is m <= 2^8191 - 1)"
        );

        // base/exp == -2^8191 (minBoundI) is VALID (asymmetric inclusive
        // range), #828.4b — must NOT fail.
        let min_bound_base = Value::Const(Constant::Integer(-bound()));
        assert!(
            run(
                BuiltinId::ExpModInteger,
                vec![min_bound_base, int(1), int(1_000_003)]
            )
            .is_ok(),
            "base == -2^8191 (minBoundI) must be ACCEPTED, not rejected"
        );

        // base/exp == +2^8191 is INVALID (max is 2^8191 - 1) — must fail.
        let plus_bound_base = Value::Const(Constant::Integer(bound()));
        assert!(matches!(
            run(
                BuiltinId::ExpModInteger,
                vec![plus_bound_base, int(1), int(1_000_003)]
            ),
            Err(UplcError::BuiltinFailure { .. })
        ));

        // base/exp == +(2^8191 - 1) (maxBoundI) is VALID.
        let max_bound_base = Value::Const(Constant::Integer(bound() - 1));
        assert!(
            run(
                BuiltinId::ExpModInteger,
                vec![max_bound_base, int(1), int(1_000_003)]
            )
            .is_ok(),
            "base == 2^8191 - 1 (maxBoundI) must be ACCEPTED"
        );

        // Ordinary case still computes correctly: 3^4 mod 5 = 81 mod 5 = 1.
        assert_eq!(
            run(BuiltinId::ExpModInteger, vec![int(3), int(4), int(5)]).unwrap(),
            int(1)
        );
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
                BigInt::from(3),
                vec![Data::I(BigInt::from(1)), Data::I(BigInt::from(2))]
            ))
        );
    }

    /// #859 (closes the #828.5 residual): `constrData`'s tag argument is
    /// unlifted as `Word64` at D/E (PV>=11) — an out-of-range tag is a
    /// genuine `BuiltinFailure` — but as plain arbitrary-precision
    /// `Integer` at A/B/C (PV<11), where Haskell ACCEPTS a negative or
    /// oversized tag and builds `Constr tag args` with it. `Data::Constr`'s
    /// tag is now a `BigInt` (issue #859), so dugite represents this
    /// domain exactly at A/B/C — no more `Internal`/representational-
    /// limitation fallback. In-range tags behave IDENTICALLY at every
    /// variant. The conformance corpus runs a single (LATEST=E, no-PV)
    /// harness and cannot catch a wrong PV<11 branch, hence this dedicated
    /// matrix test.
    #[test]
    fn constr_data_tag_range_is_pv_gated() {
        // In-range tag (fits u64): succeeds identically at every variant —
        // (c) regression: a small-tag Constr still round-trips exactly.
        let fields = || list_of_data(vec![]);
        for variant in [
            SemanticsVariant::A,
            SemanticsVariant::B,
            SemanticsVariant::C,
            SemanticsVariant::D,
            SemanticsVariant::E,
        ] {
            let v = run_variant(BuiltinId::ConstrData, vec![int(7), fields()], variant).unwrap();
            assert_eq!(v, data_val(Data::Constr(BigInt::from(7), vec![])));
        }

        // (b) Out-of-u64-range tag (negative): D/E reject as a genuine
        // BuiltinFailure (Word64 unlifting failure) — NOT Internal, NOT a
        // silent clamp.
        let neg_tag = Value::Const(Constant::Integer(BigInt::from(-1)));
        for variant in [SemanticsVariant::D, SemanticsVariant::E] {
            assert!(
                matches!(
                    run_variant(
                        BuiltinId::ConstrData,
                        vec![neg_tag.clone(), fields()],
                        variant
                    ),
                    Err(UplcError::BuiltinFailure { .. })
                ),
                "variant {variant:?} must reject an out-of-Word64 tag as BuiltinFailure"
            );
        }

        // (a) A/B/C: Haskell ACCEPTS a negative OR oversized (> u64::MAX)
        // tag (plain arbitrary-precision Integer argument) and builds the
        // wide Constr. dugite's BigInt-tagged `Data::Constr` now
        // represents this exactly — no error, no clamp, no representational
        // gap — and the tag round-trips through `unConstrData` unchanged.
        let huge_tag_value: BigInt = BigInt::from(1u64) << 80;
        let huge_tag = Value::Const(Constant::Integer(huge_tag_value.clone()));
        for variant in [
            SemanticsVariant::A,
            SemanticsVariant::B,
            SemanticsVariant::C,
        ] {
            let neg = run_variant(
                BuiltinId::ConstrData,
                vec![neg_tag.clone(), fields()],
                variant,
            )
            .unwrap_or_else(|e| panic!("variant {variant:?} must accept a negative tag: {e:?}"));
            assert_eq!(neg, data_val(Data::Constr(BigInt::from(-1), vec![])));

            let huge = run_variant(
                BuiltinId::ConstrData,
                vec![huge_tag.clone(), fields()],
                variant,
            )
            .unwrap_or_else(|e| panic!("variant {variant:?} must accept an oversized tag: {e:?}"));
            assert_eq!(huge, data_val(Data::Constr(huge_tag_value.clone(), vec![])));

            // The wide tag round-trips through unConstrData as a wide
            // Integer (not truncated/clamped to u64).
            let unpacked = run(BuiltinId::UnConstrData, vec![huge]).unwrap();
            if let Value::Const(Constant::ProtoPair { a, .. }) = unpacked {
                assert_eq!(*a, Constant::Integer(huge_tag_value.clone()));
            } else {
                panic!("expected ProtoPair from unConstrData");
            }
        }
    }

    #[test]
    fn un_constr_data_unpacks() {
        let d = data_val(Data::Constr(
            BigInt::from(3),
            vec![Data::I(BigInt::from(1))],
        ));
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
        let d_constr = data_val(Data::Constr(BigInt::from(0), vec![]));
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
        let d = data_val(Data::Constr(
            BigInt::from(0),
            vec![Data::I(BigInt::from(42))],
        ));
        let v = run(BuiltinId::SerialiseData, vec![d]).unwrap();
        // The result is the CBOR encoding of Constr 0 [I 42]
        if let Value::Const(Constant::ByteString(bytes)) = v {
            // CBOR: tag 121 + array(1) + 42 (which is 0x18 0x2a)
            assert_eq!(bytes[0..2], [0xd8, 0x79]); // tag 121 (0x79 = 121)
        } else {
            panic!("expected ByteString");
        }
    }

    /// `serialiseData` is, in Haskell, the *structural canonical re-encode*
    /// `BSL.toStrict . serialise` (NOT a memoised verbatim copy of the
    /// on-chain bytes). A non-empty Constr's args array is therefore
    /// rendered with cborg's indefinite-length list framing
    /// (`0x9f … 0xff`), and an empty one with the definite `0x80`.
    ///
    /// This test runs the real `SerialiseData` builtin on a *machine-
    /// constructed* `Data` (no memo could exist) and asserts the
    /// indefinite framing — guarding against a regression to definite
    /// arrays (the original suspected #15 bug) AND against a "return the
    /// verbatim input bytes" memo implementation.
    #[test]
    fn serialise_data_uses_indefinite_arrays_for_nonempty_constr() {
        // Constr 1 [ Constr 0 [ B 0xab, I 7 ] ]
        let d = data_val(Data::Constr(
            BigInt::from(1),
            vec![Data::Constr(
                BigInt::from(0),
                vec![Data::B(vec![0xab]), Data::I(BigInt::from(7))],
            )],
        ));
        let v = run(BuiltinId::SerialiseData, vec![d]).unwrap();
        let Value::Const(Constant::ByteString(bytes)) = v else {
            panic!("expected ByteString");
        };
        assert_eq!(hex::encode(&bytes), "d87a9fd8799f41ab07ffff");
    }

    /// Gold byte-exact regression for divergence #15 (preprod PlutusV3
    /// script 7afbde08, tx 27751ab9): the script computes
    /// `blake2b256(serialiseData datum) == datum_hash`. The real on-chain
    /// datum (276 bytes, 8 indefinite-length arrays, prefix
    /// `d87a9fd8799f…`) hashes to the on-chain datum_hash
    /// `bbd352028feffe9a80a2822b46b9858bc1cf883cff383e1191b47d27ed708eb0`.
    ///
    /// We feed the *structurally-decoded* datum into the real
    /// `SerialiseData` builtin and assert `blake2b256` of the result equals
    /// that datum_hash. This proves dugite's `serialiseData` is byte-exact
    /// with cardano-node for the exact failing tx — without any memoisation
    /// of original bytes.
    ///
    /// Datum bytes sourced from Koios preprod `/datum_info`
    /// (datum_hash bbd352…, creation tx d653e369…).
    #[test]
    fn serialise_data_gold_preprod_datum_hash_matches_onchain() {
        use crate::data::Data;
        const GOLD_DATUM_HEX: &str = "d87a9fd8799fd8799fd8799f581c9929c128c357ff9b7bdd79ee69d3540e87da001777f15a4c914928dcffd87a80ff1a017d78401a004c4b401a0243d5801b0000019e5bf806edd8799fd8799f581c43d7590ef124ba849222553b19fb84d056a7306dbcfec925002896f3ffd87a80ff58205afe303b6b0feae7632926b07e73921978d8fa7f02ca358a8676de1d3381b89c582097450e7fc42aa1f45e9f0abda20d32024bc40c3351a390ec409a42951657b2c858201dec2a9a7014a1fa0ae3b3fb7d8b483e5f40c427627d8b1c73f3fec282904d62581c57a437cbed5709a2214d40bdf44eb08d1b88e97967798d83ec774fb6581c535e4be12d936e564b44b33618f2ae55090b1ac0f3be37ef8beb60e7ffff";
        const ON_CHAIN_DATUM_HASH: &str =
            "bbd352028feffe9a80a2822b46b9858bc1cf883cff383e1191b47d27ed708eb0";

        let datum_bytes = hex::decode(GOLD_DATUM_HEX).unwrap();
        // Decode structurally (mirrors how a datum reaches the script as a
        // `Constant::Data` value) — discards any "original bytes" channel.
        let d = Data::from_cbor(&datum_bytes).unwrap();

        // Run the actual builtin.
        let v = run(
            BuiltinId::SerialiseData,
            vec![Value::Const(Constant::Data(d))],
        )
        .unwrap();
        let Value::Const(Constant::ByteString(serialised)) = v else {
            panic!("expected ByteString");
        };

        // serialiseData must reproduce the on-chain bytes exactly …
        assert_eq!(
            hex::encode(&serialised),
            GOLD_DATUM_HEX,
            "serialiseData(datum) must equal the on-chain datum bytes"
        );
        // … hence blake2b256 must equal the on-chain datum_hash.
        let digest = {
            use blake2::Digest;
            blake2::Blake2b::<blake2::digest::consts::U32>::digest(&serialised).to_vec()
        };
        assert_eq!(
            hex::encode(digest),
            ON_CHAIN_DATUM_HASH,
            "blake2b256(serialiseData datum) must equal the on-chain datum_hash"
        );
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

    /// p = 2^255 - 19, little-endian 32-byte encoding with the sign-bit
    /// slot cleared. Used to hand-construct the boundary vectors below.
    fn p_le_bytes() -> [u8; 32] {
        let mut p = [0xffu8; 32];
        p[0] = 0xed;
        p[31] = 0x7f;
        p
    }

    #[test]
    fn pk_is_canonical_boundary_vectors() {
        // 0 is canonical (magnitude 0 < p).
        assert!(pk_is_canonical(&[0u8; 32]));

        // p - 1 is canonical (the largest canonical magnitude).
        let mut p_minus_1 = p_le_bytes();
        p_minus_1[0] -= 1;
        assert!(pk_is_canonical(&p_minus_1));

        // p itself is NOT canonical (magnitude >= p).
        assert!(!pk_is_canonical(&p_le_bytes()));

        // p + 1 is NOT canonical.
        let mut p_plus_1 = p_le_bytes();
        p_plus_1[0] += 1; // 0xed + 1 = 0xee, no carry out of byte 0
        assert!(!pk_is_canonical(&p_plus_1));

        // 2^255 - 1 (all high bits set, the maximum 255-bit value) is
        // NOT canonical.
        let all_ones = {
            let mut b = [0xffu8; 32];
            b[31] = 0x7f;
            b
        };
        assert!(!pk_is_canonical(&all_ones));

        // The sign bit (top bit of byte 31) must be masked off before
        // comparison: setting it on an otherwise-canonical value must
        // not change the verdict.
        let mut zero_with_sign_bit = [0u8; 32];
        zero_with_sign_bit[31] = 0x80;
        assert!(pk_is_canonical(&zero_with_sign_bit));
    }

    #[test]
    fn verify_ed25519_rejects_all_19_noncanonical_pk_aliases() {
        // The 19 raw byte patterns whose 255-bit magnitude is in
        // [p, 2^255-1] = [p, p+18] (y_actual = 0..=18) are exactly the
        // "p+k" aliasing class libsodium's `ge25519_is_canonical`
        // rejects unconditionally, before point decompression is even
        // attempted. `ed25519-dalek`'s permissive decompress would
        // otherwise silently reduce these mod p and (for the subset
        // that land on an ordinary, non-small-order point) potentially
        // accept a signature libsodium/cardano-node reject outright.
        // The message/signature bytes are irrelevant here: canonicity
        // must be rejected before any curve arithmetic runs.
        let msg = b"whatever";
        let sig = [0u8; 64];
        for y_actual in 0u8..=18 {
            let mut pk = p_le_bytes();
            // p_le_bytes() + y_actual: p's low byte is 0xed, so adding
            // y_actual in 0..=18 never carries out of byte 0 (0xed+18 =
            // 0xff) and never touches the sign bit.
            pk[0] += y_actual;
            assert!(
                !pk_is_canonical(&pk),
                "y_actual={y_actual} should be non-canonical"
            );
            let v = run(
                BuiltinId::VerifyEd25519Signature,
                vec![bs(&pk), bs(msg), bs(&sig)],
            )
            .unwrap();
            assert_eq!(
                v,
                b(false),
                "non-canonical pk alias (y_actual={y_actual}) must verify to False"
            );
        }
    }

    #[test]
    fn verify_ed25519_canonical_pk_still_verifies_true() {
        // Regression: the new canonicity gate must not disturb the
        // existing byte-exact RFC 8032 vector (a fully canonical
        // public key, well below p).
        let pk = hex::decode("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a")
            .unwrap();
        assert!(pk.len() >= 32);
        let pk32: [u8; 32] = pk[..32].try_into().unwrap();
        assert!(pk_is_canonical(&pk32));
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
        // `run` uses the STRICT / latest variant (V3 semantics): out-of-range
        // integers are a BuiltinFailure.
        assert!(matches!(
            run(BuiltinId::ConsByteString, vec![int(256), bs(b"")]),
            Err(UplcError::BuiltinFailure { .. })
        ));
        assert!(matches!(
            run(BuiltinId::ConsByteString, vec![int(-1), bs(b"")]),
            Err(UplcError::BuiltinFailure { .. })
        ));
    }

    // ── consByteString: per-SemanticsVariant denotation (backlog #32) ──────
    //
    // LENIENT (V1/V2 variants A/B/D): `BS.cons . fromIntegral` — reduce the
    // integer mod 256 (Euclidean) to a byte; NEVER errors.
    // STRICT (V3 variants C/E): `Word8` arg — error outside 0..=255.

    #[test]
    fn cons_byte_string_lenient_wraps_modulo_256() {
        use SemanticsVariant::A;
        // 256 → 0x00
        assert_eq!(
            run_variant(BuiltinId::ConsByteString, vec![int(256), bs(b"")], A).unwrap(),
            bs(&[0x00])
        );
        // 257, "x" → [0x01, 0x78]
        assert_eq!(
            run_variant(BuiltinId::ConsByteString, vec![int(257), bs(b"x")], A).unwrap(),
            bs(&[0x01, 0x78])
        );
        // 511 → 0xFF
        assert_eq!(
            run_variant(BuiltinId::ConsByteString, vec![int(511), bs(b"")], A).unwrap(),
            bs(&[0xFF])
        );
        // 0, "ab" → [0x00, 0x61, 0x62] (in-range value, byte-identical to strict)
        assert_eq!(
            run_variant(BuiltinId::ConsByteString, vec![int(0), bs(b"ab")], A).unwrap(),
            bs(&[0x00, 0x61, 0x62])
        );
    }

    #[test]
    fn cons_byte_string_lenient_handles_negatives_via_rem_euclid() {
        // The sign guard: num-bigint `%` would give -1 % 256 == -1 (WRONG);
        // `rem_euclid` gives 255 → 0xFF.
        for variant in [
            SemanticsVariant::A,
            SemanticsVariant::B,
            SemanticsVariant::D,
        ] {
            assert_eq!(
                run_variant(BuiltinId::ConsByteString, vec![int(-1), bs(b"")], variant).unwrap(),
                bs(&[0xFF]),
                "{variant:?}: -1 must wrap to 0xFF",
            );
            // -256 → 0x00
            assert_eq!(
                run_variant(BuiltinId::ConsByteString, vec![int(-256), bs(b"")], variant).unwrap(),
                bs(&[0x00]),
                "{variant:?}: -256 must wrap to 0x00",
            );
        }
    }

    #[test]
    fn cons_byte_string_strict_v3_rejects_out_of_range() {
        for variant in [SemanticsVariant::C, SemanticsVariant::E] {
            assert!(
                matches!(
                    run_variant(BuiltinId::ConsByteString, vec![int(256), bs(b"")], variant),
                    Err(UplcError::BuiltinFailure { .. })
                ),
                "{variant:?}: 256 must error under strict semantics",
            );
            assert!(
                matches!(
                    run_variant(BuiltinId::ConsByteString, vec![int(-1), bs(b"")], variant),
                    Err(UplcError::BuiltinFailure { .. })
                ),
                "{variant:?}: -1 must error under strict semantics",
            );
            // In-range value still succeeds.
            assert_eq!(
                run_variant(BuiltinId::ConsByteString, vec![int(255), bs(b"")], variant).unwrap(),
                bs(&[0xFF]),
                "{variant:?}: 255 must succeed → 0xFF",
            );
        }
    }

    #[test]
    fn cons_byte_string_lenient_equals_strict_for_in_range_bytes() {
        // Regression guard: for EVERY in-range integer (0..=255) the lenient
        // and strict paths produce byte-identical output. This is what keeps
        // the 999-vector conformance suite and the in-range #730 dumps
        // unchanged.
        for i in 0u16..=255 {
            let lenient = run_variant(
                BuiltinId::ConsByteString,
                vec![int(i as i64), bs(b"z")],
                SemanticsVariant::A,
            )
            .unwrap();
            let strict = run_variant(
                BuiltinId::ConsByteString,
                vec![int(i as i64), bs(b"z")],
                SemanticsVariant::E,
            )
            .unwrap();
            assert_eq!(lenient, strict, "lenient != strict for in-range byte {i}");
            assert_eq!(lenient, bs(&[i as u8, b'z']));
        }
    }

    // ── writeBits: maximumInputLength cap (backlog #33) ─────────────────────
    //
    // Plutus `writeBits` caps the INPUT bytestring at maximumInputLength = 4096
    // bytes (Bitwise.hs:81-83). The guard `BS.length b > 4096 ->
    // builtinResultFailure` (Builtins.hs:2191-2221) fires on the INPUT length
    // BEFORE the per-index out-of-bounds checks, and is gated by `ensurable`
    // (true for D/E only). A/B/C impose NO cap.

    /// Build a `ProtoList Integer` of indices for the `writeBits` indices arg.
    fn idx_list(indices: &[i64]) -> Value {
        Value::Const(Constant::ProtoList {
            elem_type: crate::term::TypeTag::Integer,
            elements: indices
                .iter()
                .map(|&i| Constant::Integer(BigInt::from(i)))
                .collect(),
        })
    }

    #[test]
    fn write_bits_over_length_rejected_under_de() {
        // 4097-byte input, empty indices, value=true: variants D and E enforce
        // the 4096-byte cap → BuiltinFailure.
        let over = bs(&vec![0u8; 4097]);
        for v in [SemanticsVariant::D, SemanticsVariant::E] {
            assert!(
                matches!(
                    run_variant(
                        BuiltinId::WriteBits,
                        vec![over.clone(), idx_list(&[]), b(true)],
                        v,
                    ),
                    Err(UplcError::BuiltinFailure { .. })
                ),
                "expected over-length writeBits to fail under {v:?}"
            );
        }
    }

    #[test]
    fn write_bits_over_length_accepted_under_c() {
        // Variant C (V3 < plominPV) imposes NO cap → over-length is fine.
        let over = bs(&vec![0u8; 4097]);
        assert!(matches!(
            run_variant(
                BuiltinId::WriteBits,
                vec![over, idx_list(&[]), b(true)],
                SemanticsVariant::C,
            ),
            Ok(Value::Const(Constant::ByteString(_)))
        ));
    }

    #[test]
    fn write_bits_boundary_4096() {
        // The cap is `>` not `>=`: exactly 4096 bytes is accepted under E, 4097
        // is rejected.
        let at = bs(&vec![0u8; 4096]);
        assert!(matches!(
            run_variant(
                BuiltinId::WriteBits,
                vec![at, idx_list(&[]), b(true)],
                SemanticsVariant::E,
            ),
            Ok(Value::Const(Constant::ByteString(_)))
        ));
        let over = bs(&vec![0u8; 4097]);
        assert!(matches!(
            run_variant(
                BuiltinId::WriteBits,
                vec![over, idx_list(&[]), b(true)],
                SemanticsVariant::E,
            ),
            Err(UplcError::BuiltinFailure { .. })
        ));
    }

    #[test]
    fn write_bits_normal_identical_c_e() {
        // A normal 32-byte input with a valid index produces byte-identical
        // output under C (no cap) and E (cap, but input is well under it).
        let input = bs(&[0u8; 32]);
        let under_c = run_variant(
            BuiltinId::WriteBits,
            vec![input.clone(), idx_list(&[5]), b(true)],
            SemanticsVariant::C,
        )
        .unwrap();
        let under_e = run_variant(
            BuiltinId::WriteBits,
            vec![input, idx_list(&[5]), b(true)],
            SemanticsVariant::E,
        )
        .unwrap();
        assert_eq!(under_c, under_e);
        // bit 5 set in the LAST byte (CIP-122 ordering): byte 31 = 0b0010_0000.
        let mut expect = vec![0u8; 32];
        expect[31] = 0b0010_0000;
        assert_eq!(under_e, bs(&expect));
    }

    #[test]
    fn write_bits_length_guard_precedes_index() {
        // Over-length input with an IN-RANGE index under E still fails on the
        // length cap (which precedes the per-index check) — NOT on the index.
        let over = bs(&vec![0u8; 4097]);
        // index 0 is in range (4097*8 bits available).
        assert!(matches!(
            run_variant(
                BuiltinId::WriteBits,
                vec![over, idx_list(&[0]), b(true)],
                SemanticsVariant::E,
            ),
            Err(UplcError::BuiltinFailure { .. })
        ));
    }

    #[test]
    fn write_bits_empty_indices_over_length_under_e() {
        // Empty indices + over-length input under E → the length guard fires
        // even though there are no indices to check.
        let over = bs(&vec![0u8; 4097]);
        assert!(matches!(
            run_variant(
                BuiltinId::WriteBits,
                vec![over, idx_list(&[]), b(true)],
                SemanticsVariant::E,
            ),
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
    fn slice_byte_string_fails_on_out_of_int64_args() {
        // #828.2: `sliceByteString`'s `start`/`length` are unlifted as
        // Haskell `Int` (bounds-checked against `Int64`), NOT plain
        // `Integer` — a value outside `[-2^63, 2^63-1]` is an evaluation
        // FAILURE at unlifting, never a silent clamp to 0 / usize::MAX.
        let bs7 = bs(b"abcdefg");
        let too_big = Value::Const(Constant::Integer(BigInt::from(i64::MAX) + 1));
        let too_small = Value::Const(Constant::Integer(BigInt::from(i64::MIN) - 1));

        assert!(
            matches!(
                run(
                    BuiltinId::SliceByteString,
                    vec![too_big.clone(), int(1), bs7.clone()]
                ),
                Err(UplcError::BuiltinFailure { .. })
            ),
            "start beyond Int64::MAX must fail, not clamp"
        );
        assert!(
            matches!(
                run(
                    BuiltinId::SliceByteString,
                    vec![int(0), too_big, bs7.clone()]
                ),
                Err(UplcError::BuiltinFailure { .. })
            ),
            "length beyond Int64::MAX must fail, not clamp"
        );
        assert!(
            matches!(
                run(
                    BuiltinId::SliceByteString,
                    vec![too_small, int(1), bs7.clone()]
                ),
                Err(UplcError::BuiltinFailure { .. })
            ),
            "start below Int64::MIN must fail, not clamp"
        );

        // A value AT the Int64 boundary still succeeds and clamps normally
        // (the fix only rejects values OUTSIDE Int64, not extreme-but-valid
        // ones).
        assert_eq!(
            run(BuiltinId::SliceByteString, vec![int(i64::MAX), int(3), bs7]).unwrap(),
            bs(b"")
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

    // ── #844: countSetBits u64 accumulator ─────────────────────────

    #[test]
    fn count_set_bits_basic() {
        // 0xFF has 8 set bits per byte.
        assert_eq!(
            run(BuiltinId::CountSetBits, vec![bs(&[0xFF, 0x00, 0x0F])]).unwrap(),
            int(12)
        );
    }

    #[test]
    fn count_set_bits_moderate_size_does_not_overflow_u32_style_accumulator() {
        // 100_000 bytes of 0xFF = 800_000 set bits — comfortably inside a
        // `u32` (this alone would not have caught the pre-fix bug, which
        // only manifests past `u32::MAX` / 8 bytes of all-ones; a fixture
        // that large is impractical for a unit test). This test pins the
        // `u64`-accumulator code path stays correct for a realistic size.
        let big = vec![0xFFu8; 100_000];
        assert_eq!(
            run(BuiltinId::CountSetBits, vec![bs(&big)]).unwrap(),
            int(800_000)
        );
    }

    // ── #844: shiftByteString wide early-out before usize narrowing ─

    #[test]
    fn shift_byte_string_large_shift_returns_all_zeros() {
        // A shift far beyond the bit length must return all-zeros via the
        // wide (BigInt) `>= len_bits` comparison, not fail while
        // narrowing to `usize` (only observable on a 32-bit host, but the
        // all-zeros result must hold on every host).
        let input = bs(&[0xFF, 0xFF]);
        let huge_shift = int(1_000_000_000);
        assert_eq!(
            run(BuiltinId::ShiftByteString, vec![input, huge_shift]).unwrap(),
            bs(&[0x00, 0x00])
        );
    }

    #[test]
    fn shift_byte_string_negative_large_shift_returns_all_zeros() {
        let input = bs(&[0xFF, 0xFF]);
        let huge_negative_shift = int(-1_000_000_000);
        assert_eq!(
            run(BuiltinId::ShiftByteString, vec![input, huge_negative_shift]).unwrap(),
            bs(&[0x00, 0x00])
        );
    }
}
