//! CEK values.

use crate::machine::env::Env;
use crate::term::{BuiltinId, Constant, Term};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Const(Constant),
    Lambda {
        body: Box<Term>,
        env: Env,
    },
    Delay {
        body: Box<Term>,
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
            body: Box::new(Term::Var(1)),
            env: Env::new(),
        };
        let l2 = Value::Lambda {
            body: Box::new(Term::Var(1)),
            env: Env::new(),
        };
        assert_eq!(l1, l2);
    }
}
