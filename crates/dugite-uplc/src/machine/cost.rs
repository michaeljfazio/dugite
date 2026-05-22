//! Cost-model loader and slippage budget enforcement.
//!
//! The Plutus cost model is a flat array of integers (the
//! cardano-cli `cost-models` field of ProtocolParameters), one entry
//! per cost-model parameter. The number of parameters depends on
//! the Plutus version:
//!
//!   - V1: 166 parameters
//!   - V2: 175 parameters
//!   - V3: 297 parameters
//!
//! Each parameter is a piece of an `ExBudgetCost` shape — most
//! builtins use a constant + linear-in-input-size model, but some
//! have quadratic / piecewise definitions.
//!
//! This module currently lands the **infrastructure** — `ExBudget`
//! subtraction and a `step_charge` API the CEK driver calls per
//! reduction step. The per-builtin cost tables (parsed from
//! protocol params) arrive in a follow-on commit alongside the
//! phase-two wrapper; for now the CEK machine charges a flat unit
//! cost per step against a caller-provided budget. This is
//! sufficient to enforce the "no infinite loops" property required
//! for safe script execution before the byte-exact cost-model wire-up.

pub use super::ExBudget;
use crate::UplcError;

/// Per-step CPU cost when no detailed cost-model is loaded. Matches
/// the average cost across V3 builtins (~30k cpu units / step)
/// rounded conservatively low; the byte-exact loader replaces this
/// with the per-builtin amortised cost.
pub const DEFAULT_STEP_CPU: i64 = 23_000;

/// Per-step memory cost when no detailed cost-model is loaded.
pub const DEFAULT_STEP_MEM: i64 = 100;

/// The Haskell CEK uses a "slippage" counter: instead of charging the
/// budget on every step (expensive), charge in blocks of
/// `SLIPPAGE` steps. We mirror the same constant.
pub const SLIPPAGE: u64 = 200;

/// State for a budget-tracking CEK run.
#[derive(Debug, Clone)]
pub struct BudgetTracker {
    pub remaining: ExBudget,
    pub starting: ExBudget,
    /// Steps accumulated since the last budget deduction.
    steps_since_last_charge: u64,
}

impl BudgetTracker {
    pub fn new(initial: ExBudget) -> Self {
        Self {
            remaining: initial,
            starting: initial,
            steps_since_last_charge: 0,
        }
    }

    /// Increment the step counter and, if we've hit the slippage
    /// threshold, flush the accumulated cost against the budget.
    pub fn tick(&mut self) -> Result<(), UplcError> {
        self.steps_since_last_charge += 1;
        if self.steps_since_last_charge >= SLIPPAGE {
            self.flush()?;
        }
        Ok(())
    }

    /// Charge accumulated step cost against the remaining budget.
    /// Called from `tick` at slippage boundaries and by the caller
    /// at evaluation end to capture the trailing partial slip.
    pub fn flush(&mut self) -> Result<(), UplcError> {
        let cost = ExBudget {
            cpu: DEFAULT_STEP_CPU.saturating_mul(self.steps_since_last_charge as i64),
            mem: DEFAULT_STEP_MEM.saturating_mul(self.steps_since_last_charge as i64),
        };
        self.steps_since_last_charge = 0;
        if !self.remaining.try_subtract(cost) {
            return Err(UplcError::BudgetExhausted {
                cpu_remaining: self.remaining.cpu,
                mem_remaining: self.remaining.mem,
            });
        }
        Ok(())
    }

    /// Charge an explicit cost (e.g., for a builtin denotation
    /// invocation). The caller passes the cost computed from
    /// `arity_of` + per-builtin cost-model entry.
    pub fn charge(&mut self, cost: ExBudget) -> Result<(), UplcError> {
        // Flush pending step charges first so the order of charges
        // matches the Haskell reference's `chargeAndStep` semantics.
        self.flush()?;
        if !self.remaining.try_subtract(cost) {
            return Err(UplcError::BudgetExhausted {
                cpu_remaining: self.remaining.cpu,
                mem_remaining: self.remaining.mem,
            });
        }
        Ok(())
    }

    /// Compute the consumed budget = starting - remaining.
    pub fn consumed(&self) -> ExBudget {
        ExBudget {
            cpu: self.starting.cpu - self.remaining.cpu,
            mem: self.starting.mem - self.remaining.mem,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_without_threshold_no_charge() {
        let mut t = BudgetTracker::new(ExBudget {
            cpu: 1_000_000,
            mem: 1_000_000,
        });
        for _ in 0..(SLIPPAGE - 1) {
            t.tick().unwrap();
        }
        assert_eq!(t.remaining.cpu, 1_000_000);
        assert_eq!(t.remaining.mem, 1_000_000);
    }

    #[test]
    fn tick_at_threshold_charges() {
        let mut t = BudgetTracker::new(ExBudget {
            cpu: 10_000_000,
            mem: 100_000,
        });
        for _ in 0..SLIPPAGE {
            t.tick().unwrap();
        }
        let consumed = t.consumed();
        assert_eq!(consumed.cpu, DEFAULT_STEP_CPU * SLIPPAGE as i64);
        assert_eq!(consumed.mem, DEFAULT_STEP_MEM * SLIPPAGE as i64);
    }

    #[test]
    fn exhausted_budget_yields_typed_error() {
        let mut t = BudgetTracker::new(ExBudget { cpu: 100, mem: 100 });
        // First slippage flush should exhaust.
        let err = (0..SLIPPAGE)
            .map(|_| t.tick())
            .find(|r| r.is_err())
            .expect("expected at least one error in the slip range");
        assert!(matches!(err, Err(UplcError::BudgetExhausted { .. })));
    }

    #[test]
    fn explicit_charge_subtracts() {
        let mut t = BudgetTracker::new(ExBudget {
            cpu: 1_000_000,
            mem: 1_000_000,
        });
        t.charge(ExBudget {
            cpu: 100_000,
            mem: 50_000,
        })
        .unwrap();
        assert_eq!(t.remaining.cpu, 900_000);
        assert_eq!(t.remaining.mem, 950_000);
    }

    #[test]
    fn explicit_charge_over_budget_errors() {
        let mut t = BudgetTracker::new(ExBudget { cpu: 50, mem: 50 });
        let err = t
            .charge(ExBudget { cpu: 100, mem: 1 })
            .expect_err("expected exhaustion");
        assert!(matches!(err, UplcError::BudgetExhausted { .. }));
    }

    #[test]
    fn flush_captures_partial_slip() {
        let mut t = BudgetTracker::new(ExBudget {
            cpu: 1_000_000,
            mem: 1_000_000,
        });
        for _ in 0..10 {
            t.tick().unwrap();
        }
        t.flush().unwrap();
        let consumed = t.consumed();
        assert_eq!(consumed.cpu, DEFAULT_STEP_CPU * 10);
        assert_eq!(consumed.mem, DEFAULT_STEP_MEM * 10);
    }
}
