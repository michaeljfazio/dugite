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
        Term::Constr { .. } | Term::Case { .. } => Err(UplcError::Internal(
            "Constr / Case evaluation not yet wired (UPLC-3 part 2)".into(),
        )),
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
}
