//! Shared fixture loader and sentinel check for upstream conformance areas.
//!
//! The fixture root is resolved (in priority order):
//!   1. `DUGITE_UPSTREAM_FIXTURES_DIR` env var
//!   2. `<workspace-root>/tests/conformance/upstream/fixtures`
//!
//! Whether missing fixtures are a hard failure or a silent skip depends on
//! `DUGITE_REQUIRE_UPSTREAM=1`. CI always sets it; dev builds never do.

use std::path::{Path, PathBuf};
use std::{env, fs};

/// Resolve and return the fixture root path.
///
/// Does **not** check whether it exists — callers should use [`require_fixture_dir`]
/// or the [`require_upstream!`] macro for that.
pub fn fixture_root() -> PathBuf {
    if let Ok(dir) = env::var("DUGITE_UPSTREAM_FIXTURES_DIR") {
        return PathBuf::from(dir);
    }
    workspace_root().join("tests/conformance/upstream/fixtures")
}

/// Return the path for a specific area's fixture directory.
pub fn area_dir(area: &str) -> PathBuf {
    fixture_root().join(area)
}

/// Returns `Ok(path)` when the area directory exists and contains at least one file,
/// or `Err(reason)` otherwise.
pub fn require_fixture_dir(area: &str) -> Result<PathBuf, String> {
    let dir = area_dir(area);
    if !dir.exists() {
        return Err(format!("fixture dir missing: {}", dir.display()));
    }
    if !has_any_file(&dir) {
        return Err(format!("fixture dir is empty: {}", dir.display()));
    }
    Ok(dir)
}

/// Returns `true` when the MANIFEST_SHA256 sentinel exists and matches the
/// SHA-256 of the current `manifest.toml`.
pub fn sentinel_matches() -> bool {
    let sentinel = fixture_root().join("MANIFEST_SHA256");
    let manifest = workspace_root().join("tests/conformance/upstream/manifest.toml");
    let Ok(recorded) = fs::read_to_string(&sentinel) else {
        return false;
    };
    let Ok(text) = fs::read_to_string(&manifest) else {
        return false;
    };
    use sha2::{Digest, Sha256};
    let hash: String = Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    hash.trim() == recorded.trim()
}

/// Returns `true` when `DUGITE_REQUIRE_UPSTREAM=1` is set.
pub fn require_mode() -> bool {
    env::var("DUGITE_REQUIRE_UPSTREAM").as_deref() == Ok("1")
}

/// Iterate all files in `dir`, recursively. Returns sorted paths.
pub fn all_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_files(dir, &mut out);
    out.sort();
    out
}

fn collect_files(dir: &Path, acc: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_file() {
            acc.push(path);
        } else if path.is_dir() {
            collect_files(&path, acc);
        }
    }
}

fn has_any_file(dir: &Path) -> bool {
    let Ok(mut rd) = fs::read_dir(dir) else {
        return false;
    };
    rd.any(|e| e.map(|e| e.path().is_file()).unwrap_or(false)) || {
        let Ok(rd2) = fs::read_dir(dir) else {
            return false;
        };
        rd2.flatten()
            .any(|e| e.path().is_dir() && has_any_file(&e.path()))
    }
}

fn workspace_root() -> PathBuf {
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        let cargo = dir.join("Cargo.toml");
        if cargo.exists()
            && fs::read_to_string(&cargo)
                .map(|s| s.contains("[workspace]"))
                .unwrap_or(false)
        {
            return dir;
        }
        if !dir.pop() {
            panic!("workspace root not found");
        }
    }
}

/// Skip-or-panic helper for upstream fixture tests.
///
/// In dev mode (no `DUGITE_REQUIRE_UPSTREAM`): returns `None`, callers
/// should `return` immediately (silently skip).
///
/// In CI mode (`DUGITE_REQUIRE_UPSTREAM=1`): panics with an actionable
/// message so the test is a hard failure.
pub fn check_area(area: &str) -> Option<PathBuf> {
    match require_fixture_dir(area) {
        Ok(dir) => Some(dir),
        Err(reason) => {
            let msg = format!(
                "Upstream fixtures for area '{area}' are not available: {reason}.\n\
                 Run: cargo xtask download-upstream-fixtures"
            );
            if require_mode() {
                panic!("{msg}");
            } else {
                eprintln!("SKIP: {msg}");
                None
            }
        }
    }
}
