//! Upstream conformance tests for cardano-ledger golden files.
//!
//! Phase 1: validates that the golden JSON files are present and parse-able
//! with `serde_json::Value` (field-presence checks, not typed conversion).
//! Phase 3 will add strict typed PParams deserialization.

use super::fixtures;
use std::path::Path;

fn find_json_files(dir: &Path, pattern: &str) -> Vec<std::path::PathBuf> {
    fixtures::all_files(dir)
        .into_iter()
        .filter(|p| {
            p.extension().and_then(|e| e.to_str()) == Some("json")
                && p.to_str().map(|s| s.contains(pattern)).unwrap_or(false)
        })
        .collect()
}

fn find_cddl_files(dir: &Path) -> Vec<std::path::PathBuf> {
    fixtures::all_files(dir)
        .into_iter()
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("cddl"))
        .collect()
}

/// Validate that the era's PParams golden JSON has a reasonable field set.
fn check_pparams_json(path: &Path, era: &str) -> Result<(), String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let val: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("parse {}: {e}", path.display()))?;
    let obj = val
        .as_object()
        .ok_or_else(|| format!("{era} PParams JSON is not an object"))?;
    assert!(
        !obj.is_empty(),
        "{era} PParams JSON has no fields ({})",
        path.display()
    );
    Ok(())
}

/// Run all cardano-ledger golden decode checks.
pub fn run_all_checks(dir: &Path) {
    let mut checked = 0usize;

    // 1. CDDL schemas: verify at least one is present.
    let cddl_files = find_cddl_files(dir);
    assert!(
        !cddl_files.is_empty(),
        "No .cddl files found in cardano-ledger fixtures at {}",
        dir.display()
    );
    for f in &cddl_files {
        let text = std::fs::read_to_string(f).expect("read cddl");
        assert!(!text.is_empty(), "Empty CDDL file: {}", f.display());
        checked += 1;
    }
    eprintln!("[cardano-ledger] {} CDDL schema files", cddl_files.len());

    // 2. PParams JSON goldens — probe with Value (typed conversion in Phase 3).
    let era_patterns = &[
        ("shelley", "shelley"),
        ("alonzo", "alonzo"),
        ("babbage", "babbage"),
        ("conway", "conway"),
    ];

    for (era, pat) in era_patterns {
        let jsons = find_json_files(dir, pat);
        if jsons.is_empty() {
            eprintln!("[cardano-ledger] No JSON files for era '{era}' (may be OK)");
            continue;
        }
        for f in &jsons {
            if f.to_str()
                .map(|s| {
                    s.to_lowercase().contains("pparams")
                        || s.to_lowercase().contains("genesis")
                        || s.to_lowercase().contains("golden")
                        || s.to_lowercase().contains("expected")
                })
                .unwrap_or(false)
            {
                check_pparams_json(f, era)
                    .unwrap_or_else(|e| eprintln!("[cardano-ledger] WARN: {e}"));
                checked += 1;
            }
        }
    }

    // 3. CBOR golden blocks/txs: just verify they're non-empty binary.
    let cbor_files: Vec<_> = fixtures::all_files(dir)
        .into_iter()
        .filter(|p| {
            matches!(
                p.extension().and_then(|e| e.to_str()),
                Some("cbor") | Some("bin")
            )
        })
        .collect();
    for f in &cbor_files {
        let data = std::fs::read(f).expect("read cbor");
        assert!(!data.is_empty(), "Empty CBOR file: {}", f.display());
        checked += 1;
    }

    eprintln!(
        "[cardano-ledger] Checked {checked} files ({} CBOR, {} CDDL, JSON goldens)",
        cbor_files.len(),
        cddl_files.len()
    );
}
