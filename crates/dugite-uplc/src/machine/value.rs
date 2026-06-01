//! CEK values.

use crate::machine::env::Env;
use crate::term::{BuiltinId, Constant, Term};
use std::rc::Rc;

/// Extract the owned `Term` from an `Rc<Term>`.
///
/// When this is the sole live reference (ref_count == 1), `Rc::try_unwrap`
/// succeeds and moves the term out for free — zero allocation, zero copy.
/// When the env node still holds a second reference (ref_count ≥ 2), we fall
/// back to a `(*rc).clone()` — the same cost as the old `Box<Term>` path, but
/// this now happens at most once per *distinct* apply site rather than once per
/// *lookup*. For a lambda called N times from the same env binding, the Rc path
/// saves N-1 deep-clone+drop cycles over the `Box` path.
#[inline]
pub fn rc_into_term(rc: Rc<Term>) -> Term {
    Rc::try_unwrap(rc).unwrap_or_else(|rc| (*rc).clone())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Const(Constant),
    /// Lambda closure. The body is wrapped in `Rc<Term>` so that
    /// `Value::clone()` — called on every De Bruijn `Var`-lookup from
    /// the env — copies only a reference count instead of deep-cloning
    /// the full term tree.
    ///
    /// The body term is immutable after construction; sharing the same
    /// allocation between multiple closure copies is semantically
    /// identical to the old `Box<Term>` ownership model.
    Lambda {
        body: Rc<Term>,
        env: Env,
    },
    /// Delay thunk. Same `Rc`-sharing rationale as `Lambda`: the body
    /// is the frozen thunk code, cloned by reference not by value.
    Delay {
        body: Rc<Term>,
        env: Env,
    },
    Builtin {
        id: BuiltinId,
        forces: u8,
        args: Vec<Value>,
    },
    Constr {
        tag: u64,
        args: Vec<Value>,
    },
}

impl Value {
    pub fn is_unit(&self) -> bool {
        matches!(self, Value::Const(Constant::Unit))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use num_bigint::BigInt;

    #[test]
    fn const_integer_inequality() {
        let a = Value::Const(Constant::Integer(BigInt::from(1)));
        let b = Value::Const(Constant::Integer(BigInt::from(2)));
        assert_ne!(a, b);
    }

    #[test]
    fn unit_predicate() {
        assert!(Value::Const(Constant::Unit).is_unit());
        assert!(!Value::Const(Constant::Bool(true)).is_unit());
    }

    #[test]
    fn lambda_eq_requires_same_body() {
        let l1 = Value::Lambda {
            body: Rc::new(Term::Var(1)),
            env: Env::new(),
        };
        let l2 = Value::Lambda {
            body: Rc::new(Term::Var(1)),
            env: Env::new(),
        };
        assert_eq!(l1, l2);
    }

    #[test]
    fn lambda_clone_shares_body_rc() {
        // Cloning a Lambda bumps the Rc refcount — the body allocation
        // is shared, not duplicated.
        let body = Rc::new(Term::Var(1));
        let l1 = Value::Lambda {
            body: Rc::clone(&body),
            env: Env::new(),
        };
        let l2 = l1.clone();
        // Both l1 and l2 share the same Rc allocation as `body`.
        assert_eq!(Rc::strong_count(&body), 3); // body + l1.body + l2.body
        assert_eq!(l1, l2);
    }
}
