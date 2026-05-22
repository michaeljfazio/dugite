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

        // Anything else is a future commit.
        _ => Err(UplcError::Internal(format!(
            "builtin denotation for {} not yet wired",
            id.name()
        ))),
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
        // sha2_256 (a hash builtin) not wired yet.
        let err = run(BuiltinId::Sha2_256, vec![]).unwrap_err();
        assert!(matches!(err, UplcError::Internal(_)));
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
