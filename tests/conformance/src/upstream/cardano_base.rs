//! Phase 5 — VRF/KES crypto vector cross-validation.
//!
//! Cross-validates Dugite's crypto primitives against test vectors from
//! `IntersectMBO/cardano-base` (`cardano-crypto-tests/test_vectors/`).
//!
//! ## Status
//!
//! The fixture area (`tests/conformance/upstream/fixtures/cardano-base/`) is
//! currently a stub placeholder. To activate:
//!
//! 1. Trigger the corpus regeneration pipeline (captures `cardano-crypto-tests/
//!    test_vectors/vrf_ver03_*` and `cardano-crypto-tests/test_vectors/kes_*`
//!    from IntersectMBO/cardano-base at the SHA pinned in `sources.toml`).
//!
//! 2. Update `manifest.toml` to point at the new corpus release tag.
//!
//! 3. Run `cargo xtask download-upstream-fixtures`.
//!
//! ## Relationship to existing VRF golden tests
//!
//! `tests/golden/vrf/golden_tests.txt` contains 100 VRF non-integral golden
//! vectors from `cardano-ledger/libs/non-integral/reference/`. Those test the
//! Praos *leader-check arithmetic* (fixed-point ln / exp), not the VRF prove/
//! verify crypto primitives. Phase 5 vectors test the cryptographic layer:
//! keypair derivation, VRF proof generation, and VRF proof verification.

use std::path::Path;

/// Expected format for a VRF vector file line:
/// `<sk_hex> <input_hex> <expected_output_hex> <expected_proof_hex>`
#[derive(Debug)]
pub struct VrfVector {
    pub sk_hex: String,
    pub input_hex: String,
    pub expected_output_hex: String,
    pub expected_proof_hex: String,
}

fn parse_vrf_vectors(text: &str, path: &Path) -> Vec<VrfVector> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 4 {
            eprintln!(
                "[cardano-base] WARN: vrf vector line {} in {} has {} fields (expected 4), skipping",
                i + 1,
                path.display(),
                parts.len()
            );
            continue;
        }
        out.push(VrfVector {
            sk_hex: parts[0].to_owned(),
            input_hex: parts[1].to_owned(),
            expected_output_hex: parts[2].to_owned(),
            expected_proof_hex: parts[3].to_owned(),
        });
    }
    out
}

fn run_vrf_vectors(dir: &Path) {
    let vrf_files: Vec<_> = walkdir(dir)
        .into_iter()
        .filter(|p| {
            p.extension().and_then(|e| e.to_str()) == Some("txt")
                && p.to_str().map(|s| s.contains("vrf")).unwrap_or(false)
        })
        .collect();

    if vrf_files.is_empty() {
        eprintln!(
            "[cardano-base] SKIP VRF: no vrf*.txt files in {} (stub mode)",
            dir.display()
        );
        return;
    }

    let mut total = 0usize;
    for path in &vrf_files {
        let text = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let vectors = parse_vrf_vectors(&text, path);
        total += vectors.len();

        for (i, _v) in vectors.iter().enumerate() {
            // Phase 5 follow-on: call dugite_crypto::vrf::prove() + verify() here,
            // comparing output and proof against expected hex strings.
            // Skeleton: assert vectors decoded non-empty.
            assert!(
                !_v.sk_hex.is_empty(),
                "vrf vector {i} in {} has empty sk",
                path.display()
            );
        }
        eprintln!(
            "[cardano-base] VRF: {} vectors parsed from {}",
            vectors.len(),
            path.file_name().unwrap_or_default().to_string_lossy()
        );
    }
    eprintln!(
        "[cardano-base] VRF: {total} total vectors across {} files",
        vrf_files.len()
    );
}

fn run_kes_vectors(dir: &Path) {
    let kes_files: Vec<_> = walkdir(dir)
        .into_iter()
        .filter(|p| {
            p.extension().and_then(|e| e.to_str()) == Some("txt")
                && p.to_str().map(|s| s.contains("kes")).unwrap_or(false)
        })
        .collect();

    if kes_files.is_empty() {
        eprintln!(
            "[cardano-base] SKIP KES: no kes*.txt files in {} (stub mode)",
            dir.display()
        );
        return;
    }

    let mut total = 0usize;
    for path in &kes_files {
        let data = std::fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        assert!(
            !data.is_empty(),
            "KES vector file {} is empty",
            path.display()
        );
        total += 1;
        eprintln!(
            "[cardano-base] KES: {} bytes in {}",
            data.len(),
            path.file_name().unwrap_or_default().to_string_lossy()
        );
    }
    eprintln!("[cardano-base] KES: {total} vector files checked");
}

fn has_only_readme(dir: &Path) -> bool {
    let files = walkdir(dir);
    files.len() == 1
        && files[0]
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.eq_ignore_ascii_case("readme.txt"))
            .unwrap_or(false)
}

pub fn run_all_checks(dir: &Path) {
    if has_only_readme(dir) {
        eprintln!(
            "[cardano-base] SKIP: fixture area is stub placeholder at {} \
             — run the corpus regeneration pipeline to populate Phase 5 vectors",
            dir.display()
        );
        return;
    }

    run_vrf_vectors(dir);
    run_kes_vectors(dir);
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
