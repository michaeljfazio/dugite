//! ImpSpec ledger-rule vectors.
//!
//! * **LEDGER and NEWEPOCH are APPLIED and COMPARED.** Each vector's state is
//!   decoded with the production Haskell-snapshot decoder, imported through
//!   `LedgerState::from_haskell_snapshot`, the signal is applied through
//!   `LedgerState::apply_block` (`ValidateAll`), and the result is compared with
//!   Haskell's `st_out` over an enumerated field list (`project.rs`, which also
//!   lists what is NOT compared and why). A vector with no `st_out` (Haskell's
//!   STS rejected) must be rejected by dugite too. Known divergences are pinned in
//!   `tests/conformance/expected/ledger_rules_replay.tsv` (`expected.rs`).
//!   See `imp_state.rs`, `apply.rs`, `replay.rs`, `compare.rs`.
//! * **The other rules are DECODE-ONLY**: POOL, CERT, CERTS, DELEG, GOVCERT, GOV,
//!   ENACT and RATIFY (ExecSpecRule states, which are not a `NewEpochState`) get a
//!   signal-shape check in `runner.rs`; nothing is applied or compared.
//!
//! `ctx` is not read by the replay: an STS transition is `TRC (env, st, sig)`, and
//! for LEDGER `env` is the `LedgerEnv` (slot, epoch, protocol parameters, pots)
//! while NEWEPOCH's `env` is `()`.
//!
//! ## Corpus generation: patched ImpSpec (official Haskell test vectors)
//!
//! The corpus is produced by patching the official cardano-ledger ImpSpec
//! conformance machinery to dump ALL test cases when `CONFORMANCE_CBOR_DUMP_PATH`
//! is set (not just Haskell/Agda divergences).
//!
//! Two ImpSpec files are patched by `patch-impspec-core.py`:
//!
//! 1. **`ExecSpecRule/Core.hs` — `testConformance`** (QuickCheck path)
//!    — captures ENACT, DELEG, GOVCERT, POOL, CERT, CERTS, GOV
//!
//! 2. **`Imp/Core.hs` — `conformanceHook`** (hook path)
//!    — captures NEWEPOCH (epoch boundaries) and LEDGER (tx submissions)
//!
//! The inputs come from ImpSpec's constrained-generator framework (authoritative).
//! The expected outputs (`st_out`) are the Haskell STS results (authoritative).
//!
//! ## Vector format (5 files per test-case directory; st_out is optional)
//!
//! ```text
//! <fixtures>/ledger-rules/<Rule>/<test_name>/
//!   conformance_dump_ctx.cbor     -- ExecContext (0xF6 CBOR null for NEWEPOCH)
//!   conformance_dump_env.cbor     -- Environment (0xF6 CBOR null for NEWEPOCH)
//!   conformance_dump_st.cbor      -- State (NewEpochState array(7); LedgerState array(2) for LEDGER)
//!   conformance_dump_sig.cbor     -- Signal (EpochNo for NEWEPOCH; tx CBOR for LEDGER)
//!   conformance_dump_st_out.cbor  -- Haskell expected final state (absent when STS rejects)
//! ```
//!
//! For LEDGER, `st` is a bare `LedgerState` (`array(2)`) and `env` a `LedgerEnv`;
//! for NEWEPOCH, `st` is a bare `NewEpochState` (`array(7)`). The UTxO is inline.
//!
//! Each test-case directory under a rule directory is decoded as one `ImpVector`.
//! See `vector.rs` for the decoder, `bridge.rs` for the decode-only checks,
//! `runner.rs` for those checks, and `replay.rs` for the apply-and-compare.

pub mod apply;
pub mod bridge;
pub mod compare;
pub mod expected;
pub mod imp_state;
pub mod project;
pub mod replay;
pub mod runner;
pub mod vector;

use std::path::Path;
use std::sync::OnceLock;

use crate::upstream::ledger_rules_replay::expected::{gate, Expected, UPDATE_ENV};
use crate::upstream::ledger_rules_replay::replay::{replay_vector, Replay, Verdict};

/// Test scenarios known to fail due to unimplemented Dugite features, for the
/// rules that are only DECODED (the ExecSpecRule rules).
///
/// Each entry is a tuple of (rule_substring, issue_url).  The pattern is
/// matched as a substring of the rule name.  Every entry must reference a
/// tracking issue.  **This list should decay to zero.**
///
/// LEDGER and NEWEPOCH do not use it: their known divergences live in the
/// expected-results file (see [`expected`]), which pins each one and fails on a
/// change in either direction.
const SKIP_LIST: &[(&str, &str)] = &[];

/// Returns the issue URL if a test's rule matches a skip entry, or `None`.
///
/// Patterns are matched as substrings of the rule name.
fn is_skipped(rule: &str) -> Option<&'static str> {
    for (pattern, issue) in SKIP_LIST {
        if rule.contains(pattern) {
            return Some(issue);
        }
    }
    None
}

/// The 4 required CBOR file names per test-case directory.
const REQUIRED_FILES: &[&str] = &[
    "conformance_dump_ctx.cbor",
    "conformance_dump_env.cbor",
    "conformance_dump_st.cbor",
    "conformance_dump_sig.cbor",
];

/// Returns `true` when `dir` contains all 4 required CBOR files.
fn is_test_case_dir(dir: &Path) -> bool {
    REQUIRED_FILES.iter().all(|name| dir.join(name).is_file())
}

/// Collect all test-case directories under `rule_dir` (two levels deep).
///
/// Structure: `<rule_dir>/<Rule>/<test_name>/` — each `<test_name>` directory
/// that contains all 4 required files is returned.
pub fn collect_test_case_dirs(rule_dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(rule_dir) else {
        return out;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // This is a rule directory (e.g. NEWEPOCH). Collect test-case dirs
            // inside it.
            let Ok(inner) = std::fs::read_dir(&path) else {
                continue;
            };
            for inner_entry in inner.flatten() {
                let inner_path = inner_entry.path();
                if inner_path.is_dir() && is_test_case_dir(&inner_path) {
                    out.push(inner_path);
                }
            }
        } else if path.is_file() && is_test_case_dir(rule_dir) {
            // Flat layout: the rule_dir itself is the test-case dir.
            // (Only push once, not once per file.)
            let rule_dir_buf = rule_dir.to_path_buf();
            if !out.contains(&rule_dir_buf) {
                out.push(rule_dir_buf);
            }
        }
    }
    out.sort();
    out
}

/// Outcome of validating one `test_N` vector directory.
#[derive(Debug, PartialEq, Eq)]
pub enum VectorOutcome {
    /// Vector ran successfully.
    Pass,
    /// Vector was intentionally skipped (matches `SKIP_LIST` or the
    /// runner returned `Skipped`).
    Skip(String),
    /// Vector failed validation.  String is a human-readable reason.
    Fail(String),
}

/// The vector id used in the expected-results file: `<Rule>/<test_dir>`.
pub fn vector_id(test_dir: &Path) -> String {
    let name = |p: Option<&Path>| {
        p.and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    format!("{}/{}", name(test_dir.parent()), name(Some(test_dir)))
}

/// Set to `1` to print, for every replayed vector, the field-level differences
/// behind its verdict (post-state and import fidelity).
pub const DETAIL_ENV: &str = "DUGITE_REPLAY_DETAIL";

/// Print the evidence behind a verdict.
pub fn report_detail(id: &str, r: &Replay) {
    eprintln!("[detail] {id}: {:?}", r.verdict);
    if let Some(post) = &r.post {
        eprintln!("[detail]   post-state differences ({}):", post.diffs.len());
        eprint!("{}", post.render(30));
    }
    if !r.pre.diffs.is_empty() {
        eprintln!(
            "[detail]   import (pre-state) differences ({}):",
            r.pre.diffs.len()
        );
        eprint!("{}", r.pre.render(30));
    }
}

fn committed_expected() -> &'static Result<Expected, String> {
    static EXPECTED: OnceLock<Result<Expected, String>> = OnceLock::new();
    EXPECTED.get_or_init(|| {
        let e = Expected::load()?;
        let tag = expected::manifest_tag()?;
        if e.corpus_tag != tag {
            return Err(format!(
                "expected-results file is for corpus `{}` but manifest.toml pins `{tag}`: \
                 regenerate it ({UPDATE_ENV}=1) and review the diff",
                e.corpus_tag
            ));
        }
        Ok(e)
    })
}

/// Validate one ledger-rules `test_N` directory.  Returns the outcome
/// so `build.rs`-generated per-vector tests can decide pass/skip/fail.
///
/// LEDGER and NEWEPOCH are applied through dugite's ledger and compared with
/// Haskell's `st_out`; the verdict is then judged against the expected-results
/// contract. The other rules are decode-only.
pub fn check_one_test_dir(test_dir: &Path) -> VectorOutcome {
    let vec = match vector::decode_vector(test_dir) {
        Ok(v) => v,
        Err(e) => return VectorOutcome::Fail(format!("decode: {e}")),
    };
    if let Some(issue) = is_skipped(&vec.rule) {
        return VectorOutcome::Skip(format!("rule={} ({issue})", vec.rule));
    }
    if let Some(replay) = replay_vector(&vec) {
        if std::env::var(DETAIL_ENV).as_deref() == Ok("1") {
            report_detail(&vector_id(test_dir), &replay);
        }
        let expected = match committed_expected() {
            Ok(e) => e,
            Err(e) => return VectorOutcome::Fail(e.clone()),
        };
        let id = vector_id(test_dir);
        return match gate(expected, &id, &replay.verdict).describe(&id) {
            None => VectorOutcome::Pass,
            Some(msg) => VectorOutcome::Fail(msg),
        };
    }
    if let Err(e) = bridge::decode_state(&vec.st_cbor, "st") {
        return VectorOutcome::Fail(format!("st_cbor decode: {e}"));
    }
    match runner::run_vector(&vec) {
        runner::RunOutcome::UtxoDecoded { .. } | runner::RunOutcome::NativeSigDecoded { .. } => {
            VectorOutcome::Pass
        }
        runner::RunOutcome::Skipped { reason } => VectorOutcome::Skip(reason),
        runner::RunOutcome::Failed { detail } => VectorOutcome::Fail(detail),
    }
}

/// Per-rule tallies for the replayed rules.
#[derive(Default)]
struct Tally {
    applied: usize,
    matched: usize,
    diverged: usize,
    harness_errors: usize,
}

/// Coarse class of an observed result, for the summary: the text before the
/// first space for rejections, `STATE_MISMATCH <groups>` for state diffs.
fn class_of(observed: &str) -> String {
    if let Some(rest) = observed.strip_prefix("STATE_MISMATCH ") {
        let post = rest.split_whitespace().next().unwrap_or("");
        format!("STATE_MISMATCH {post}")
    } else {
        observed.to_string()
    }
}

/// What one vector contributed to the run, small enough to keep for all of them.
enum Outcome {
    /// LEDGER / NEWEPOCH, replayed.
    Replayed {
        rule: String,
        verdict: Verdict,
        compared: Vec<&'static str>,
        pre_groups: Vec<&'static str>,
    },
    /// A decode-only rule that passed.
    DecodePassed,
    /// Something that fails the run, with its description.
    Failed(String),
    /// Intentionally skipped (printed when produced).
    Skipped,
}

fn run_one(test_dir: &Path) -> Outcome {
    let id = vector_id(test_dir);
    let vec = match vector::decode_vector(test_dir) {
        Ok(v) => v,
        Err(e) => return Outcome::Failed(format!("{id}: decode: {e}")),
    };
    if let Some(replay) = replay_vector(&vec) {
        return Outcome::Replayed {
            rule: vec.rule.clone(),
            verdict: replay.verdict.clone(),
            compared: replay.pre.compared.clone(),
            pre_groups: replay.pre.groups(),
        };
    }
    if let Some(issue) = is_skipped(&vec.rule) {
        eprintln!(
            "[ledger-rules] SKIP {id} rule={} (tracked: {issue})",
            vec.rule
        );
        return Outcome::Skipped;
    }
    if let Err(e) = bridge::decode_state(&vec.st_cbor, "st") {
        return Outcome::Failed(format!("{id}: st_cbor decode: {e}"));
    }
    match runner::run_vector(&vec) {
        runner::RunOutcome::UtxoDecoded { .. } | runner::RunOutcome::NativeSigDecoded { .. } => {
            Outcome::DecodePassed
        }
        runner::RunOutcome::Skipped { reason } => {
            eprintln!("[ledger-rules] SKIP {id} rule={}: {reason}", vec.rule);
            Outcome::Skipped
        }
        runner::RunOutcome::Failed { detail } => Outcome::Failed(format!("{id}: {detail}")),
    }
}

/// Which half of the vectors a run covers.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Selection {
    /// LEDGER and NEWEPOCH: applied through dugite and compared with `st_out`.
    Replayed,
    /// Every other rule: the signal is decoded, no rule is applied.
    DecodeOnly,
}

/// Whether a vector directory belongs to a replayed rule (`<Rule>/<test_N>`).
fn is_replayed_dir(test_dir: &Path) -> bool {
    test_dir
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| replay::rule_kind(&n.to_string_lossy()).is_some())
        .unwrap_or(false)
}

/// Apply every LEDGER and NEWEPOCH vector under `dir` through dugite's ledger,
/// compare with Haskell's `st_out`, and judge each verdict against the
/// expected-results contract. Replays in parallel (~8,000 vectors).
pub fn run_apply_checks(dir: &Path) {
    run_checks(dir, Selection::Replayed);
}

/// Decode-only checks for the rules that are not replayed (the ExecSpecRule
/// rules and UTXO). Nothing is applied and nothing is compared.
pub fn run_decode_checks(dir: &Path) {
    run_checks(dir, Selection::DecodeOnly);
}

/// Both halves.
pub fn run_all_checks(dir: &Path) {
    run_apply_checks(dir);
    run_decode_checks(dir);
}

fn run_checks(dir: &Path, selection: Selection) {
    let test_cases: Vec<_> = collect_test_case_dirs(dir)
        .into_iter()
        .filter(|d| is_replayed_dir(d) == (selection == Selection::Replayed))
        .collect();

    if test_cases.is_empty() {
        eprintln!(
            "[ledger-rules] SKIP: no test-case directories found under {} \
             (stub mode — run `just regenerate-corpus-local` to populate vectors)",
            dir.display()
        );
        return;
    }

    let replayed = selection == Selection::Replayed;
    let update = replayed && std::env::var(UPDATE_ENV).as_deref() == Ok("1");
    // Updating records what was observed instead of judging it; a vector the
    // harness could not run still fails the run (see the `HarnessError` arm).
    let expected = if !replayed || update {
        None
    } else {
        match committed_expected() {
            Ok(e) => Some(e.clone()),
            Err(e) => panic!("[ledger-rules] {e}"),
        }
    };

    // Replay in parallel: a vector is independent of every other.
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results: std::sync::Mutex<Vec<(usize, Outcome)>> = Default::default();
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(test_dir) = test_cases.get(i) else {
                    break;
                };
                let outcome = run_one(test_dir);
                results.lock().expect("results lock").push((i, outcome));
            });
        }
    });
    let mut results = results.into_inner().expect("results lock");
    results.sort_by_key(|(i, _)| *i);

    let mut decode_passed = 0usize;
    let mut decode_failed = 0usize;
    let mut failures: Vec<String> = Vec::new();
    let mut tallies: std::collections::BTreeMap<String, Tally> = Default::default();
    let mut observed: std::collections::BTreeMap<String, String> = Default::default();
    let mut classes: std::collections::BTreeMap<String, usize> = Default::default();
    let mut seen_ids: std::collections::BTreeSet<String> = Default::default();
    let mut compared_groups: std::collections::BTreeSet<&'static str> = Default::default();
    let mut pre_lossy: std::collections::BTreeMap<&'static str, usize> = Default::default();

    for (i, outcome) in results {
        let id = vector_id(&test_cases[i]);
        match outcome {
            Outcome::Skipped => {}
            Outcome::DecodePassed => decode_passed += 1,
            Outcome::Failed(msg) => {
                decode_failed += 1;
                failures.push(msg);
            }
            Outcome::Replayed {
                rule,
                verdict,
                compared,
                pre_groups,
            } => {
                seen_ids.insert(id.clone());
                compared_groups.extend(compared);
                for g in pre_groups {
                    *pre_lossy.entry(g).or_default() += 1;
                }
                let t = tallies.entry(rule.clone()).or_default();
                t.applied += 1;
                match &verdict {
                    Verdict::Match => t.matched += 1,
                    Verdict::Diverges(o) => {
                        t.diverged += 1;
                        observed.insert(id.clone(), o.clone());
                        *classes
                            .entry(format!("{rule} :: {}", class_of(o)))
                            .or_default() += 1;
                    }
                    Verdict::HarnessError(e) => {
                        t.harness_errors += 1;
                        if expected.is_none() {
                            failures.push(format!("{id}: harness error: {e}"));
                        }
                    }
                }
                if let Some(exp) = &expected {
                    if let Some(msg) = gate(exp, &id, &verdict).describe(&id) {
                        failures.push(msg);
                    }
                }
            }
        }
    }

    if update {
        let new = Expected {
            corpus_tag: expected::manifest_tag().expect("manifest tag"),
            entries: observed.clone(),
        };
        let path = expected::expected_path();
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir expected");
        std::fs::write(&path, new.render()).expect("write expected-results file");
        eprintln!(
            "[ledger-rules] wrote {} entries to {}",
            new.entries.len(),
            path.display()
        );
    } else if let Some(exp) = &expected {
        // A recorded id whose vector is not in the corpus can never be judged.
        for id in exp.entries.keys() {
            if !seen_ids.contains(id) {
                failures.push(format!(
                    "{id}: recorded in the expected-results file but no such vector was replayed"
                ));
            }
        }
    }

    if replayed {
        eprintln!(
            "[ledger-rules] ── replayed rules (applied through dugite, compared with st_out) ──"
        );
        for (rule, t) in &tallies {
            eprintln!(
                "[ledger-rules]   {rule}: applied {} · matched {} · diverged {} · harness errors {}",
                t.applied, t.matched, t.diverged, t.harness_errors
            );
        }
        for (class, n) in &classes {
            eprintln!("[ledger-rules]   diverged  {n:>5}  {class}");
        }
        eprintln!(
            "[ledger-rules] compared groups: {}",
            compared_groups
                .iter()
                .copied()
                .collect::<Vec<_>>()
                .join(", ")
        );
        for (g, n) in &pre_lossy {
            eprintln!(
                "[ledger-rules]   import not faithful for group `{g}` on {n} vector(s) (pre=)"
            );
        }
        eprintln!("[ledger-rules] NOT compared (with reason):");
        for (field, rules, why) in project::NOT_COMPARED {
            eprintln!("[ledger-rules]   - {field} [{rules}]: {why}");
        }
    } else {
        eprintln!(
            "[ledger-rules] decode-only rules (nothing applied, nothing compared): \
             {decode_passed} passed, {decode_failed} failed"
        );
    }

    if !failures.is_empty() {
        let shown: Vec<_> = failures.iter().take(40).cloned().collect();
        panic!(
            "[ledger-rules] {} vector(s) violate the expected-results contract or failed:\n{}{}",
            failures.len(),
            shown.join("\n"),
            if failures.len() > shown.len() {
                format!("\n… {} more", failures.len() - shown.len())
            } else {
                String::new()
            }
        );
    }
}
