//! CEK step driver — minimal subset of UPLC-3.

use crate::machine::context::{Frame, Kont};
use crate::machine::env::Env;
use crate::machine::value::Value;
use crate::term::Term;
use crate::UplcError;

#[derive(Debug)]
pub enum State {
    Compute { term: Term, env: Env, kont: Kont },
    Return { value: Value, kont: Kont },
    Done(Value),
}

pub fn evaluate(term: Term) -> Result<Value, UplcError> {
    let mut state = State::Compute {
        term,
        env: Env::new(),
        kont: Kont::new(),
    };
    loop {
        state = step(state, None, None)?;
        if let State::Done(v) = state {
            return Ok(v);
        }
    }
}

/// Evaluate with explicit budget tracking. Charges the per-term-type
/// CEK machine cost on each `Compute` transition (mirroring the
/// Haskell reference's `enterComputeCek`) and the per-builtin cost at
/// each saturated builtin application; `Return` transitions are
/// otherwise free.  Startup cost is charged once by
/// `BudgetTracker::new`.  On success, `tracker.consumed()` reports the
/// budget actually used.
///
/// `trace_log`, when `Some`, receives every string emitted by the
/// `Trace` builtin during evaluation, in emission order (FIFO). This
/// mirrors the Haskell CEK `emit` mechanism. Pass `None` to discard
/// trace output (e.g. when running the conformance suite without log
/// capture).
pub fn evaluate_with_budget(
    term: Term,
    tracker: &mut crate::machine::cost::BudgetTracker,
    mut trace_log: Option<&mut Vec<String>>,
) -> Result<Value, UplcError> {
    let mut state = State::Compute {
        term,
        env: Env::new(),
        kont: Kont::new(),
    };
    loop {
        if let State::Compute { ref term, .. } = state {
            tracker.compute_step(term)?;
        }
        state = step(state, Some(&mut *tracker), trace_log.as_deref_mut())?;
        if let State::Done(v) = state {
            tracker.flush()?;
            return Ok(v);
        }
    }
}

fn step(
    state: State,
    tracker: Option<&mut crate::machine::cost::BudgetTracker>,
    trace_log: Option<&mut Vec<String>>,
) -> Result<State, UplcError> {
    match state {
        State::Compute { term, env, kont } => compute(term, env, kont),
        State::Return { value, kont } => return_compute(value, kont, tracker, trace_log),
        State::Done(_) => Err(UplcError::Internal("step called on Done state".into())),
    }
}

fn compute(term: Term, env: Env, mut kont: Kont) -> Result<State, UplcError> {
    match term {
        Term::Var(i) => {
            let v = env.lookup(i)?.clone();
            Ok(State::Return { value: v, kont })
        }
        Term::Lam(body) => {
            let v = Value::Lambda { body, env };
            Ok(State::Return { value: v, kont })
        }
        Term::App(fun, arg) => {
            kont.push(Frame::AwaitFunTerm {
                argument: *arg,
                env: env.clone(),
            })?;
            Ok(State::Compute {
                term: *fun,
                env,
                kont,
            })
        }
        Term::Const(c) => Ok(State::Return {
            value: Value::Const(c),
            kont,
        }),
        Term::Delay(body) => Ok(State::Return {
            value: Value::Delay { body, env },
            kont,
        }),
        Term::Force(body) => {
            kont.push(Frame::Force)?;
            Ok(State::Compute {
                term: *body,
                env,
                kont,
            })
        }
        Term::Error => Err(UplcError::ScriptError),
        Term::Builtin(id) => {
            // Emit a partially-applied builtin value; the dispatcher
            // accumulates forces / args as subsequent `Force`s and
            // `Apply`s occur, then fires the denotation when the
            // builtin's arity is satisfied. A zero-force / zero-arity
            // builtin would fire immediately, but no such builtin
            // exists on the wire.
            let (required_forces, required_arity) = crate::builtin::arity::arity_of(id);
            let v = Value::Builtin {
                id,
                forces: 0,
                args: Vec::new(),
            };
            // Defensive check: a zero-force, zero-arity builtin would
            // be a value already and should fire its denotation here.
            // No such on-chain builtin exists, but we surface the
            // hypothetical via a typed error rather than skipping.
            if required_forces == 0 && required_arity == 0 {
                return Err(UplcError::Internal(format!(
                    "builtin {} has zero forces+arity; no on-chain \
                     denotation should match this shape",
                    id.name()
                )));
            }
            Ok(State::Return { value: v, kont })
        }
        Term::Constr { tag, args } => {
            // No args: short-circuit to a fully-evaluated Constr value.
            if args.is_empty() {
                return Ok(State::Return {
                    value: Value::Constr {
                        tag,
                        args: Vec::new(),
                    },
                    kont,
                });
            }
            // Otherwise: push a Constr frame and evaluate the first arg.
            let mut pending: Vec<Term> = args.into_iter().rev().collect();
            // SAFETY: just confirmed args.is_empty() == false, so pop
            // is Some. Surfaced as Internal for the no-panic invariant.
            let first = pending.pop().ok_or_else(|| {
                UplcError::Internal("Constr args empty after non-empty check".into())
            })?;
            kont.push(Frame::Constr {
                tag,
                pending,
                evaluated: Vec::new(),
                env: env.clone(),
            })?;
            Ok(State::Compute {
                term: first,
                env,
                kont,
            })
        }
        Term::Case {
            scrutinee,
            branches,
        } => {
            // Evaluate the scrutinee; save the branches for the
            // `Cases` frame to dispatch on once the scrutinee value
            // (a `Constr`) lands.
            kont.push(Frame::Cases {
                branches,
                env: env.clone(),
            })?;
            Ok(State::Compute {
                term: *scrutinee,
                env,
                kont,
            })
        }
    }
}

fn return_compute(
    value: Value,
    mut kont: Kont,
    tracker: Option<&mut crate::machine::cost::BudgetTracker>,
    trace_log: Option<&mut Vec<String>>,
) -> Result<State, UplcError> {
    let frame = match kont.pop() {
        None => return Ok(State::Done(value)),
        Some(f) => f,
    };
    match frame {
        Frame::AwaitFunTerm { argument, env } => {
            kont.push(Frame::AwaitArg {
                function: value,
                env: env.clone(),
            })?;
            Ok(State::Compute {
                term: argument,
                env,
                kont,
            })
        }
        Frame::AwaitArg { function, .. } => apply(function, value, kont, tracker, trace_log),
        Frame::Force => force_value(value, kont, tracker, trace_log),
        Frame::ApplyValue { argument } => apply(value, argument, kont, tracker, trace_log),
        Frame::Constr {
            tag,
            mut pending,
            mut evaluated,
            env,
        } => {
            // We just got an evaluated arg. Append it; if more args
            // remain, evaluate the next one; else emit the Constr
            // value.
            evaluated.push(value);
            if let Some(next) = pending.pop() {
                kont.push(Frame::Constr {
                    tag,
                    pending,
                    evaluated,
                    env: env.clone(),
                })?;
                Ok(State::Compute {
                    term: next,
                    env,
                    kont,
                })
            } else {
                Ok(State::Return {
                    value: Value::Constr {
                        tag,
                        args: evaluated,
                    },
                    kont,
                })
            }
        }
        Frame::Cases { branches, env } => {
            // Plutus 1.1.0 `case` accepts a SOP `Constr` (CIP-0085) or
            // a small set of enumerable `Constant` scrutinees — see
            // the formal spec.  For constants, the scrutinee is
            // re-projected to a `(tag, args)` shape and the matching
            // branch is invoked with `args` applied in order.
            // Plutus 1.1.0 `case`-on-Constant requires the branch
            // count to match the scrutinee's constructor count
            // exactly (or be in-range for Integer).  Mismatched
            // branch counts surface as `ScriptError` per the spec's
            // `MissingCaseBranch` / extra-branch semantics.
            let branch_count = branches.len();
            // For constant scrutinees, `max_branches` caps the
            // branch list length to the number of constructors in
            // the scrutinee's type — extra branches are a script
            // error.  `None` ⇒ no cap (Constr; Integer-indexed).
            let (tag, payload, max_branches): (
                u64,
                Vec<crate::machine::value::Value>,
                Option<usize>,
            ) = match value {
                crate::machine::value::Value::Constr { tag, args } => (tag, args, None),
                crate::machine::value::Value::Const(c) => {
                    use crate::term::Constant;
                    match c {
                        // Bool: False = 0, True = 1.  Exactly 2 branches.
                        Constant::Bool(false) => (0, Vec::new(), Some(2)),
                        Constant::Bool(true) => (1, Vec::new(), Some(2)),
                        // Unit: single branch.
                        Constant::Unit => (0, Vec::new(), Some(1)),
                        // List: branch 0 = Cons (head, tail), branch 1 = Nil.
                        // Exactly 2 branches.
                        Constant::ProtoList {
                            elem_type,
                            elements,
                        } => {
                            if elements.is_empty() {
                                (1, Vec::new(), Some(2))
                            } else {
                                let mut iter = elements.into_iter();
                                let head = iter.next().ok_or_else(|| {
                                    UplcError::Internal("list empty after non-empty check".into())
                                })?;
                                let tail: Vec<Constant> = iter.collect();
                                (
                                    0,
                                    vec![
                                        crate::machine::value::Value::Const(head),
                                        crate::machine::value::Value::Const(Constant::ProtoList {
                                            elem_type,
                                            elements: tail,
                                        }),
                                    ],
                                    Some(2),
                                )
                            }
                        }
                        // Pair: single branch with `(a, b)` payload.
                        Constant::ProtoPair { a, b, .. } => (
                            0,
                            vec![
                                crate::machine::value::Value::Const(*a),
                                crate::machine::value::Value::Const(*b),
                            ],
                            Some(1),
                        ),
                        // Integer: scrutinee value indexes into the
                        // branches list (must be in `0..len`).  Out
                        // of range → ScriptError.
                        Constant::Integer(n) => {
                            use num_traits::ToPrimitive;
                            let idx = n
                                .to_u64()
                                .filter(|i| (*i as usize) < branch_count)
                                .ok_or(UplcError::ScriptError)?;
                            // No required-count constraint — any
                            // non-empty branch list whose length
                            // covers the index is fine.
                            (idx, Vec::new(), None)
                        }
                        // Other constants (ByteString, String, Data,
                        // BLS): not enumerable, `case` fails.
                        other => {
                            return Err(UplcError::Internal(format!(
                                "case on non-enumerable constant: {:?}",
                                std::mem::discriminant(&other)
                            )));
                        }
                    }
                }
                other => {
                    return Err(UplcError::Internal(format!(
                        "Case scrutinee must reduce to Constr or enumerable Constant, got {:?}",
                        std::mem::discriminant(&other)
                    )));
                }
            };
            if let Some(max) = max_branches {
                if branch_count > max {
                    return Err(UplcError::ScriptError);
                }
            }
            // Pick the branch term by tag. Mismatched tag = script
            // error (Haskell `MissingCaseBranch`).
            let branch = branches
                .get(tag as usize)
                .ok_or(UplcError::ScriptError)?
                .clone();
            // Apply `branch` to each payload value in order. Push
            // ApplyValue frames in REVERSE so the first arg ends up
            // on top of the stack and is popped (applied) first.
            for arg in payload.into_iter().rev() {
                kont.push(Frame::ApplyValue { argument: arg })?;
            }
            Ok(State::Compute {
                term: branch,
                env,
                kont,
            })
        }
    }
}

fn apply(
    function: Value,
    argument: Value,
    kont: Kont,
    tracker: Option<&mut crate::machine::cost::BudgetTracker>,
    trace_log: Option<&mut Vec<String>>,
) -> Result<State, UplcError> {
    match function {
        Value::Lambda { body, env } => Ok(State::Compute {
            term: *body,
            env: env.extend(argument),
            kont,
        }),
        Value::Builtin { id, forces, args } => {
            let v = crate::builtin::dispatch::apply_builtin(
                id, forces, args, argument, tracker, trace_log,
            )?;
            Ok(State::Return { value: v, kont })
        }
        Value::Const(_) | Value::Delay { .. } | Value::Constr { .. } => {
            Err(UplcError::Internal(format!(
                "applied non-function value: {:?}",
                std::mem::discriminant(&function)
            )))
        }
    }
}

fn force_value(
    value: Value,
    kont: Kont,
    tracker: Option<&mut crate::machine::cost::BudgetTracker>,
    trace_log: Option<&mut Vec<String>>,
) -> Result<State, UplcError> {
    match value {
        Value::Delay { body, env } => Ok(State::Compute {
            term: *body,
            env,
            kont,
        }),
        Value::Builtin { id, forces, args } => {
            use crate::builtin::dispatch::{force_builtin, ForceOutcome};
            match force_builtin(id, forces, args, tracker, trace_log)? {
                ForceOutcome::Pending(v) | ForceOutcome::Done(v) => {
                    Ok(State::Return { value: v, kont })
                }
                ForceOutcome::Excess => Err(UplcError::BuiltinTypeError {
                    builtin: id.name(),
                    reason: "excess force on already-saturated builtin".into(),
                }),
            }
        }
        other => Err(UplcError::Internal(format!(
            "force applied to non-Delay value: {:?}",
            std::mem::discriminant(&other)
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::Constant;
    use num_bigint::BigInt;

    fn int_term(n: i64) -> Term {
        Term::Const(Constant::Integer(BigInt::from(n)))
    }
    fn int_val(n: i64) -> Value {
        Value::Const(Constant::Integer(BigInt::from(n)))
    }

    #[test]
    fn const_int_evaluates_to_itself() {
        assert_eq!(evaluate(int_term(42)).unwrap(), int_val(42));
    }

    #[test]
    fn const_unit_evaluates_to_unit_value() {
        assert!(evaluate(Term::Const(Constant::Unit)).unwrap().is_unit());
    }

    #[test]
    fn error_term_yields_script_error() {
        assert!(matches!(evaluate(Term::Error), Err(UplcError::ScriptError)));
    }

    #[test]
    fn identity_lambda_applied_returns_arg() {
        // (lam x. x) 7  ⇒  7
        let id_app = Term::App(
            Box::new(Term::Lam(Box::new(Term::Var(1)))),
            Box::new(int_term(7)),
        );
        assert_eq!(evaluate(id_app).unwrap(), int_val(7));
    }

    #[test]
    fn const_function_applied_returns_first() {
        // (lam x. (lam y. x)) 1 2  ⇒  1
        let k = Term::Lam(Box::new(Term::Lam(Box::new(Term::Var(2)))));
        let applied = Term::App(
            Box::new(Term::App(Box::new(k), Box::new(int_term(1)))),
            Box::new(int_term(2)),
        );
        assert_eq!(evaluate(applied).unwrap(), int_val(1));
    }

    #[test]
    fn delay_then_force_recovers_value() {
        // (force (delay 42))  ⇒  42
        let dt = Term::Force(Box::new(Term::Delay(Box::new(int_term(42)))));
        assert_eq!(evaluate(dt).unwrap(), int_val(42));
    }

    #[test]
    fn force_of_non_delay_errors() {
        let bad = Term::Force(Box::new(int_term(42)));
        assert!(matches!(evaluate(bad), Err(UplcError::Internal(_))));
    }

    #[test]
    fn apply_non_function_errors() {
        let bad = Term::App(Box::new(int_term(1)), Box::new(int_term(2)));
        assert!(matches!(evaluate(bad), Err(UplcError::Internal(_))));
    }

    #[test]
    fn open_term_var_errors() {
        assert!(matches!(
            evaluate(Term::Var(1)),
            Err(UplcError::Internal(_))
        ));
    }

    // ── Builtin dispatch (UPLC-4 part 2) ───────────────────────────────────

    fn app(f: Term, a: Term) -> Term {
        Term::App(Box::new(f), Box::new(a))
    }

    #[test]
    fn add_integer_via_builtin_dispatch() {
        // (addInteger 3 4)  ⇒  7
        let t = app(
            app(
                Term::Builtin(crate::term::BuiltinId::AddInteger),
                int_term(3),
            ),
            int_term(4),
        );
        assert_eq!(evaluate(t).unwrap(), int_val(7));
    }

    #[test]
    fn equals_integer_via_builtin_dispatch() {
        let t = app(
            app(
                Term::Builtin(crate::term::BuiltinId::EqualsInteger),
                int_term(5),
            ),
            int_term(5),
        );
        assert_eq!(evaluate(t).unwrap(), Value::Const(Constant::Bool(true)));
    }

    #[test]
    fn if_then_else_requires_force_first() {
        // IfThenElse needs 1 force. Without forcing first, applying
        // an arg is a BuiltinTypeError.
        let t = app(
            Term::Builtin(crate::term::BuiltinId::IfThenElse),
            Term::Const(Constant::Bool(true)),
        );
        assert!(matches!(
            evaluate(t),
            Err(UplcError::BuiltinTypeError { .. })
        ));
    }

    #[test]
    fn if_then_else_picks_then_branch_after_force() {
        // (force ifThenElse) True 1 2  ⇒  1
        let t = app(
            app(
                app(
                    Term::Force(Box::new(Term::Builtin(crate::term::BuiltinId::IfThenElse))),
                    Term::Const(Constant::Bool(true)),
                ),
                int_term(1),
            ),
            int_term(2),
        );
        assert_eq!(evaluate(t).unwrap(), int_val(1));
    }

    #[test]
    fn divide_by_zero_yields_builtin_failure() {
        let t = app(
            app(
                Term::Builtin(crate::term::BuiltinId::DivideInteger),
                int_term(7),
            ),
            int_term(0),
        );
        assert!(matches!(evaluate(t), Err(UplcError::BuiltinFailure { .. })));
    }

    // ── Constr / Case (UPLC-3 part 2) ──────────────────────────────────────

    #[test]
    fn constr_zero_args_evaluates_to_constr_value() {
        let t = Term::Constr {
            tag: 0,
            args: vec![],
        };
        let v = evaluate(t).unwrap();
        assert_eq!(
            v,
            Value::Constr {
                tag: 0,
                args: vec![]
            }
        );
    }

    #[test]
    fn constr_with_args_evaluates_left_to_right() {
        // Constr 3 [1, 2]
        let t = Term::Constr {
            tag: 3,
            args: vec![int_term(1), int_term(2)],
        };
        let v = evaluate(t).unwrap();
        assert_eq!(
            v,
            Value::Constr {
                tag: 3,
                args: vec![int_val(1), int_val(2)]
            }
        );
    }

    #[test]
    fn constr_arg_evaluates_to_lambda() {
        // Constr 0 [(lam x. x)]
        let id = Term::Lam(Box::new(Term::Var(1)));
        let t = Term::Constr {
            tag: 0,
            args: vec![id.clone()],
        };
        let v = evaluate(t).unwrap();
        match v {
            Value::Constr { tag, args } => {
                assert_eq!(tag, 0);
                assert_eq!(args.len(), 1);
                assert!(matches!(args[0], Value::Lambda { .. }));
            }
            other => panic!("expected Constr, got {other:?}"),
        }
    }

    #[test]
    fn case_picks_branch_by_constr_tag() {
        // (case (Constr 1) [42, 99])  ⇒  99
        let scrutinee = Term::Constr {
            tag: 1,
            args: vec![],
        };
        let case = Term::Case {
            scrutinee: Box::new(scrutinee),
            branches: vec![int_term(42), int_term(99)],
        };
        let v = evaluate(case).unwrap();
        assert_eq!(v, int_val(99));
    }

    #[test]
    fn case_applies_branch_to_constr_args() {
        // (case (Constr 0 1 2) [(lam x. lam y. y)])  ⇒  2
        // i.e. the branch is a 2-arg lambda returning the second arg.
        let scrutinee = Term::Constr {
            tag: 0,
            args: vec![int_term(1), int_term(2)],
        };
        // (lam (lam (var 1)))  — index 1 = innermost (= second arg)
        let branch = Term::Lam(Box::new(Term::Lam(Box::new(Term::Var(1)))));
        let case = Term::Case {
            scrutinee: Box::new(scrutinee),
            branches: vec![branch],
        };
        let v = evaluate(case).unwrap();
        assert_eq!(v, int_val(2));
    }

    #[test]
    fn case_first_arg_then_second_applied_in_order() {
        // (case (Constr 0 10 20) [(lam (lam (var 2)))])  ⇒  10
        // The branch returns its FIRST argument (which is at de Bruijn
        // index 2 from inside the inner lambda).
        let scrutinee = Term::Constr {
            tag: 0,
            args: vec![int_term(10), int_term(20)],
        };
        let branch = Term::Lam(Box::new(Term::Lam(Box::new(Term::Var(2)))));
        let case = Term::Case {
            scrutinee: Box::new(scrutinee),
            branches: vec![branch],
        };
        let v = evaluate(case).unwrap();
        assert_eq!(v, int_val(10));
    }

    #[test]
    fn case_with_out_of_range_tag_yields_script_error() {
        // Constr 5 with only 2 branches.
        let scrutinee = Term::Constr {
            tag: 5,
            args: vec![],
        };
        let case = Term::Case {
            scrutinee: Box::new(scrutinee),
            branches: vec![int_term(1), int_term(2)],
        };
        assert!(matches!(evaluate(case), Err(UplcError::ScriptError)));
    }

    #[test]
    fn case_with_integer_scrutinee_out_of_range_errors() {
        // (case 42 [99]) — Plutus 1.1.0 case-on-Integer indexes
        // into branches by the scrutinee value; 42 is out of range
        // for a single-branch list → ScriptError.
        let case = Term::Case {
            scrutinee: Box::new(int_term(42)),
            branches: vec![int_term(99)],
        };
        assert!(matches!(evaluate(case), Err(UplcError::ScriptError)));
    }

    #[test]
    fn case_with_bytestring_scrutinee_errors() {
        // ByteString is not enumerable by case → Internal error.
        use crate::term::Constant;
        let case = Term::Case {
            scrutinee: Box::new(Term::Const(Constant::ByteString(vec![1, 2]))),
            branches: vec![int_term(99)],
        };
        assert!(matches!(evaluate(case), Err(UplcError::Internal(_))));
    }
}
