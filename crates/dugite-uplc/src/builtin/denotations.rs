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

        // Anything else is a future commit.
        _ => Err(UplcError::Internal(format!(
            "builtin denotation for {} not yet wired",
            id.name()
        ))),
    }
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
        // ByteString builtin not wired yet.
        let err = run(BuiltinId::AppendByteString, vec![]).unwrap_err();
        assert!(matches!(err, UplcError::Internal(_)));
    }
}
