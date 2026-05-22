//! De Bruijn environment for the CEK machine.
//!
//! Environments are cons-list "stacks" of values bound by enclosing
//! `Lam`s. Lookup is 1-based De Bruijn (the innermost binder is
//! index 1; index 0 is the sentinel for free variables and yields an
//! error per the Haskell reference's `checkScope` rule).

use crate::machine::value::Value;
use crate::UplcError;

/// CEK environment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Env {
    /// Values pushed by enclosing `Lam`s. The innermost binder is the
    /// LAST element of the vec; De Bruijn index `i` (1-based) resolves
    /// to `entries[len - i]`.
    entries: Vec<Value>,
}

impl Env {
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolve a 1-based De Bruijn index.
    pub fn lookup(&self, index: u64) -> Result<&Value, UplcError> {
        if index == 0 {
            return Err(UplcError::Internal(
                "CEK env: De Bruijn index 0 is the free-variable sentinel".into(),
            ));
        }
        let len = self.entries.len() as u64;
        if index > len {
            return Err(UplcError::Internal(format!(
                "CEK env: De Bruijn index {index} out of range (env depth {len})"
            )));
        }
        let slot = (len - index) as usize;
        Ok(&self.entries[slot])
    }

    pub fn extend(&self, v: Value) -> Self {
        let mut next = self.clone();
        next.entries.push(v);
        next
    }

    pub fn depth(&self) -> usize {
        self.entries.len()
    }
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
    fn empty_env_lookup_errors() {
        let env = Env::new();
        assert!(env.lookup(1).is_err());
        assert_eq!(env.depth(), 0);
    }

    #[test]
    fn index_zero_is_sentinel_error() {
        let env = Env::new().extend(int_val(1));
        assert!(matches!(env.lookup(0), Err(UplcError::Internal(_))));
    }

    #[test]
    fn innermost_binder_is_index_1() {
        let env = Env::new().extend(int_val(10)).extend(int_val(20));
        assert_eq!(env.lookup(1).unwrap(), &int_val(20));
        assert_eq!(env.lookup(2).unwrap(), &int_val(10));
    }

    #[test]
    fn index_past_bottom_errors() {
        let env = Env::new().extend(int_val(7));
        assert!(env.lookup(2).is_err());
    }

    #[test]
    fn extend_is_non_destructive() {
        let env1 = Env::new().extend(int_val(1));
        let env2 = env1.extend(int_val(2));
        assert_eq!(env1.depth(), 1);
        assert_eq!(env2.depth(), 2);
        assert_eq!(env2.lookup(1).unwrap(), &int_val(2));
    }
}
