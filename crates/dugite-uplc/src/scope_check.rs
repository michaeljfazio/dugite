//! Eager de Bruijn scope check ("`checkScope`"), run once over the
//! fully-applied term before CEK evaluation starts (issue #823,
//! oracle-confirmed).
//!
//! Haskell's `mkTermToEvaluate` (`PlutusLedgerApi.Common.Eval`) runs
//! `UntypedPlutusCore.Check.Scope.checkScope` over the fully-applied
//! term (script + all pre-applied args) BEFORE the CEK machine starts.
//! A free variable is a phase-2 (collateral-consuming) failure
//! regardless of whether the machine would ever dynamically reach it —
//! e.g. a free variable under an unforced `Delay`, or inside a lambda
//! body that's never applied. dugite's CEK machine only catches a free
//! variable lazily, at `Env::lookup`, i.e. only if it is actually
//! reached — silently accepting scripts cardano-node rejects.
//!
//! ## The Haskell "blind spot" — mirrored exactly
//!
//! `checkScope` recurses through `Var`, `LamAbs`, `Apply`, `Force`,
//! `Delay` — and NOTHING ELSE. `Constr` args and `Case` (scrutinee and
//! branches) are *not* traversed; a free variable that only appears
//! there is not statically rejected by Haskell, so this checker must
//! not reject it either. A fully-recursive check would over-reject
//! mainnet-valid scripts — see the `constr_arg_free_var_not_rejected` /
//! `case_scrutinee_and_branch_free_vars_not_rejected` tests below.

use crate::term::Term;
use crate::UplcError;

/// Check that every `Var` reachable via the `Var`/`Lam`/`App`/`Force`/
/// `Delay` spine of `term` refers to an enclosing binder.
///
/// De Bruijn indices are 1-based (the innermost binder is index 1);
/// index 0 is never valid (see `crate::machine::env`'s sentinel
/// convention). `lvl` starts at 0 — no enclosing binders at the top of
/// the fully-applied term.
pub fn check_scope(term: &Term) -> Result<(), UplcError> {
    go(term, 0)
}

fn go(term: &Term, lvl: u64) -> Result<(), UplcError> {
    // Deeply right-nested terms (e.g. non-tail-recursive validators —
    // see #817) can nest thousands of `App`/`Force`/`Delay` levels.
    // `stacker::maybe_grow` extends the OS stack transparently rather
    // than imposing a depth cap Haskell has no analogue for, mirroring
    // the same pattern used by the flat decoder's term walks
    // (`crate::flat::term::validate_term_depth`).
    stacker::maybe_grow(128 * 1024, 1024 * 1024, || go_inner(term, lvl))
}

fn go_inner(term: &Term, lvl: u64) -> Result<(), UplcError> {
    match term {
        Term::Var(i) => {
            if *i == 0 || *i > lvl {
                return Err(UplcError::FreeVariable(*i));
            }
            Ok(())
        }
        Term::Lam(body) => go(body, lvl + 1),
        Term::App(fun, arg) => {
            go(fun, lvl)?;
            go(arg, lvl)
        }
        Term::Force(body) | Term::Delay(body) => go(body, lvl),
        // Haskell's `checkScope` does NOT recurse into `Constr` args or
        // `Case` (scrutinee or branches) — its `_ -> pure ()` catch-all
        // arm. Matched here exactly; see the module doc's "blind spot".
        Term::Const(_)
        | Term::Builtin(_)
        | Term::Error
        | Term::Constr { .. }
        | Term::Case { .. } => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::Constant;
    use std::rc::Rc;

    fn int_term(n: i64) -> Term {
        Term::Const(Constant::Integer(num_bigint::BigInt::from(n)))
    }

    #[test]
    fn closed_term_passes() {
        // (lam x. x) 7
        let t = Term::App(
            Rc::new(Term::Lam(Rc::new(Term::Var(1)))),
            Rc::new(int_term(7)),
        );
        assert!(check_scope(&t).is_ok());
    }

    #[test]
    fn top_level_free_var_is_rejected() {
        assert!(matches!(
            check_scope(&Term::Var(1)),
            Err(UplcError::FreeVariable(1))
        ));
    }

    #[test]
    fn var_zero_is_rejected_as_free() {
        // Index 0 is never a valid binder reference (sentinel), even
        // with enclosing binders.
        let t = Term::Lam(Rc::new(Term::Var(0)));
        assert!(matches!(check_scope(&t), Err(UplcError::FreeVariable(0))));
    }

    #[test]
    fn free_var_under_unforced_delay_is_rejected() {
        // #823's canonical example: `(lam x (con integer 1)) (delay (var 3))`.
        // The lambda ignores `x`, so `delay (var 3)` is never forced at
        // runtime — but `checkScope` recurses into `Delay` eagerly and
        // must reject this statically, matching Haskell.
        let t = Term::App(
            Rc::new(Term::Lam(Rc::new(int_term(1)))),
            Rc::new(Term::Delay(Rc::new(Term::Var(3)))),
        );
        assert!(matches!(check_scope(&t), Err(UplcError::FreeVariable(3))));
    }

    #[test]
    fn free_var_under_force_is_rejected() {
        let t = Term::Force(Rc::new(Term::Var(5)));
        assert!(matches!(check_scope(&t), Err(UplcError::FreeVariable(5))));
    }

    #[test]
    fn free_var_in_unapplied_lambda_body_is_rejected() {
        // A free var deep inside a lambda that itself is never applied
        // still sits on the Lam/Var spine, so `checkScope` reaches it.
        let unused_lambda = Term::Lam(Rc::new(Term::Var(9)));
        // Top-level term never applies `unused_lambda` to anything —
        // wrap it in a `Delay` so it's a plain closed-over subterm.
        let t = Term::Delay(Rc::new(unused_lambda));
        assert!(matches!(check_scope(&t), Err(UplcError::FreeVariable(9))));
    }

    #[test]
    fn constr_arg_free_var_not_rejected() {
        // Haskell's `checkScope` does not recurse into `Constr` args —
        // a fully-recursive check would over-reject this mainnet-valid
        // shape. Must NOT be rejected here either.
        let t = Term::Constr {
            tag: 0,
            args: vec![Rc::new(Term::Var(99))],
        };
        assert!(check_scope(&t).is_ok());
    }

    #[test]
    fn case_scrutinee_and_branch_free_vars_not_rejected() {
        // Haskell's `checkScope` does not recurse into `Case` at all
        // (neither scrutinee nor branches) — mirrored exactly.
        let t = Term::Case {
            scrutinee: Rc::new(Term::Var(42)),
            branches: vec![Rc::new(Term::Var(7)), Rc::new(Term::Var(8))],
        };
        assert!(check_scope(&t).is_ok());
    }

    #[test]
    fn deep_non_tail_recursion_does_not_overflow_the_native_stack() {
        // Mirrors the #817 regression shape: thousands of nested
        // `App`s on the right spine. `check_scope` must walk this
        // without blowing the Rust call stack (via `stacker::maybe_grow`).
        const DEPTH: i64 = 5000;
        let mut term = int_term(0);
        for _ in 0..DEPTH {
            term = Term::App(
                Rc::new(Term::App(
                    Rc::new(Term::Builtin(crate::term::BuiltinId::AddInteger)),
                    Rc::new(int_term(1)),
                )),
                Rc::new(term),
            );
        }
        assert!(check_scope(&term).is_ok());
    }
}
