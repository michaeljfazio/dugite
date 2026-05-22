//! UPLC evaluation conformance harness — runs the official Plutus
//! test vectors (IntersectMBO/plutus, `plutus-conformance/test-cases/
//! uplc/evaluation/`) against dugite-uplc end-to-end.
//!
//! ## Pipeline
//!
//! For each leaf directory under `tests/conformance/`:
//!
//!  1. `name.uplc` — textual input.
//!  2. `dugite_uplc::syn::parse_program` — first-party textual parser.
//!  3. `dugite_uplc::machine::evaluate_with_budget` — CEK execution.
//!  4. Readback `Value → Term` — to compare against the golden.
//!  5. `name.uplc.expected` — same parse path; compared as
//!     De-Bruijn-normalised programs.
//!  6. `name.uplc.budget.expected` — formatted as Haskell pretty-prints
//!     `ExBudget` and string-compared.
//!
//! `parse error` and `evaluation failure` sentinels follow the
//! conventions established by `PlutusConformance.Common`
//! (`shownParseError` / `shownEvaluationFailure`).
//!
//! No third-party UPLC implementation is linked in. The entire stack
//! exercised here — parser, flat round-trip, CEK machine, readback —
//! is first-party dugite-uplc code.

#![cfg(feature = "conformance")]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use dugite_uplc::machine::cost::BudgetTracker;
use dugite_uplc::machine::step::evaluate_with_budget;
use dugite_uplc::machine::{ExBudget, Value};
use dugite_uplc::syn::{parse_program, ParseError};
use dugite_uplc::term::{BuiltinId, Term};
use dugite_uplc::Program;

/// Sentinel strings used by the Haskell conformance harness for
/// negative tests (`PlutusConformance.Common.shownParseError` /
/// `shownEvaluationFailure`).
const SHOWN_PARSE_ERROR: &str = "parse error";
const SHOWN_EVAL_FAILURE: &str = "evaluation failure";

/// Per-test entry point invoked by the build-script-generated test
/// functions. `corpus_rel` is the test directory relative to
/// `tests/conformance/`, used purely for diagnostics.
fn run_conformance_test(
    corpus_rel: &str,
    input_src: &str,
    expected_src: &str,
    expected_budget: &str,
) {
    // The expected outputs are golden files. Trim trailing newlines so
    // sentinels match regardless of file-end conventions, but leave the
    // body alone — the textual programs may legitimately contain
    // whitespace.
    let expected_src_trim = expected_src.trim_end_matches('\n').trim_end();
    let expected_budget_trim = expected_budget.trim_end_matches('\n').trim_end();
    let expected_kind = classify(expected_src_trim);

    // Step 1: parse the input. A parse error here is what the corpus
    // calls a "parse error".
    let parsed: Result<Program, ParseError> = parse_program(input_src);
    let input_prog = match parsed {
        Ok(p) => p,
        Err(_e) => {
            assert_eq!(
                expected_kind,
                ExpectKind::ParseError,
                "[{corpus_rel}] input did not parse, but expected {expected_src_trim:?}",
            );
            assert_eq!(
                expected_budget_trim, SHOWN_PARSE_ERROR,
                "[{corpus_rel}] budget golden disagrees with eval golden \
                 (parse error path)",
            );
            return;
        }
    };

    // Step 2: evaluate with budget tracking. Haskell uses `counting`
    // mode (unbounded budget); we use a budget so large that legitimate
    // programs never exhaust it. The corpus's "evaluation failure"
    // tests fail by other means (type mismatch, builtin failure, ...).
    let mut tracker = BudgetTracker::new(ExBudget {
        cpu: i64::MAX / 2,
        mem: i64::MAX / 2,
    });
    let eval = evaluate_with_budget(input_prog.term.clone(), &mut tracker);

    let value = match eval {
        Ok(v) => v,
        Err(_) => {
            assert_eq!(
                expected_kind,
                ExpectKind::EvalFailure,
                "[{corpus_rel}] evaluation failed but expected {expected_src_trim:?}",
            );
            assert_eq!(
                expected_budget_trim, SHOWN_EVAL_FAILURE,
                "[{corpus_rel}] budget golden disagrees with eval golden \
                 (eval failure path)",
            );
            return;
        }
    };

    // Successful evaluation — verify the golden expected an actual
    // program (not a parse/eval failure sentinel).
    match expected_kind {
        ExpectKind::ParseError => panic!(
            "[{corpus_rel}] expected `parse error` but dugite-uplc \
             accepted + evaluated the program"
        ),
        ExpectKind::EvalFailure => panic!(
            "[{corpus_rel}] expected `evaluation failure` but dugite-uplc \
             evaluated to {value:?}"
        ),
        ExpectKind::Program => {}
    }

    // Step 4: readback Value → Term.
    let result_term =
        readback(value).unwrap_or_else(|why| panic!("[{corpus_rel}] readback unsupported: {why}"));

    // Step 5: parse the expected golden and compare against the
    // result. Both sides are in De Bruijn form (the parser converts
    // names away during parsing), so equality is alpha-equivalence.
    let expected_prog = parse_program(expected_src).unwrap_or_else(|e| {
        panic!(
            "[{corpus_rel}] could not parse expected golden: {} (at {})",
            e.message, e.offset,
        )
    });

    let actual_prog = Program {
        version: input_prog.version,
        term: result_term,
    };

    assert_eq!(
        actual_prog, expected_prog,
        "[{corpus_rel}] result program disagrees with golden\n  \
         actual:   {actual_prog:?}\n  expected: {expected_prog:?}",
    );

    // Step 6: compare budget. Haskell pretty-prints `ExBudget` as
    //   ({cpu: <n>
    //   | mem: <n>})
    // (newline + pipe between the two fields). The corpus golden
    // files use this exact format.
    let consumed = tracker.consumed();
    let actual_budget = format!("({{cpu: {}\n| mem: {}}})", consumed.cpu, consumed.mem);

    assert_eq!(
        actual_budget.trim_end(),
        expected_budget_trim,
        "[{corpus_rel}] budget mismatch",
    );
}

#[derive(Debug, PartialEq, Eq)]
enum ExpectKind {
    ParseError,
    EvalFailure,
    Program,
}

fn classify(expected_trim: &str) -> ExpectKind {
    match expected_trim {
        SHOWN_PARSE_ERROR => ExpectKind::ParseError,
        SHOWN_EVAL_FAILURE => ExpectKind::EvalFailure,
        _ => ExpectKind::Program,
    }
}

/// Convert a CEK `Value` back to a `Term` for comparison against the
/// golden output. Matches the readback performed by the Haskell
/// `runCekNoEmit` driver.
///
/// The conformance corpus reduces most programs to a `Const`. A
/// handful reduce to closed lambdas, delays, partial-application
/// builtins, or constrs — those map back to the corresponding term
/// shape directly. Closures with non-empty captured environments
/// would require De Bruijn substitution; we surface that as an
/// explicit harness limitation rather than silently producing a
/// term that disagrees with the golden.
fn readback(value: Value) -> Result<Term, String> {
    match value {
        Value::Const(c) => Ok(Term::Const(c)),
        Value::Lambda { body, env } => {
            if env.depth() != 0 {
                return Err(format!(
                    "Value::Lambda with non-empty captured env ({} entries) — \
                     readback requires De Bruijn substitution",
                    env.depth()
                ));
            }
            Ok(Term::Lam(body))
        }
        Value::Delay { body, env } => {
            if env.depth() != 0 {
                return Err(format!(
                    "Value::Delay with non-empty captured env ({} entries) — \
                     readback requires De Bruijn substitution",
                    env.depth()
                ));
            }
            Ok(Term::Delay(body))
        }
        Value::Builtin { id, forces, args } => {
            // Reconstruct as `Force...Force (builtin id) arg0 arg1 ...`.
            let mut t = Term::Builtin(id_pass(id));
            for _ in 0..forces {
                t = Term::Force(Box::new(t));
            }
            for arg in args {
                let arg_term = readback(arg)?;
                t = Term::App(Box::new(t), Box::new(arg_term));
            }
            Ok(t)
        }
        Value::Constr { tag, args } => {
            let mut out = Vec::with_capacity(args.len());
            for a in args {
                out.push(readback(a)?);
            }
            Ok(Term::Constr { tag, args: out })
        }
    }
}

/// Identity on `BuiltinId`. Exists so the readback for `Value::Builtin`
/// reads uniformly across variant kinds.
fn id_pass(id: BuiltinId) -> BuiltinId {
    id
}

// Include the per-test-vector `#[test]` functions emitted by build.rs.
// When the conformance feature is enabled but the corpus is not yet
// downloaded, this file contains a single sentinel `#[test]` that
// prints a hint pointing at `just uplc-conformance-fetch`.
include!(concat!(env!("OUT_DIR"), "/generated_conformance_tests.rs"));
