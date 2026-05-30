//! De Bruijn environment for the CEK machine.
//!
//! Environments are cons-list "stacks" of values bound by enclosing
//! `Lam`s. Lookup is 1-based De Bruijn (the innermost binder is
//! index 1; index 0 is the sentinel for free variables and yields an
//! error per the Haskell reference's `checkScope` rule).
//!
//! ## Representation: shared persistent cons-list
//!
//! The environment is an `Rc`-linked cons-list — the same shape as the
//! Haskell CEK machine's `Env` (a chain of value cells). `extend` is
//! **O(1)**: it allocates a single node pointing at the existing tail
//! (an `Rc` pointer bump), so the parent environment is *shared*, never
//! copied. This is critical for deeply-recursive Plutus scripts: a naive
//! `Vec<Value>` that clones the whole vector on every binder push costs
//! O(depth) per extend and O(depth²·value_size) overall, which let a
//! single recursive Alonzo script balloon the heap to tens of GB
//! (issue #26). The CEK machine's only memory bound is the `ExBudget`
//! memory dimension, which charges *abstract* `ExMemory` units — it does
//! not account for an implementation that physically copies the
//! environment, so the representation itself must be allocation-frugal
//! and share structure the way the reference machine does.
//!
//! Lookup walks the parent chain (`O(index)`), matching the reference
//! machine's cons-list lookup.

use crate::machine::value::Value;
use crate::UplcError;
use std::rc::Rc;

/// A single environment cell: a bound value and a shared pointer to the
/// enclosing environment.
#[derive(Debug, PartialEq, Eq)]
struct Node {
    value: Value,
    parent: Option<Rc<Node>>,
}

/// CEK environment — a shared persistent cons-list of bound values.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Env {
    /// Innermost binder (De Bruijn index 1). `None` is the empty env.
    head: Option<Rc<Node>>,
    /// Cached depth so `lookup` bounds-checks and `depth()` stay O(1).
    depth: usize,
}

impl Env {
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolve a 1-based De Bruijn index. The innermost binder is index 1.
    pub fn lookup(&self, index: u64) -> Result<&Value, UplcError> {
        if index == 0 {
            return Err(UplcError::Internal(
                "CEK env: De Bruijn index 0 is the free-variable sentinel".into(),
            ));
        }
        let out_of_range = || {
            UplcError::Internal(format!(
                "CEK env: De Bruijn index {index} out of range (env depth {})",
                self.depth
            ))
        };
        if index > self.depth as u64 {
            return Err(out_of_range());
        }
        // index 1 → head; index i → walk i-1 parents. The bound check above
        // guarantees the chain is long enough, but we resolve fallibly rather
        // than panic if a future change ever desyncs `depth` from the chain.
        let mut node: &Node = self.head.as_deref().ok_or_else(out_of_range)?;
        for _ in 1..index {
            node = node.parent.as_deref().ok_or_else(out_of_range)?;
        }
        Ok(&node.value)
    }

    /// Push a binder. O(1): the parent environment is shared, not copied.
    pub fn extend(&self, v: Value) -> Self {
        Env {
            head: Some(Rc::new(Node {
                value: v,
                parent: self.head.clone(),
            })),
            depth: self.depth + 1,
        }
    }

    pub fn depth(&self) -> usize {
        self.depth
    }
}

impl Drop for Env {
    /// Drop the cons-list iteratively rather than letting the compiler's
    /// recursive `Rc<Node>` drop blow the stack on a deeply-nested
    /// environment (a recursive script can build a chain thousands deep).
    /// We only unwind cells we uniquely own; the moment a node is still
    /// shared by another `Env`, its tail is reachable elsewhere and we
    /// stop.
    fn drop(&mut self) {
        let mut link = self.head.take();
        while let Some(rc) = link {
            match Rc::try_unwrap(rc) {
                Ok(mut node) => link = node.parent.take(),
                Err(_) => break,
            }
        }
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
    fn extend_is_non_destructive_and_shares_parent() {
        let env1 = Env::new().extend(int_val(1));
        let env2 = env1.extend(int_val(2));
        assert_eq!(env1.depth(), 1);
        assert_eq!(env2.depth(), 2);
        assert_eq!(env2.lookup(1).unwrap(), &int_val(2));
        assert_eq!(env2.lookup(2).unwrap(), &int_val(1));
        // env1 still valid and unchanged after env2 extended from it.
        assert_eq!(env1.lookup(1).unwrap(), &int_val(1));
    }

    /// A deep chain must extend in O(1) per step and drop without
    /// overflowing the stack (regression for the recursive-script heap
    /// blowup, issue #26).
    #[test]
    fn deep_chain_extends_and_drops() {
        let mut env = Env::new();
        for i in 0..200_000i64 {
            env = env.extend(int_val(i));
        }
        assert_eq!(env.depth(), 200_000);
        assert_eq!(env.lookup(1).unwrap(), &int_val(199_999));
        assert_eq!(env.lookup(200_000).unwrap(), &int_val(0));
        // Dropping `env` here exercises the iterative Drop.
    }
}
