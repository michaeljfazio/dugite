//! CEK machine — the Plutus Core abstract machine.
//!
//! "CEK" stands for the three components of the machine state:
//!
//!  - **C**ontrol: the term being reduced (or the value being returned
//!    up a continuation).
//!  - **E**nvironment: a stack of values bound by enclosing `Lam`s
//!    (indexed by De Bruijn index).
//!  - **K**ontinuation: a stack of `Frame`s describing what to do next
//!    when the current term reduces to a value.
//!
//! The implementation mirrors the formal spec (see DESIGN.md §4). Two
//! invariants are non-negotiable:
//!
//!  1. **Bounded heap allocation.** No unbounded `Vec::reserve` or
//!     `Box::new` that an adversary can drive past memory limits.
//!  2. **Step-by-step budget accounting.** Every reduction step charges
//!     the cost model and aborts the moment either dimension goes
//!     negative — *before* taking the step's allocations, not after.
//!
//! The continuation stack (`Kont`) has no depth cap: it is a
//! heap-allocated `Vec<Frame>`, not OS call-stack recursion, so it
//! cannot overflow the native stack. Its growth is bounded purely by
//! `ExBudget` exhaustion — exactly like Haskell's `Context`
//! (`UntypedPlutusCore.Evaluation.Machine.Cek.Internal`), which has no
//! depth field at all. An earlier hard `MAX_KONTINUATION_DEPTH` cap was
//! removed (#817): it rejected budget-valid, non-tail-recursive scripts
//! that cardano-node accepts, which is a live consensus-fork risk, not
//! a DoS mitigation Haskell itself relies on.

pub mod context;
pub mod cost;
pub mod env;
pub mod step;
pub mod value;

pub use self::step::{evaluate, State};
pub use self::value::Value;

/// `(cpu, mem)` units, mirroring the Haskell `ExBudget` record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ExBudget {
    pub cpu: i64,
    pub mem: i64,
}

impl ExBudget {
    /// Subtract `other` from `self` in place. Returns `false` if either
    /// dimension would go negative (caller treats this as
    /// budget-exhaustion).
    pub fn try_subtract(&mut self, other: ExBudget) -> bool {
        let new_cpu = self.cpu - other.cpu;
        let new_mem = self.mem - other.mem;
        if new_cpu < 0 || new_mem < 0 {
            return false;
        }
        self.cpu = new_cpu;
        self.mem = new_mem;
        true
    }
}

/// Result of a CEK evaluation: the produced value, the budget consumed,
/// and the captured trace log.
#[derive(Debug)]
pub struct EvalResult {
    pub value: crate::term::Term,
    pub budget_consumed: ExBudget,
    pub logs: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ex_budget_default_is_zero() {
        let b = ExBudget::default();
        assert_eq!(b.cpu, 0);
        assert_eq!(b.mem, 0);
    }

    #[test]
    fn try_subtract_succeeds_when_in_budget() {
        let mut b = ExBudget { cpu: 100, mem: 50 };
        assert!(b.try_subtract(ExBudget { cpu: 40, mem: 30 }));
        assert_eq!(b, ExBudget { cpu: 60, mem: 20 });
    }

    #[test]
    fn try_subtract_succeeds_at_exact_zero() {
        let mut b = ExBudget { cpu: 10, mem: 5 };
        assert!(b.try_subtract(ExBudget { cpu: 10, mem: 5 }));
        assert_eq!(b, ExBudget { cpu: 0, mem: 0 });
    }

    #[test]
    fn try_subtract_fails_on_cpu_overshoot() {
        let mut b = ExBudget { cpu: 10, mem: 100 };
        let before = b;
        assert!(!b.try_subtract(ExBudget { cpu: 11, mem: 5 }));
        // Failed subtractions must not mutate state — the caller may
        // want to retry with a different budget or surface a
        // BudgetExhausted error using the pre-attempt remaining.
        assert_eq!(b, before);
    }

    #[test]
    fn try_subtract_fails_on_mem_overshoot() {
        let mut b = ExBudget { cpu: 100, mem: 10 };
        let before = b;
        assert!(!b.try_subtract(ExBudget { cpu: 5, mem: 11 }));
        assert_eq!(b, before);
    }

    #[test]
    fn try_subtract_fails_if_either_dim_overshoots() {
        let mut b = ExBudget { cpu: 5, mem: 5 };
        let before = b;
        assert!(!b.try_subtract(ExBudget { cpu: 10, mem: 10 }));
        assert_eq!(b, before);
    }
}
