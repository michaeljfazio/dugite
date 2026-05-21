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
//! Defensive bound: a hard `MAX_KONTINUATION_DEPTH` cap on the
//! continuation stack protects against pathological terms that spawn
//! exponentially-deep frame stacks without consuming proportional CPU
//! budget (a Cardano-specific DoS concern noted in the original Plutus
//! tech report).

#![allow(dead_code)]

pub mod context;
pub mod cost;
pub mod env;
pub mod step;
pub mod value;

/// Cap on the continuation-stack depth. Mainnet scripts never reach
/// anywhere close to this; the limit is purely a DoS guard.
pub const MAX_KONTINUATION_DEPTH: usize = 4 * 1024;

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
