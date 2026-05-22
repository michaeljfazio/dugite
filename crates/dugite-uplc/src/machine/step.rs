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
        state = step(state)?;
        if let State::Done(v) = state {
            return Ok(v);
        }
    }
}

fn step(state: State) -> Result<State, UplcError> {
    match state {
        State::Compute { term, env, kont } => compute(term, env, kont),
        State::Return { value, kont } => return_compute(value, kont),
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
        Term::Builtin(id) => Err(UplcError::Internal(format!(
            "Builtin {} not yet wired (UPLC-4)",
            id.name()
        ))),
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

fn return_compute(value: Value, mut kont: Kont) -> Result<State, UplcError> {
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
        Frame::AwaitArg { function, .. } => apply(function, value, kont),
        Frame::Force => force_value(value, kont),
        Frame::ApplyValue { argument } => apply(value, argument, kont),
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
        Frame::Cases { branches, env } => match value {
            Value::Constr { tag, args } => {
                // Pick the branch term by tag. Mismatched tag = script
                // error (Haskell `MissingCaseBranch`).
                let branch = branches
                    .get(tag as usize)
                    .ok_or(UplcError::ScriptError)?
                    .clone();
                // Apply `branch` to each constr arg in order. Push
                // ApplyValue frames in REVERSE so the first arg ends
                // up on top of the stack and is popped (applied)
                // first.
                for arg in args.into_iter().rev() {
                    kont.push(Frame::ApplyValue { argument: arg })?;
                }
                Ok(State::Compute {
                    term: branch,
                    env,
                    kont,
                })
            }
            other => Err(UplcError::Internal(format!(
                "Case scrutinee must reduce to Constr, got {:?}",
                std::mem::discriminant(&other)
            ))),
        },
    }
}

fn apply(function: Value, argument: Value, kont: Kont) -> Result<State, UplcError> {
    match function {
        Value::Lambda { body, env } => Ok(State::Compute {
            term: *body,
            env: env.extend(argument),
            kont,
        }),
        Value::Builtin { .. } => Err(UplcError::Internal(
            "Builtin application not yet wired (UPLC-4)".into(),
        )),
        Value::Const(_) | Value::Delay { .. } | Value::Constr { .. } => {
            Err(UplcError::Internal(format!(
                "applied non-function value: {:?}",
                std::mem::discriminant(&function)
            )))
        }
    }
}

fn force_value(value: Value, kont: Kont) -> Result<State, UplcError> {
    match value {
        Value::Delay { body, env } => Ok(State::Compute {
            term: *body,
            env,
            kont,
        }),
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

    #[test]
    fn builtin_application_pending_returns_internal() {
        let bad = Term::Builtin(crate::term::BuiltinId::AddInteger);
        assert!(matches!(evaluate(bad), Err(UplcError::Internal(_))));
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
    fn case_with_non_constr_scrutinee_errors() {
        // (case 42 [99]) — scrutinee isn't a Constr.
        let case = Term::Case {
            scrutinee: Box::new(int_term(42)),
            branches: vec![int_term(99)],
        };
        assert!(matches!(evaluate(case), Err(UplcError::Internal(_))));
    }
}
