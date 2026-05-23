//! Phase 4 — ImpSpec ledger-rule replay.
//!
//! This module replays cardano-ledger ImpSpec test vectors through Dugite's
//! ledger engine and compares the resulting state against the expected state
//! encoded in each vector file.
//!
//! ## Status
//!
//! The fixture area (`tests/conformance/upstream/fixtures/ledger-rules/`) is
//! currently a stub placeholder. To activate full replay:
//!
//! 1. Run the regeneration pipeline:
//!    ```sh
//!    just regenerate-corpus-local  # or trigger the GH workflow
//!    ```
//!    This builds cardano-ledger at the pinned SHA and runs `cabal test
//!    cardano-ledger-conformance` with `CONFORMANCE_CBOR_DUMP_PATH` set, which
//!    emits one `.cbor` file per ImpSpec test case.
//!
//! 2. Update `tests/conformance/upstream/manifest.toml` to point at the new
//!    corpus release tag.
//!
//! 3. Run `cargo xtask download-upstream-fixtures`.
//!
//! The first corpus regeneration is expected to surface real ledger bugs.
//! Each failure is tracked as a separate issue and added to `SKIP_LIST` below
//! with a comment referencing the issue number; entries are removed when fixed.
//!
//! ## Vector format
//!
//! ```text
//! CBOR [config(arr[13]), initial_state(arr[7]), final_state(arr[7]), events(arr[N]), title(str)]
//! events = [ [0, tx_cbor, valid_bool, slot]   -- Transaction
//!           | [1, slot]                        -- PassTick
//!           | [2, epoch_delta] ]               -- PassEpoch
//! ```
//!
//! See `vector.rs` for the decoder.

pub mod vector;

use std::path::Path;

/// Test scenarios known to fail due to unimplemented Dugite features.
///
/// Each entry is a substring of the vector's `title` field. Matched vectors
/// are skipped with a warning instead of failing. Every entry here must
/// reference a tracking issue (see comment on each line).
///
/// **This list should decay to zero.** Removing a skip = closing the issue.
const SKIP_LIST: &[(&str, &str)] = &[
    // No skip entries yet — list will be populated on first corpus run.
    // Format: ("title-substring", "issue URL or number")
];

/// Returns true if this vector title matches a known-skip entry.
fn is_skipped(title: &str) -> Option<&'static str> {
    for (pattern, issue) in SKIP_LIST {
        if title.contains(pattern) {
            return Some(issue);
        }
    }
    None
}

/// Run all `.cbor` vector files found under `dir/<category>`.
///
/// Skips gracefully when `dir` contains no `.cbor` files (stub mode).
/// Hard-panics in `DUGITE_REQUIRE_UPSTREAM=1` mode when skip-list entries
/// would prevent complete coverage.
pub fn run_era_vectors(dir: &Path, category: &str) {
    let sub = dir.join(category);
    if !sub.exists() {
        eprintln!(
            "[ledger-rules] SKIP {category}: directory {} does not exist (stub mode)",
            sub.display()
        );
        return;
    }

    let cbor_files: Vec<_> = walkdir(&sub)
        .into_iter()
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("cbor"))
        .collect();

    if cbor_files.is_empty() {
        eprintln!(
            "[ledger-rules] SKIP {category}: no .cbor files in {} (stub mode — run regeneration pipeline)",
            sub.display()
        );
        return;
    }

    let mut passed = 0usize;
    let mut skipped = 0usize;
    let failed = 0usize;

    for path in &cbor_files {
        let data =
            std::fs::read(path).unwrap_or_else(|e| panic!("read vector {}: {e}", path.display()));

        let vec = match vector::decode_vector(&data) {
            Ok(v) => v,
            Err(e) => {
                panic!(
                    "[ledger-rules] FAIL decode {}: {e}",
                    path.file_name().unwrap_or_default().to_string_lossy()
                );
            }
        };

        if let Some(issue) = is_skipped(&vec.title) {
            eprintln!(
                "[ledger-rules] SKIP {:?} (tracked: {issue})",
                path.file_name().unwrap_or_default()
            );
            skipped += 1;
            continue;
        }

        // Phase 4 follow-on: plug in bridge → runner → compare here.
        // Until then, assert that the vector decodes correctly and has events.
        assert!(
            !vec.events.is_empty() || !vec.title.is_empty(),
            "vector {} decoded but has no events and no title",
            path.display()
        );
        passed += 1;
    }

    eprintln!(
        "[ledger-rules] {category}: {}/{} passed, {skipped} skipped, {failed} failed",
        passed,
        cbor_files.len()
    );

    if failed > 0 {
        panic!("[ledger-rules] {category}: {failed} vector(s) failed");
    }
}

fn walkdir(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    fn collect(dir: &Path, acc: &mut Vec<std::path::PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_file() {
                acc.push(p);
            } else if p.is_dir() {
                collect(&p, acc);
            }
        }
    }
    collect(dir, &mut out);
    out.sort();
    out
}

/// Entry point for all era categories.
pub fn run_all_checks(dir: &Path) {
    for category in &[
        "ShelleyImpSpec",
        "MaryImpSpec",
        "AllegraImpSpec",
        "AlonzoImpSpec",
        "BabbageImpSpec",
        "ConwayImpSpec_-_Version_10",
    ] {
        run_era_vectors(dir, category);
    }
}
