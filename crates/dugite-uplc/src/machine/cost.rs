//! CEK machine cost-model — per-term-type charges and budget tracking.
//!
//! Mirrors `PlutusCore.Evaluation.Machine.MachineParameters` from the
//! Haskell reference. Each CEK `Compute` step charges a [`ExBudget`]
//! determined by the term variant being entered; a separate startup
//! cost is charged once when the machine boots.
//!
//! For programs whose cost model is configured (e.g. phase-2 evaluation
//! on chain), the [`MachineCosts`] table is loaded from the per-version
//! cost-model JSON. For programs whose cost model is *not* configured
//! (the conformance harness, ad-hoc test runs), [`MachineCosts::DEFAULT`]
//! is used — these are the Plutus 1.65.0.0 reference defaults
//! (`cekMachineCostsE.json`), the cost model the conformance corpus
//! goldens are computed against.
//!
//! A "slippage" counter buffers per-step charges; the running total is
//! flushed against the remaining budget every [`SLIPPAGE`] steps so
//! exhaustion checks remain cheap.

pub use super::ExBudget;
use crate::term::Term;
use crate::UplcError;

/// Per-term-type CEK machine costs.
///
/// Each field is the cost charged on a single CEK `Compute` step that
/// enters a term of the matching variant. Layout matches the Haskell
/// `CekMachineCosts` record exactly so a future cost-model loader can
/// populate it from on-chain protocol parameters with no field shuffle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MachineCosts {
    /// Charged once when [`BudgetTracker::new`] constructs the tracker
    /// — mirrors `cekStartupCost`.
    pub startup: ExBudget,
    pub var: ExBudget,
    pub constant: ExBudget,
    pub lam: ExBudget,
    pub delay: ExBudget,
    pub force: ExBudget,
    pub apply: ExBudget,
    pub builtin: ExBudget,
    pub constr: ExBudget,
    pub case_: ExBudget,
}

impl MachineCosts {
    /// Plutus 1.65.0.0 default machine cost model
    /// (`plutus-core/cost-model/data/cekMachineCostsE.json`).  This is
    /// the model the upstream `plutus-conformance` corpus computes its
    /// budget goldens against.
    pub const DEFAULT: Self = Self {
        startup: ExBudget { cpu: 100, mem: 100 },
        var: ExBudget {
            cpu: 16_000,
            mem: 100,
        },
        constant: ExBudget {
            cpu: 16_000,
            mem: 100,
        },
        lam: ExBudget {
            cpu: 16_000,
            mem: 100,
        },
        delay: ExBudget {
            cpu: 16_000,
            mem: 100,
        },
        force: ExBudget {
            cpu: 16_000,
            mem: 100,
        },
        apply: ExBudget {
            cpu: 16_000,
            mem: 100,
        },
        builtin: ExBudget {
            cpu: 16_000,
            mem: 100,
        },
        constr: ExBudget {
            cpu: 16_000,
            mem: 100,
        },
        case_: ExBudget {
            cpu: 16_000,
            mem: 100,
        },
    };

    /// Cost charged when entering `term` in a `Compute` step.
    ///
    /// `Term::Error` does not advance the machine (it short-circuits to
    /// a script failure) and therefore costs nothing.
    pub const fn cost_for(&self, term: &Term) -> ExBudget {
        match term {
            Term::Var(_) => self.var,
            Term::Const(_) => self.constant,
            Term::Lam(_) => self.lam,
            Term::Delay(_) => self.delay,
            Term::Force(_) => self.force,
            Term::App(_, _) => self.apply,
            Term::Builtin(_) => self.builtin,
            Term::Constr { .. } => self.constr,
            Term::Case { .. } => self.case_,
            Term::Error => ExBudget { cpu: 0, mem: 0 },
        }
    }
}

/// The Haskell CEK uses a "slippage" counter: instead of checking the
/// remaining budget on every step (expensive), accumulate charges and
/// check in blocks of [`SLIPPAGE`] steps. We mirror the same constant.
pub const SLIPPAGE: u64 = 200;

/// State for a budget-tracking CEK run.
#[derive(Debug, Clone)]
pub struct BudgetTracker {
    pub remaining: ExBudget,
    pub starting: ExBudget,
    pub costs: MachineCosts,
    /// Charges accumulated since the last flush against `remaining`.
    pending: ExBudget,
    /// Compute steps accumulated since the last flush — when this
    /// reaches `SLIPPAGE` the running total is flushed.
    pending_count: u64,
}

impl BudgetTracker {
    /// Construct a tracker with the default machine cost model.  The
    /// startup cost is charged immediately so [`consumed`] reflects it
    /// from the very first call.
    ///
    /// [`consumed`]: BudgetTracker::consumed
    pub fn new(initial: ExBudget) -> Self {
        Self::with_costs(initial, MachineCosts::DEFAULT)
    }

    /// Construct a tracker with a caller-supplied machine cost model
    /// (e.g. one loaded from on-chain protocol parameters).
    pub fn with_costs(initial: ExBudget, costs: MachineCosts) -> Self {
        let mut t = Self {
            remaining: initial,
            starting: initial,
            costs,
            pending: ExBudget { cpu: 0, mem: 0 },
            pending_count: 0,
        };
        // Charge startup so `consumed()` always includes it. We
        // tolerate a starting budget too small to cover the startup
        // cost (remaining goes negative) — the next flush surfaces
        // it as a normal exhaustion error.
        let _ = t.remaining.try_subtract(t.costs.startup);
        t
    }

    /// Charge a CEK `Compute` step for `term`.  Costs are accumulated
    /// and checked against the remaining budget every [`SLIPPAGE`]
    /// steps; exhaustion is returned at the flush boundary.
    pub fn compute_step(&mut self, term: &Term) -> Result<(), UplcError> {
        let cost = self.costs.cost_for(term);
        self.pending.cpu = self.pending.cpu.saturating_add(cost.cpu);
        self.pending.mem = self.pending.mem.saturating_add(cost.mem);
        self.pending_count = self.pending_count.saturating_add(1);
        if self.pending_count >= SLIPPAGE {
            self.flush()?;
        }
        Ok(())
    }

    /// Charge pending accumulated step cost against `remaining`.
    /// Called from `compute_step` at slippage boundaries and by the
    /// CEK driver at evaluation end to capture the trailing partial
    /// slip.
    pub fn flush(&mut self) -> Result<(), UplcError> {
        let cost = std::mem::replace(&mut self.pending, ExBudget { cpu: 0, mem: 0 });
        self.pending_count = 0;
        if cost.cpu == 0 && cost.mem == 0 {
            return Ok(());
        }
        if !self.remaining.try_subtract(cost) {
            return Err(UplcError::BudgetExhausted {
                cpu_remaining: self.remaining.cpu,
                mem_remaining: self.remaining.mem,
            });
        }
        Ok(())
    }

    /// Charge an explicit cost (e.g., for a builtin denotation
    /// invocation). Flushes pending step charges first so the order of
    /// charges matches the Haskell reference's `chargeAndStep`
    /// semantics.
    pub fn charge(&mut self, cost: ExBudget) -> Result<(), UplcError> {
        self.flush()?;
        if !self.remaining.try_subtract(cost) {
            return Err(UplcError::BudgetExhausted {
                cpu_remaining: self.remaining.cpu,
                mem_remaining: self.remaining.mem,
            });
        }
        Ok(())
    }

    /// Compute the consumed budget = starting - remaining + pending.
    /// Includes unflushed step charges so partial-run observability is
    /// accurate.
    pub fn consumed(&self) -> ExBudget {
        ExBudget {
            cpu: (self.starting.cpu - self.remaining.cpu).saturating_add(self.pending.cpu),
            mem: (self.starting.mem - self.remaining.mem).saturating_add(self.pending.mem),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::Constant;

    fn unit_term() -> Term {
        Term::Const(Constant::Unit)
    }

    #[test]
    fn new_charges_startup_immediately() {
        let t = BudgetTracker::new(ExBudget {
            cpu: 1_000_000,
            mem: 1_000_000,
        });
        let consumed = t.consumed();
        assert_eq!(consumed.cpu, MachineCosts::DEFAULT.startup.cpu);
        assert_eq!(consumed.mem, MachineCosts::DEFAULT.startup.mem);
    }

    #[test]
    fn compute_step_below_slippage_buffers() {
        let mut t = BudgetTracker::new(ExBudget {
            cpu: 1_000_000_000,
            mem: 1_000_000_000,
        });
        let baseline = t.remaining;
        for _ in 0..(SLIPPAGE - 1) {
            t.compute_step(&unit_term()).unwrap();
        }
        // Remaining has only been reduced by startup; the steps are
        // still in `pending`.
        assert_eq!(t.remaining.cpu, baseline.cpu);
        assert_eq!(t.remaining.mem, baseline.mem);
        // `consumed` still includes the buffered cost.
        let consumed = t.consumed();
        let per_step = MachineCosts::DEFAULT.constant;
        assert_eq!(
            consumed.cpu,
            MachineCosts::DEFAULT.startup.cpu + per_step.cpu * (SLIPPAGE as i64 - 1),
        );
    }

    #[test]
    fn compute_step_at_slippage_flushes() {
        let mut t = BudgetTracker::new(ExBudget {
            cpu: 1_000_000_000,
            mem: 1_000_000_000,
        });
        for _ in 0..SLIPPAGE {
            t.compute_step(&unit_term()).unwrap();
        }
        // After the boundary the pending bucket is empty.
        assert_eq!(t.consumed().cpu - MachineCosts::DEFAULT.startup.cpu, {
            MachineCosts::DEFAULT.constant.cpu * SLIPPAGE as i64
        });
    }

    #[test]
    fn exhausted_budget_yields_typed_error() {
        let mut t = BudgetTracker::new(ExBudget { cpu: 200, mem: 200 });
        // Startup charges 100/100, leaving 100/100. The first flush of
        // even a single 16000-cpu step exhausts the budget.
        let mut err = None;
        for _ in 0..SLIPPAGE {
            if let Err(e) = t.compute_step(&unit_term()) {
                err = Some(e);
                break;
            }
        }
        let err = err.expect("expected exhaustion within one slippage window");
        assert!(matches!(err, UplcError::BudgetExhausted { .. }));
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
        let startup = MachineCosts::DEFAULT.startup;
        assert_eq!(t.remaining.cpu, 1_000_000 - 100_000 - startup.cpu);
        assert_eq!(t.remaining.mem, 1_000_000 - 50_000 - startup.mem);
    }

    #[test]
    fn explicit_charge_over_budget_errors() {
        let mut t = BudgetTracker::new(ExBudget { cpu: 150, mem: 150 });
        // Startup leaves 50/50; an explicit charge above that fails.
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
            t.compute_step(&unit_term()).unwrap();
        }
        t.flush().unwrap();
        let consumed = t.consumed();
        let startup = MachineCosts::DEFAULT.startup;
        let per_step = MachineCosts::DEFAULT.constant;
        assert_eq!(consumed.cpu, startup.cpu + per_step.cpu * 10);
        assert_eq!(consumed.mem, startup.mem + per_step.mem * 10);
    }

    #[test]
    fn error_term_is_free() {
        let mut t = BudgetTracker::new(ExBudget {
            cpu: 1_000_000,
            mem: 1_000_000,
        });
        let before = t.consumed();
        t.compute_step(&Term::Error).unwrap();
        assert_eq!(t.consumed(), before);
    }
}
