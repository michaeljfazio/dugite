//! Phase 4 — ImpSpec ledger-rule replay.
//!
//! This module replays cardano-ledger ImpSpec test vectors through Dugite's
//! ledger engine and compares the resulting state against the expected state
//! encoded in each vector directory.
//!
//! ## Status — corpus generation is blocked by design
//!
//! There are currently NO real ImpSpec CBOR vectors.
//!
//! The ImpSpec dump mechanism (`CONFORMANCE_CBOR_DUMP_PATH`) fires ONLY when
//! the Haskell ledger implementation diverges from the Agda formal spec.
//! Confirmed by oracle research on SHA `ebed62de1ebcd4b13512418d49d17802a193e2c1`,
//! function `checkConformance` in
//! `libs/cardano-ledger-conformance/src/Test/Cardano/Ledger/Conformance/ExecSpecRule/Core.hs`:
//!
//! ```haskell
//! case (implResNorm, agdaResNorm) of
//!     (Right agda, Right impl)
//!       | agda == impl -> pure ()   -- MATCH: no dump
//!     (Left _, Left _) -> pure ()   -- BOTH FAIL: no dump
//!     (agda, impl) -> do            -- DIVERGENCE ONLY: dump fires
//!       ...
//!       CONFORMANCE_CBOR_DUMP_PATH → dumpCbor ...
//! ```
//!
//! Because the reference implementation at any stable pinned SHA passes all of
//! its own ImpSpec tests, running `CONFORMANCE_CBOR_DUMP_PATH=/path cabal test
//! cardano-ledger-conformance` produces ZERO dump files.  ImpSpec is a
//! divergence detector between Haskell STS and Agda MAlonzo, not a fixture
//! generator.
//!
//! Phase 4 requires a redesigned capture approach.  See `HANDOFF.md` for the
//! full analysis and alternative options (standalone Haskell generator,
//! QuickCheck-based generator, Agda/MAlonzo direct invocation, or hand-crafted
//! vectors).
//!
//! ## What IS implemented (ready for real vectors)
//!
//! - 4-file vector format: `vector.rs` reads `conformance_dump_{ctx,env,st,sig}.cbor`
//! - Full NewEpochState structural bridge: `bridge.rs` decodes all 7 fields
//! - Runner: NEWEPOCH epoch-invariant check + UTXO tx decode
//! - Synthetic fixture: `ConwayNEWEPOCH/test_minimal_epoch_advance` exercises decode path
//! - SKIP_LIST: empty (no pending entries — no corpus vectors exist yet)
//!
//! ## Vector format (4 files per test-case directory)
//!
//! ```text
//! <fixtures>/ledger-rules/<Rule>/<test_name>/
//!   conformance_dump_ctx.cbor   -- ExecContext (`F6` CBOR null for NEWEPOCH — EncCBOR () = encodeNull)
//!   conformance_dump_env.cbor   -- Environment (`F6` CBOR null for NEWEPOCH — EncCBOR () = encodeNull)
//!   conformance_dump_st.cbor    -- State (NewEpochState array(7))
//!   conformance_dump_sig.cbor   -- Signal (u64 EpochNo for NEWEPOCH; tx CBOR for UTXO)
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
/// This list is empty because there are no real ImpSpec CBOR vectors yet.
///
/// The ImpSpec dump mechanism fires ONLY when the Haskell ledger implementation
/// diverges from the Agda formal spec — which never happens at the pinned SHA
/// since that SHA is the validated reference implementation.  Running
/// `CONFORMANCE_CBOR_DUMP_PATH=/path cabal test cardano-ledger-conformance`
/// produces ZERO dump files.
///
/// Phase 4 requires a redesigned capture approach. See `HANDOFF.md` for the
/// full analysis and product-owner decision required.
///
/// When real vectors exist, per-rule entries follow this format:
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
                eprintln!(
                    "[ledger-rules] PASS {label} rule={}: NEWEPOCH {initial_epoch} → {signal_epoch}\
                     {acct}{utxo_info}{pool_info} (state shape: {})",
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
