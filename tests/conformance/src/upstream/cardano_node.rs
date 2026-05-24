//! Upstream conformance tests for cardano-node fixture files.
//!
//! Walks **every** file under `fixtures/cardano-node/**`. Each file in the
//! current corpus pin is JSON (genesis specs, bench profiles, golden query
//! outputs from cardano-testnet). We verify each one parses as a JSON value.

use std::path::Path;

use super::fixtures;

/// Validate one cardano-node fixture file.  Returns `Ok(())` if it
/// parses as a non-empty JSON value, else a human-readable reason.
/// Exposed so `build.rs`-generated per-vector tests can call it.
pub fn check_one_file(path: &Path) -> Result<(), String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if ext != "json" {
        return Err(format!(
            "unexpected file type (extension: {ext:?}), expected .json"
        ));
    }
    let text = std::fs::read_to_string(path).map_err(|e| format!("read error: {e}"))?;
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("JSON parse error: {e}"))?;
    let non_empty = match &value {
        serde_json::Value::Object(o) => !o.is_empty(),
        serde_json::Value::Array(a) => !a.is_empty(),
        serde_json::Value::Null => false,
        _ => true,
    };
    if !non_empty {
        return Err("empty JSON value".to_string());
    }
    Ok(())
}

/// Run all cardano-node fixture checks.
///
/// The current corpus exposes:
///   - bench/cardano-profile/data/test/<profile>/node-specs.json
///   - cardano-testnet/.../golden/node_default_config.json
///   - cardano-testnet/.../golden/queries/*.json
///
/// Every file is JSON; we parse-check each one.
pub fn run_all_checks(dir: &Path) {
    let files = fixtures::all_files(dir);
    assert!(
        !files.is_empty(),
        "No fixture files found under {}.\nRun: cargo xtask download-upstream-fixtures",
        dir.display()
    );

    let mut checked = 0usize;
    let mut by_category: std::collections::BTreeMap<&'static str, usize> =
        std::collections::BTreeMap::new();
    let mut failures: Vec<(std::path::PathBuf, String)> = Vec::new();

    for path in &files {
        match check_one_file(path) {
            Ok(()) => {
                checked += 1;
                let rel = path.strip_prefix(dir).unwrap_or(path);
                let category = rel
                    .components()
                    .next()
                    .and_then(|c| c.as_os_str().to_str())
                    .map(|s| match s {
                        "bench" => "bench/cardano-profile",
                        "cardano-testnet" => "cardano-testnet",
                        other => Box::leak(other.to_string().into_boxed_str()) as &'static str,
                    })
                    .unwrap_or("(root)");
                *by_category.entry(category).or_insert(0) += 1;
            }
            Err(reason) => failures.push((path.clone(), reason)),
        }
    }

    if !failures.is_empty() {
        let mut msg = format!(
            "cardano-node: {} of {} fixtures failed validation:\n",
            failures.len(),
            files.len()
        );
        for (path, reason) in failures.iter().take(20) {
            msg.push_str(&format!("  - {}: {}\n", path.display(), reason));
        }
        if failures.len() > 20 {
            msg.push_str(&format!("  ... and {} more\n", failures.len() - 20));
        }
        panic!("{msg}");
    }

    eprintln!(
        "[cardano-node] Validated {checked}/{} JSON fixtures",
        files.len()
    );
    for (category, count) in &by_category {
        eprintln!("[cardano-node]   {category}: {count}");
    }
}
