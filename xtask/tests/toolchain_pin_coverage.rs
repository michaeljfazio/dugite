//! Guards that CI lints with the same compiler developers do.
//!
//! Every workflow used `dtolnay/rust-toolchain@stable` and floated to whatever
//! stable was current, while `rust-toolchain.toml` did not exist and each
//! developer linted with whatever they had installed. Any lint introduced in a
//! newer release then passed `just check` locally and failed CI.
//!
//! That is not hypothetical: on 2026-08-05 a push went red on
//! `clippy::for_kv_map` and, once that was fixed, would have gone red again on
//! `clippy::manual_option_zip` — two lints, one push, both invisible to a local
//! gate running 1.95.0 against a CI running 1.97.0.
//!
//! The pin fixes that, but it introduces a seam: `rust-toolchain.toml` decides
//! which compiler *runs*, while `.github/workflows/*.yml` decides which
//! toolchain the setup action *installs components and cross-compile targets
//! onto*. If those drift, the action installs `llvm-tools-preview` or an
//! `aarch64-unknown-linux-gnu` std onto a toolchain cargo then doesn't use, and
//! the failure is a confusing "can't find crate for `std`" rather than anything
//! naming a version. Two files, no link. This test is the link.
//!
//! Same shape as `fuzz_matrix_coverage.rs`, which watches the seam between
//! declared fuzz targets and the matrix that runs them.

use std::path::{Path, PathBuf};

/// Workflows deliberately NOT on the pinned toolchain, with the reason.
///
/// Adding an entry here is the documented way to opt out. A workflow that
/// pins something else without appearing here fails this test.
const DOCUMENTED_EXCEPTIONS: &[(&str, &str)] = &[(
    "fuzz.yml",
    "cargo-fuzz requires nightly (`-Z sanitizer`); the job invokes `cargo \
     +nightly fuzz`, and `+toolchain` outranks rust-toolchain.toml in rustup's \
     precedence, so the pin does not interfere",
)];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask/ has a parent")
        .to_path_buf()
}

/// The `channel = "..."` value from `rust-toolchain.toml`.
fn pinned_channel(root: &Path) -> String {
    let path = root.join("rust-toolchain.toml");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));

    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("channel") {
            let value = rest
                .trim_start()
                .strip_prefix('=')
                .expect("channel line is `channel = \"...\"`")
                .trim()
                .trim_matches('"');
            assert!(
                !value.is_empty(),
                "rust-toolchain.toml declares an empty channel"
            );
            return value.to_string();
        }
    }
    panic!("{} has no `channel = \"...\"` line", path.display());
}

/// Every `dtolnay/rust-toolchain@<ref>` in a workflow, as (file, ref, line).
fn workflow_toolchain_refs(root: &Path) -> Vec<(String, String, usize)> {
    const MARKER: &str = "dtolnay/rust-toolchain@";
    let dir = root.join(".github/workflows");
    let mut found = Vec::new();

    let entries =
        std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()));

    for entry in entries {
        let path = entry.expect("readable dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("yml") {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .expect("workflow file name is UTF-8")
            .to_string();
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));

        for (idx, line) in text.lines().enumerate() {
            if let Some(pos) = line.find(MARKER) {
                let reference = line[pos + MARKER.len()..]
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .to_string();
                found.push((name.clone(), reference, idx + 1));
            }
        }
    }
    found
}

#[test]
fn every_workflow_pins_the_same_toolchain_as_rust_toolchain_toml() {
    let root = repo_root();
    let channel = pinned_channel(&root);
    let refs = workflow_toolchain_refs(&root);

    assert!(
        !refs.is_empty(),
        "found no `dtolnay/rust-toolchain@` uses — did the setup action change \
         name? This test would then be silently guarding nothing, which is the \
         defect it exists to prevent."
    );

    let mut drifted = Vec::new();
    for (file, reference, line) in &refs {
        if DOCUMENTED_EXCEPTIONS.iter().any(|(f, _)| f == file) {
            continue;
        }
        if reference != &channel {
            drifted.push(format!(
                "  {file}:{line} pins @{reference}, rust-toolchain.toml pins {channel}"
            ));
        }
    }

    assert!(
        drifted.is_empty(),
        "CI would install components/targets onto a different toolchain than \
         cargo actually runs:\n{}\n\nBump both together, or add a documented \
         exception to DOCUMENTED_EXCEPTIONS.",
        drifted.join("\n")
    );
}

#[test]
fn no_workflow_floats_to_stable() {
    let root = repo_root();
    let refs = workflow_toolchain_refs(&root);

    let floating: Vec<_> = refs
        .iter()
        .filter(|(file, r, _)| {
            r == "stable" && !DOCUMENTED_EXCEPTIONS.iter().any(|(f, _)| f == file)
        })
        .map(|(file, _, line)| format!("  {file}:{line}"))
        .collect();

    assert!(
        floating.is_empty(),
        "these workflows float to whatever stable is current, so a lint added \
         upstream turns CI red without any change to this repo — and `just \
         check` cannot reproduce it:\n{}",
        floating.join("\n")
    );
}

#[test]
fn documented_exceptions_are_real_workflows() {
    let root = repo_root();
    for (file, reason) in DOCUMENTED_EXCEPTIONS {
        let path = root.join(".github/workflows").join(file);
        assert!(
            path.exists(),
            "DOCUMENTED_EXCEPTIONS names {file}, which does not exist — a stale \
             exception silently widens what this test permits"
        );
        assert!(
            !reason.trim().is_empty(),
            "{file} is excepted without a reason"
        );
    }
}
