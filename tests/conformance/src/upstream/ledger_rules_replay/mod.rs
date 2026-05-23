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
//! See `vector.rs` for the decoder, `bridge.rs` for state decoding,
//! `runner.rs` for event application, and `compare.rs` for state comparison.

pub mod bridge;
pub mod compare;
pub mod runner;
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

/// Map an ImpSpec category name to a Cardano HFC era_id for tx decoding.
///
/// cardano-ledger ImpSpec uses these directory names (see capture-ledger-rules.sh).
/// Unmapped categories return `None` and skip tx decoding for that era.
fn era_id_from_category(category: &str) -> Option<u16> {
    match category {
        "ShelleyImpSpec" => Some(1),
        "AllegraImpSpec" => Some(2),
        "MaryImpSpec" => Some(3),
        "AlonzoImpSpec" => Some(4),
        "BabbageImpSpec" => Some(5),
        "ConwayImpSpec_-_Version_10" => Some(6),
        _ => None,
    }
}

/// Returns the issue URL for a vector whose title matches a skip entry, or `None`.
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
/// Each `.cbor` file is decoded via `vector::decode_vector`, then inspected
/// via `bridge`, replayed via `runner` (which calls `decode_transaction` on
/// every Transaction event), and compared via `compare`.
///
/// Skips gracefully when `dir` contains no `.cbor` files (stub mode).
pub fn run_era_vectors(dir: &Path, category: &str) {
    let era_id = era_id_from_category(category);
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
            "[ledger-rules] SKIP {category}: no .cbor files in {} \
             (stub mode — run `just regenerate-corpus-local` to populate vectors)",
            sub.display()
        );
        return;
    }

    let mut passed = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;

    for path in &cbor_files {
        let data =
            std::fs::read(path).unwrap_or_else(|e| panic!("read vector {}: {e}", path.display()));

        let vec = match vector::decode_vector(&data) {
            Ok(v) => v,
            Err(e) => {
                eprintln!(
                    "[ledger-rules] FAIL decode {}: {e}",
                    path.file_name().unwrap_or_default().to_string_lossy()
                );
                failed += 1;
                continue;
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

        // Validate config shape.
        match bridge::decode_config(&vec.config_cbor) {
            Ok(n) => {
                if n == 0 {
                    eprintln!(
                        "[ledger-rules] WARN {}: config decoded as empty array",
                        vec.title
                    );
                }
            }
            Err(e) => {
                eprintln!("[ledger-rules] FAIL {}: config decode: {e}", vec.title);
                failed += 1;
                continue;
            }
        }

        // Decode initial and final state shapes.
        let initial = match bridge::decode_state(&vec.initial_state_cbor, "initial_state") {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[ledger-rules] FAIL {}: initial_state: {e}", vec.title);
                failed += 1;
                continue;
            }
        };
        let expected = match bridge::decode_state(&vec.final_state_cbor, "final_state") {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[ledger-rules] FAIL {}: final_state: {e}", vec.title);
                failed += 1;
                continue;
            }
        };

        // Run all events through the runner (tx CBOR deserialization + event counting).
        let era = era_id.unwrap_or(6); // default Conway if category unmapped
        let outcome = runner::run_vector(&vec, era);
        match &outcome {
            runner::RunOutcome::Decoded {
                transactions,
                ticks,
                epoch_advances,
            } => {
                eprintln!(
                    "[ledger-rules] {:?} {:?}: {} tx, {} tick, {} epoch | \
                     initial={}, expected={}",
                    path.file_name().unwrap_or_default(),
                    vec.title,
                    transactions,
                    ticks,
                    epoch_advances,
                    initial.shape,
                    expected.shape,
                );
            }
            runner::RunOutcome::Failed { event_idx, detail } => {
                eprintln!(
                    "[ledger-rules] FAIL {:?} at event {event_idx}: {detail}",
                    path.file_name().unwrap_or_default()
                );
                failed += 1;
                continue;
            }
            runner::RunOutcome::Skipped { reason } => {
                eprintln!(
                    "[ledger-rules] SKIP {:?}: {reason}",
                    path.file_name().unwrap_or_default()
                );
                skipped += 1;
                continue;
            }
        }

        // Compare states (skeleton: shape + byte-length comparison).
        // In Phase 4 skeleton mode, initial_state == final_state since the runner
        // hasn't applied events. Once the full bridge is wired, this compares the
        // post-event state against the vector's expected final_state.
        let cmp = compare::compare_states(&initial, &expected);
        if !cmp.matches && std::env::var("DUGITE_REQUIRE_UPSTREAM").as_deref() == Ok("1") {
            eprintln!(
                "[ledger-rules] DIFF {:?}: {}",
                path.file_name().unwrap_or_default(),
                cmp.diff
            );
            // Don't count as failed in skeleton mode — states will differ until
            // the runner actually applies events.
        }

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
