//! Builtin dispatch glue.
//!
//! Wires the CEK machine's `Term::Builtin(id)` and `Value::Builtin`
//! reduction rules to the per-builtin denotation table.
//!
//! The CEK machine emits a `Value::Builtin { id, forces: 0, args: [] }`
//! when it first sees a `Term::Builtin(id)`. Each subsequent `Force`
//! bumps `forces`; each `Apply` bumps `args`. When the count of
//! forces equals the builtin's required forces AND the count of args
//! equals the builtin's required arity, the denotation fires.

use crate::builtin::arity::arity_of;
use crate::builtin::denotations::denote;
use crate::builtin::semantics::SemanticsVariant;
use crate::machine::cost::BudgetTracker;
use crate::machine::value::Value;
use crate::term::BuiltinId;
use crate::UplcError;

/// Result of accepting another `Force` against a `Value::Builtin`.
#[derive(Debug)]
pub enum ForceOutcome {
    /// Builtin remains under-applied. Continue accumulating.
    Pending(Value),
    /// Builtin is fully saturated (forces and args satisfied); the
    /// denotation has fired and produced a result.
    Done(Value),
    /// The script tried to force a builtin that has already received
    /// its required force count. This is the Haskell
    /// `BuiltinTermArgumentExpected` failure — a script-level error.
    Excess,
}

/// Apply a `Force` to a `Value::Builtin`. Bumps `forces` if more are
/// required by the builtin's arity table; otherwise reports excess.
///
/// When `tracker` is `Some`, the per-builtin cost is charged against
/// the budget the moment the denotation fires (mirrors the Haskell
/// reference's `chargeAndRun` semantics — cost is charged BEFORE the
/// denotation's computation, never after).
///
/// `trace_log` is threaded through to the denotation for the `Trace`
/// builtin to append its string argument.
///
/// `variant` is the script's [`SemanticsVariant`], forwarded to the
/// denotation for the builtins whose result depends on it (currently
/// `consByteString`).
pub fn force_builtin(
    id: BuiltinId,
    forces: u8,
    args: Vec<Value>,
    tracker: Option<&mut BudgetTracker>,
    trace_log: Option<&mut Vec<String>>,
    variant: SemanticsVariant,
) -> Result<ForceOutcome, UplcError> {
    let (required_forces, required_arity) = arity_of(id);
    if forces < required_forces {
        let new_forces = forces + 1;
        let v = Value::Builtin {
            id,
            forces: new_forces,
            args,
        };
        // If the builtin takes zero value arguments, accumulating the
        // final force fires the denotation immediately.
        if new_forces == required_forces && required_arity == 0 {
            if let Some(t) = tracker {
                let cost = t.builtin_costs.charge_for_args(id, &[]);
                t.charge(cost)?;
            }
            let result = denote(id, vec![], trace_log, variant)?;
            return Ok(ForceOutcome::Done(result));
        }
        Ok(ForceOutcome::Pending(v))
    } else {
        Ok(ForceOutcome::Excess)
    }
}

/// Apply a value-argument to a `Value::Builtin`. Bumps `args` if
/// more are required; if the saturated count is reached AND all
/// forces are also accounted for, the denotation fires.
///
/// When `tracker` is `Some`, the per-builtin cost is charged at
/// saturation (see [`force_builtin`] for the same semantics).
///
/// `trace_log` is threaded through to the denotation for the `Trace`
/// builtin to append its string argument.
///
/// `variant` is the script's [`SemanticsVariant`], forwarded to the
/// denotation for the builtins whose result depends on it (currently
/// `consByteString`).
pub fn apply_builtin(
    id: BuiltinId,
    forces: u8,
    mut args: Vec<Value>,
    arg: Value,
    tracker: Option<&mut BudgetTracker>,
    trace_log: Option<&mut Vec<String>>,
    variant: SemanticsVariant,
) -> Result<Value, UplcError> {
    let (required_forces, required_arity) = arity_of(id);
    if forces < required_forces {
        // The script applied an argument before satisfying the
        // required force count. Haskell calls this
        // `BuiltinTermArgumentExpected`. Surface as a typed
        // builtin-type error so the script halts cleanly.
        return Err(UplcError::BuiltinTypeError {
            builtin: builtin_name_static(id),
            reason: format!(
                "argument applied before required force count satisfied \
                 (got {forces} forces, want {required_forces})"
            ),
        });
    }
    args.push(arg);
    let arg_count = args.len() as u8;
    if arg_count < required_arity {
        return Ok(Value::Builtin { id, forces, args });
    }
    if arg_count > required_arity {
        // Should be unreachable given the per-application apply path,
        // but surface as a typed error rather than silently dropping
        // the surplus argument.
        return Err(UplcError::BuiltinTypeError {
            builtin: builtin_name_static(id),
            reason: format!(
                "over-application: builtin has arity {required_arity} \
                 but received {arg_count} arguments"
            ),
        });
    }
    // Saturated: charge the per-builtin cost first (Haskell mirrors
    // this order: chargeAndRun fires cost BEFORE the denotation), then
    // invoke the denotation.
    if let Some(t) = tracker {
        let cost = t.builtin_costs.charge_for_args(id, &args);
        record_builtin_charge(id, cost);
        t.charge(cost)?;
    }
    denote(id, args, trace_log, variant)
}

/// Return a `'static str` identifier for the builtin (alias for the
/// `name()` method on `BuiltinId`). Kept as a thin wrapper so error
/// constructors don't carry the full `BuiltinId` import everywhere.
fn builtin_name_static(id: BuiltinId) -> &'static str {
    id.name()
}

// ─── Diagnostic per-builtin charge trace (DUGITE_UPLC_BUILTIN_TRACE) ──────────
//
// When the env var is set, every per-builtin charge is accumulated by builtin
// name on the evaluating thread. `take_builtin_trace` drains the aggregates so
// an offline tool (e.g. `replay_phase2`) can localize a phase-2 mem/cpu
// accounting divergence to a specific builtin. Zero overhead when disabled
// (one cached atomic load per call).
thread_local! {
    static BUILTIN_TRACE: std::cell::RefCell<std::collections::HashMap<&'static str, (i64, i64, u64)>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

fn builtin_trace_enabled() -> bool {
    static E: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *E.get_or_init(|| std::env::var("DUGITE_UPLC_BUILTIN_TRACE").is_ok())
}

#[inline]
fn record_builtin_charge(id: BuiltinId, cost: crate::machine::cost::ExBudget) {
    if !builtin_trace_enabled() {
        return;
    }
    BUILTIN_TRACE.with(|m| {
        let mut m = m.borrow_mut();
        let e = m.entry(id.name()).or_insert((0, 0, 0));
        e.0 = e.0.saturating_add(cost.cpu);
        e.1 = e.1.saturating_add(cost.mem);
        e.2 += 1;
    });
}

/// Drain the per-builtin charge aggregates `(name, total_cpu, total_mem,
/// invocations)` accumulated on this thread when `DUGITE_UPLC_BUILTIN_TRACE`
/// is set, sorted by total mem descending. Diagnostic only.
pub fn take_builtin_trace() -> Vec<(&'static str, i64, i64, u64)> {
    BUILTIN_TRACE.with(|m| {
        let mut v: Vec<(&'static str, i64, i64, u64)> = m
            .borrow()
            .iter()
            .map(|(k, (c, mm, n))| (*k, *c, *mm, *n))
            .collect();
        m.borrow_mut().clear();
        v.sort_by_key(|b| std::cmp::Reverse(b.2));
        v
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::Constant;
    use num_bigint::BigInt;

    fn int_val(n: i64) -> Value {
        Value::Const(Constant::Integer(BigInt::from(n)))
    }

    #[test]
    fn force_zero_force_builtin_returns_excess_on_first_force() {
        // AddInteger needs 0 forces. The first force is already excess.
        match force_builtin(
            BuiltinId::AddInteger,
            0,
            vec![],
            None,
            None,
            SemanticsVariant::LATEST,
        )
        .unwrap()
        {
            ForceOutcome::Excess => {}
            other => panic!("expected Excess, got {other:?}"),
        }
    }

    #[test]
    fn force_one_force_builtin_becomes_pending_then_excess() {
        // IfThenElse needs 1 force.
        let v1 = match force_builtin(
            BuiltinId::IfThenElse,
            0,
            vec![],
            None,
            None,
            SemanticsVariant::LATEST,
        )
        .unwrap()
        {
            ForceOutcome::Pending(v) => v,
            other => panic!("expected Pending, got {other:?}"),
        };
        match v1 {
            Value::Builtin { id, forces, .. } => {
                assert_eq!(id, BuiltinId::IfThenElse);
                assert_eq!(forces, 1);
            }
            _ => panic!("expected Value::Builtin"),
        }
        // Forcing again is excess.
        match force_builtin(
            BuiltinId::IfThenElse,
            1,
            vec![],
            None,
            None,
            SemanticsVariant::LATEST,
        )
        .unwrap()
        {
            ForceOutcome::Excess => {}
            other => panic!("expected Excess, got {other:?}"),
        }
    }

    #[test]
    fn applying_before_forces_satisfied_errors() {
        // IfThenElse needs 1 force; applying an arg first is a
        // builtin-type error.
        let err = apply_builtin(
            BuiltinId::IfThenElse,
            0,
            vec![],
            int_val(1),
            None,
            None,
            SemanticsVariant::LATEST,
        )
        .unwrap_err();
        assert!(matches!(err, UplcError::BuiltinTypeError { .. }));
    }

    #[test]
    fn apply_under_arity_returns_partial() {
        // AddInteger arity (0, 2) — applying just one arg is still partial.
        let v = apply_builtin(
            BuiltinId::AddInteger,
            0,
            vec![],
            int_val(1),
            None,
            None,
            SemanticsVariant::LATEST,
        )
        .unwrap();
        match v {
            Value::Builtin { id, args, .. } => {
                assert_eq!(id, BuiltinId::AddInteger);
                assert_eq!(args.len(), 1);
            }
            _ => panic!("expected Builtin"),
        }
    }
}
