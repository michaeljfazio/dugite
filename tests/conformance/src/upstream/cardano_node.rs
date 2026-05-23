//! Upstream conformance tests for cardano-node genesis spec files.
//!
//! Verifies that Alonzo and Conway genesis JSON files parse correctly.

use std::path::Path;

use super::fixtures;

fn check_genesis_json(dir: &Path, name: &str) {
    let files: Vec<_> = fixtures::all_files(dir)
        .into_iter()
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n == name || n.ends_with(&format!("-{name}")))
                .unwrap_or(false)
        })
        .collect();

    if files.is_empty() {
        // Genesis files might be nested under mainnet/preview/preprod — also
        // do a substring match on the path.
        let fallback: Vec<_> = fixtures::all_files(dir)
            .into_iter()
            .filter(|p| {
                p.to_str()
                    .map(|s| s.contains(name.trim_end_matches(".json")))
                    .unwrap_or(false)
            })
            .collect();
        if fallback.is_empty() {
            eprintln!("[cardano-node] {name} not found (may be OK for this upstream pin)");
            return;
        }
        for f in &fallback {
            let text = std::fs::read_to_string(f).expect("read genesis");
            let _: serde_json::Value = serde_json::from_str(&text)
                .unwrap_or_else(|e| panic!("{}: parse error: {e}", f.display()));
        }
        eprintln!(
            "[cardano-node] {} ({} file(s) via path match)",
            name,
            fallback.len()
        );
        return;
    }

    for f in &files {
        let text = std::fs::read_to_string(f).expect("read genesis");
        let val: serde_json::Value = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("{}: parse error: {e}", f.display()));
        let obj = val
            .as_object()
            .unwrap_or_else(|| panic!("{}: not an object", f.display()));
        assert!(!obj.is_empty(), "{}: empty genesis object", f.display());
    }
    eprintln!("[cardano-node] {} ({} file(s))", name, files.len());
}

/// Run all cardano-node genesis spec checks.
pub fn run_all_checks(dir: &Path) {
    check_genesis_json(dir, "alonzo-genesis.json");
    check_genesis_json(dir, "conway-genesis.json");
}
