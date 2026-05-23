//! Phase 4 — ImpSpec ledger-rule replay.
//!
//! This module replays cardano-ledger ImpSpec test vectors through Dugite's
//! ledger engine and compares the resulting state against the expected state
//! encoded in each vector directory.
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
//!   conformance_dump_st.cbor      -- State (NewEpochState array(7))
//!   conformance_dump_sig.cbor     -- Signal (EpochNo for NEWEPOCH; tx CBOR for LEDGER)
//!   conformance_dump_st_out.cbor  -- Haskell expected final state (absent when STS rejects)
//! ```
//!
//! Each test-case directory under a rule directory is decoded as one `ImpVector`.
//! See `vector.rs` for the decoder, `bridge.rs` for state/signal decoding,
//! `runner.rs` for validation, and `compare.rs` for state comparison.

pub mod bridge;
pub mod compare;
pub mod runner;
pub mod vector;

use std::path::Path;

/// Test scenarios known to fail due to unimplemented Dugite features.
///
/// Each entry is a tuple of (rule_substring, issue_url).  The pattern is
/// matched as a substring of the rule name.  Every entry must reference a
/// tracking issue.  **This list should decay to zero.**  Removing a skip =
/// closing the issue.
///
/// ## Current state
///
/// This list is empty — vectors are produced by the patched ImpSpec approach
/// (`patch-impspec-core.py`) and all current NEWEPOCH/LEDGER tests pass or
/// are skipped (for rules without a handler).
///
/// Per-rule entries follow this format when a divergence is found:
/// ```
/// ("ConwayNEWEPOCH", "https://github.com/michaeljfazio/dugite/issues/NNN"),
/// ```
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
fn collect_test_case_dirs(rule_dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(rule_dir) else {
        return out;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // This is a rule directory (e.g. ConwayNEWEPOCH).
            // Collect test-case dirs inside it.
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

/// Create the 4 CBOR fixture files for the minimal NEWEPOCH synthetic test.
///
/// Directory layout:
/// ```text
/// dir/ConwayNEWEPOCH/test_minimal_epoch_advance/
///   conformance_dump_ctx.cbor   — 0xF6 (CBOR null — Haskell `EncCBOR () = encodeNull`)
///   conformance_dump_env.cbor   — 0xF6 (CBOR null — Haskell `EncCBOR () = encodeNull`)
///   conformance_dump_st.cbor    — NewEpochState array(7), EpochNo=0
///   conformance_dump_sig.cbor   — 0x01 (EpochNo = 1)
/// ```
///
/// **Encoding note for ctx/env**: The Haskell `EncCBOR` instance for `()` is
/// `encodeNull`, which emits the CBOR null byte `0xF6`.  The NEWEPOCH STS rule
/// uses `()` for both its context and environment, so both files contain a
/// single `0xF6` byte.  The earlier `0x80` (empty array) was incorrect.
///
/// The state file is hand-encoded as raw CBOR bytes (each byte is annotated
/// with its CBOR meaning in the inline comments).
pub fn create_minimal_newepoch_fixture(dir: &Path) {
    let test_dir = dir
        .join("ConwayNEWEPOCH")
        .join("test_minimal_epoch_advance");
    std::fs::create_dir_all(&test_dir).expect("create fixture dir");

    // ctx and env: CBOR null (0xF6) — Haskell `EncCBOR () = encodeNull`
    // NOT 0x80 (empty array): the NEWEPOCH context and environment are both
    // the unit type `()`, which Haskell serializes as CBOR null, not empty array.
    for name in &["conformance_dump_ctx.cbor", "conformance_dump_env.cbor"] {
        std::fs::write(test_dir.join(name), [0xF6u8]).expect("write ctx/env");
    }

    // sig: CBOR uint(1) = 0x01
    std::fs::write(test_dir.join("conformance_dump_sig.cbor"), [0x01u8]).expect("write sig");

    // st: NewEpochState array(7) with EpochNo=0 in field[0].
    //
    // Minimal encoding:
    //   87          — array(7)
    //   00          — [0] EpochNo = 0 (uint)
    //   a0          — [1] BlocksMade(prev) = empty map
    //   a0          — [2] BlocksMade(cur) = empty map
    //   84          — [3] EpochState = array(4)
    //     82 00 00  —   [3.0] AccountState = array(2): treasury=0, reserves=0
    //     80        —   [3.1] LedgerState = empty array (stub)
    //     80        —   [3.2] Snapshots = empty array (stub)
    //     80        —   [3.3] NonMyopic = empty array (stub)
    //   80          — [4] StrictMaybe = array(0) = Nothing
    //   a0          — [5] PoolDistr = empty map
    //   80          — [6] stashedAVVM = array(0) (Conway: always empty)
    let st_bytes: &[u8] = &[
        0x87, // array(7)
        0x00, // [0] EpochNo = 0
        0xa0, // [1] BlocksMade(prev) = {}
        0xa0, // [2] BlocksMade(cur) = {}
        0x84, // [3] EpochState = array(4)
        0x82, 0x00, 0x00, // [3.0] AccountState = [0, 0]
        0x80, // [3.1] LedgerState stub
        0x80, // [3.2] Snapshots stub
        0x80, // [3.3] NonMyopic stub
        0x80, // [4] StrictMaybe = Nothing
        0xa0, // [5] PoolDistr = {}
        0x80, // [6] stashedAVVM = []
    ];
    std::fs::write(test_dir.join("conformance_dump_st.cbor"), st_bytes).expect("write st");
}

/// Run all test-case directories found under `dir`.
///
/// `dir` is `<fixtures>/ledger-rules/`. Each subdirectory of the form
/// `<Rule>/<test_name>/` with all 4 CBOR files present is treated as one
/// test vector.
///
/// Skips gracefully when no test-case directories are found (stub mode),
/// **except** when the synthetic fixture has been created (which always
/// provides at least one test case).
pub fn run_all_checks(dir: &Path) {
    // Ensure at least the synthetic NEWEPOCH fixture exists so the test suite
    // always has one real vector to exercise.
    let synthetic_dir = dir
        .join("ConwayNEWEPOCH")
        .join("test_minimal_epoch_advance");
    if !is_test_case_dir(&synthetic_dir) {
        create_minimal_newepoch_fixture(dir);
    }

    let test_cases = collect_test_case_dirs(dir);

    if test_cases.is_empty() {
        eprintln!(
            "[ledger-rules] SKIP: no test-case directories found under {} \
             (stub mode — run `just regenerate-corpus-local` to populate vectors)",
            dir.display()
        );
        return;
    }

    let mut passed = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;

    for test_dir in &test_cases {
        let label = format!(
            "{}/{}",
            test_dir
                .parent()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy())
                .unwrap_or_default(),
            test_dir
                .file_name()
                .map(|n| n.to_string_lossy())
                .unwrap_or_default()
        );

        // Decode the 4 CBOR files.
        let vec = match vector::decode_vector(test_dir) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[ledger-rules] FAIL decode {label}: {e}");
                failed += 1;
                continue;
            }
        };

        // Skip-list check.
        if let Some(issue) = is_skipped(&vec.rule) {
            eprintln!(
                "[ledger-rules] SKIP {label} rule={} (tracked: {issue})",
                vec.rule
            );
            skipped += 1;
            continue;
        }

        // Validate the state shape.
        let state = match bridge::decode_state(&vec.st_cbor, "st") {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[ledger-rules] FAIL {label}: st_cbor decode: {e}");
                failed += 1;
                continue;
            }
        };

        // Run the rule-specific validation.
        let outcome = runner::run_vector(&vec);
        match &outcome {
            runner::RunOutcome::NewEpochValidated {
                initial_epoch,
                signal_epoch,
                treasury,
                reserves,
                utxo_count,
                pool_count,
                final_state_validated,
            } => {
                let acct = match (treasury, reserves) {
                    (Some(t), Some(r)) => {
                        format!(" treasury={t} reserves={r}")
                    }
                    _ => String::new(),
                };
                let utxo_info = utxo_count
                    .map(|n| format!(" utxos={n}"))
                    .unwrap_or_default();
                let pool_info = pool_count
                    .map(|n| format!(" pools={n}"))
                    .unwrap_or_default();
                let st_out_info = if *final_state_validated {
                    " [st_out=ok]"
                } else {
                    " [st_out=absent]"
                };
                eprintln!(
                    "[ledger-rules] PASS {label} rule={}: NEWEPOCH {initial_epoch} → {signal_epoch}\
                     {acct}{utxo_info}{pool_info}{st_out_info} (state shape: {})",
                    vec.rule, state.shape
                );
                passed += 1;
            }
            runner::RunOutcome::UtxoDecoded { era_id, tx_bytes } => {
                eprintln!(
                    "[ledger-rules] PASS {label} rule={}: UTXO tx decoded \
                     (era_id={era_id}, {tx_bytes} bytes, state shape: {})",
                    vec.rule, state.shape
                );
                passed += 1;
            }
            runner::RunOutcome::Skipped { reason } => {
                eprintln!("[ledger-rules] SKIP {label} rule={}: {reason}", vec.rule);
                skipped += 1;
            }
            runner::RunOutcome::Failed { detail } => {
                eprintln!("[ledger-rules] FAIL {label} rule={}: {detail}", vec.rule);
                failed += 1;
            }
        }
    }

    eprintln!(
        "[ledger-rules] {}/{} passed, {skipped} skipped, {failed} failed",
        passed,
        test_cases.len()
    );

    if failed > 0 {
        panic!("[ledger-rules] {failed} vector(s) failed");
    }
}
