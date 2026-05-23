//! Phase 6 — Mithril certificate fixture validation.
//!
//! Validates Mithril certificate structures using fixtures from
//! `input-output-hk/mithril`, published via the corpus regeneration pipeline.
//!
//! ## Status
//!
//! The fixture area (`tests/conformance/upstream/fixtures/mithril/`) is
//! currently a stub placeholder. The module falls back to the existing ad-hoc
//! Mithril fixtures in `crates/dugite-node/tests/fixtures/mithril-*.json`.
//!
//! To activate corpus-based fixtures:
//!
//! 1. Trigger the corpus regeneration pipeline (captures Mithril certificate
//!    fixtures from `input-output-hk/mithril` at the SHA pinned in `sources.toml`).
//!
//! 2. Update `manifest.toml` to point at the new corpus release tag.
//!
//! 3. Run `cargo xtask download-upstream-fixtures`.
//!
//! Once corpus-based fixtures are populated, the ad-hoc node-crate fixtures
//! (`crates/dugite-node/tests/fixtures/mithril-*.json`) can be removed — they
//! are superseded by the corpus copies. See the Phase 6 supersession note in
//! the design spec.

use std::path::Path;

fn has_only_readme(dir: &Path) -> bool {
    let files = walkdir(dir);
    files.len() == 1
        && files[0]
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.eq_ignore_ascii_case("readme.txt"))
            .unwrap_or(false)
}

/// Validate that `json_path` is parseable and has the minimum expected fields.
fn check_mithril_json(json_path: &Path, label: &str) {
    let text = std::fs::read_to_string(json_path)
        .unwrap_or_else(|e| panic!("read mithril fixture {}: {e}", json_path.display()));
    let val: serde_json::Value =
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {label}: {e}"));
    assert!(
        !val.is_null(),
        "Mithril fixture {label} parsed as null ({})",
        json_path.display()
    );
    // A certificate list should be a non-empty JSON array.
    if label.contains("list") {
        let arr = val
            .as_array()
            .unwrap_or_else(|| panic!("Mithril {label} expected JSON array"));
        assert!(!arr.is_empty(), "Mithril {label} list is empty");
        // Accept either "hash" (v0 API) or "certificate_hash" (v1 snapshot API).
        for (i, item) in arr.iter().enumerate() {
            assert!(
                item.get("hash").is_some() || item.get("certificate_hash").is_some() || item.get("digest").is_some(),
                "Mithril {label}[{i}] missing any known identifier field (hash / certificate_hash / digest)"
            );
        }
        eprintln!("[mithril] {label}: {} entries", arr.len());
    } else if label.contains("detail") {
        // A detail entry should be a JSON object with a known identifier field.
        let obj = val
            .as_object()
            .unwrap_or_else(|| panic!("Mithril {label} expected JSON object"));
        let id = obj
            .get("hash")
            .or_else(|| obj.get("certificate_hash"))
            .or_else(|| obj.get("digest"))
            .unwrap_or_else(|| panic!("Mithril {label} detail missing any known identifier field"));
        eprintln!("[mithril] {label}: identifier={id}");
    }
}

fn run_corpus_fixtures(dir: &Path) {
    let json_files: Vec<_> = walkdir(dir)
        .into_iter()
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();

    if json_files.is_empty() {
        eprintln!(
            "[mithril] SKIP corpus: no JSON files in {} — falling back to node-crate fixtures",
            dir.display()
        );
        return;
    }

    for path in &json_files {
        let label = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_owned();
        check_mithril_json(path, &label);
    }
    eprintln!(
        "[mithril] corpus: {} JSON fixture(s) validated",
        json_files.len()
    );
}

fn run_node_crate_fallback_fixtures() {
    // These fixtures live in crates/dugite-node/tests/fixtures/ and are
    // included at compile time. We validate them via the filesystem path so
    // we don't need to add a dependency from this crate to dugite-node.
    let fixture_pairs = &[
        ("mithril-421-list", "mithril-421-detail"),
        ("mithril-421-v1-list", "mithril-421-v1-detail"),
    ];

    // Walk up from CARGO_MANIFEST_DIR to find the workspace root.
    let mut base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        let cargo = base.join("Cargo.toml");
        if cargo.exists()
            && std::fs::read_to_string(&cargo)
                .map(|s| s.contains("[workspace]"))
                .unwrap_or(false)
        {
            break;
        }
        if !base.pop() {
            eprintln!("[mithril] SKIP fallback: workspace root not found");
            return;
        }
    }

    let fixtures_dir = base.join("crates/dugite-node/tests/fixtures");
    if !fixtures_dir.exists() {
        eprintln!(
            "[mithril] SKIP fallback: {} does not exist",
            fixtures_dir.display()
        );
        return;
    }

    let mut checked = 0usize;
    for (list_name, detail_name) in fixture_pairs {
        let list_path = fixtures_dir.join(format!("{list_name}.json"));
        let detail_path = fixtures_dir.join(format!("{detail_name}.json"));
        if list_path.exists() {
            check_mithril_json(&list_path, list_name);
            checked += 1;
        }
        if detail_path.exists() {
            check_mithril_json(&detail_path, detail_name);
            checked += 1;
        }
    }
    eprintln!("[mithril] fallback: {checked} node-crate fixture(s) validated");
}

pub fn run_all_checks(dir: &Path) {
    if has_only_readme(dir) {
        eprintln!(
            "[mithril] corpus area is stub at {} — falling back to node-crate fixtures",
            dir.display()
        );
        run_node_crate_fallback_fixtures();
        return;
    }

    run_corpus_fixtures(dir);
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
