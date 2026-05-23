//! Phase 5 — VRF/KES crypto vector cross-validation.
//!
//! Cross-validates Dugite's crypto primitives against test vectors from
//! `IntersectMBO/cardano-base` (`cardano-crypto-praos/test_vectors/`).
//!
//! ## Vector file format
//!
//! Each file is a series of `key: value` lines (one vector per file):
//!
//! ```text
//! vrf: <vrf_identifier>
//! ver: <version>         # "03" or "13"
//! ciphersuite: ECVRF-ED25519-SHA512-ELL2
//! sk: <32-byte-seed-hex>
//! pk: <32-byte-pubkey-hex>
//! alpha: <variable-hex>  # may be empty
//! pi: <80-byte-hex>      # v03; 128-byte for v13 batchcompat
//! beta: <64-byte-hex>
//! ```
//!
//! ## Version support
//!
//! Dugite-crypto implements ECVRF-ED25519-SHA512-Elligator2 draft-03 (v03),
//! matching Cardano's Ouroboros Praos usage. v13 batch-compatible vectors are
//! skipped with a diagnostic message (different proof format: 128 bytes vs 80).
//!
//! ## KES note
//!
//! cardano-base uses property-based testing for KES, not static vector files.
//! There are no KES fixture files to cross-validate against; the KES
//! correctness guarantee comes from property tests in cardano-base and
//! Dugite's own unit tests.
//!
//! ## Relationship to existing VRF golden tests
//!
//! `tests/golden/vrf/golden_tests.txt` contains 100 VRF non-integral golden
//! vectors from `cardano-ledger/libs/non-integral/reference/`. Those test the
//! Praos *leader-check arithmetic* (fixed-point ln / exp), not the VRF prove/
//! verify crypto primitives. Phase 5 vectors test the cryptographic layer:
//! keypair derivation, VRF proof generation, and VRF proof verification.

use std::collections::HashMap;
use std::path::Path;

use dugite_crypto::vrf::{generate_vrf_keypair_from_secret, generate_vrf_proof, verify_vrf_proof};

/// Parsed VRF test vector from a cardano-crypto-praos test_vectors/ file.
#[derive(Debug)]
struct VrfVector {
    /// "03" or "13"
    ver: String,
    /// 32-byte secret key seed (from `sk:` field)
    sk_seed: Vec<u8>,
    /// 32-byte public key (from `pk:` field)
    pk: Vec<u8>,
    /// Variable-length VRF input (from `alpha:` field; may be empty)
    alpha: Vec<u8>,
    /// Expected VRF proof (80 bytes for v03, 128 bytes for v13)
    pi: Vec<u8>,
    /// Expected VRF output / hash (64 bytes)
    beta: Vec<u8>,
}

/// Parse a single VRF vector file (key: value format).
fn parse_vrf_vector_file(text: &str, path: &Path) -> Option<VrfVector> {
    let mut fields: HashMap<String, String> = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            fields.insert(k.trim().to_lowercase(), v.trim().to_string());
        }
    }

    let ver = fields.get("ver").cloned().unwrap_or_default();
    let Some(sk_hex) = fields.get("sk") else {
        eprintln!(
            "[cardano-base] WARN: missing 'sk' field in {}",
            path.display()
        );
        return None;
    };
    let Some(pk_hex) = fields.get("pk") else {
        eprintln!(
            "[cardano-base] WARN: missing 'pk' field in {}",
            path.display()
        );
        return None;
    };
    let alpha_hex = fields.get("alpha").cloned().unwrap_or_default();
    let Some(pi_hex) = fields.get("pi") else {
        eprintln!(
            "[cardano-base] WARN: missing 'pi' field in {}",
            path.display()
        );
        return None;
    };
    let Some(beta_hex) = fields.get("beta") else {
        eprintln!(
            "[cardano-base] WARN: missing 'beta' field in {}",
            path.display()
        );
        return None;
    };

    let sk_seed = hex::decode(sk_hex).unwrap_or_else(|e| {
        panic!(
            "failed to decode sk in {}: {e}",
            path.file_name().unwrap_or_default().to_string_lossy()
        )
    });
    let pk = hex::decode(pk_hex).unwrap_or_else(|e| {
        panic!(
            "failed to decode pk in {}: {e}",
            path.file_name().unwrap_or_default().to_string_lossy()
        )
    });
    let alpha = if alpha_hex.is_empty() {
        vec![]
    } else {
        hex::decode(&alpha_hex).unwrap_or_else(|e| {
            panic!(
                "failed to decode alpha in {}: {e}",
                path.file_name().unwrap_or_default().to_string_lossy()
            )
        })
    };
    let pi = hex::decode(pi_hex).unwrap_or_else(|e| {
        panic!(
            "failed to decode pi in {}: {e}",
            path.file_name().unwrap_or_default().to_string_lossy()
        )
    });
    let beta = hex::decode(beta_hex).unwrap_or_else(|e| {
        panic!(
            "failed to decode beta in {}: {e}",
            path.file_name().unwrap_or_default().to_string_lossy()
        )
    });

    Some(VrfVector {
        ver,
        sk_seed,
        pk,
        alpha,
        pi,
        beta,
    })
}

/// Run VRF cross-validation against all `vrf*.txt` files in `dir`.
fn run_vrf_vectors(dir: &Path) {
    let vrf_files: Vec<_> = walkdir(dir)
        .into_iter()
        .filter(|p| {
            p.extension().and_then(|e| e.to_str()) == Some("txt")
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with("vrf"))
                    .unwrap_or(false)
        })
        .collect();

    if vrf_files.is_empty() {
        eprintln!(
            "[cardano-base] SKIP VRF: no vrf*.txt files in {} (stub mode — \
             run `just regenerate-corpus-local` or trigger the capture workflow \
             to populate Phase 5 vectors)",
            dir.display()
        );
        return;
    }

    let mut validated_v03 = 0usize;
    let mut skipped_v13 = 0usize;

    for path in &vrf_files {
        let text = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let label = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");

        let Some(vec) = parse_vrf_vector_file(&text, path) else {
            eprintln!("[cardano-base] SKIP {label}: failed to parse vector");
            continue;
        };

        // v13 batch-compatible: proof is 128 bytes; dugite-crypto only implements v03 (80 bytes).
        if vec.ver == "13" || vec.pi.len() == 128 {
            eprintln!(
                "[cardano-base] SKIP {label}: v13 batch-compatible (128-byte proof); \
                 dugite-crypto implements v03 only"
            );
            skipped_v13 += 1;
            continue;
        }

        validate_v03_vector(&vec, label);
        validated_v03 += 1;
    }

    eprintln!(
        "[cardano-base] VRF: {validated_v03} v03 vector(s) validated, \
         {skipped_v13} v13 skipped across {} file(s)",
        vrf_files.len()
    );

    // If we have vector files, at least some must be v03.
    if !vrf_files.is_empty() && validated_v03 == 0 {
        panic!(
            "[cardano-base] No v03 VRF vectors validated from {} files — \
             check vector file format",
            vrf_files.len()
        );
    }
}

/// Validate one VRF v03 vector:
///
/// 1. Keypair derivation: sk_seed → pk_derived must match pk_expected.
/// 2. Proof generation: generate_vrf_proof(sk_seed, alpha) → (pi, beta) must match expected.
/// 3. Proof verification: verify_vrf_proof(pk, pi_expected, alpha) → beta must match expected.
fn validate_v03_vector(vec: &VrfVector, label: &str) {
    assert!(
        vec.sk_seed.len() == 32,
        "[cardano-base] {label}: sk_seed must be 32 bytes, got {}",
        vec.sk_seed.len()
    );
    let sk32: [u8; 32] = vec.sk_seed[..32].try_into().unwrap();

    assert!(
        vec.pk.len() == 32,
        "[cardano-base] {label}: pk must be 32 bytes, got {}",
        vec.pk.len()
    );

    assert!(
        vec.pi.len() == 80,
        "[cardano-base] {label}: v03 pi must be 80 bytes, got {}",
        vec.pi.len()
    );

    assert!(
        vec.beta.len() == 64,
        "[cardano-base] {label}: beta must be 64 bytes, got {}",
        vec.beta.len()
    );

    // 1 — Keypair derivation: derive pk from sk_seed, compare to expected pk.
    let kp = generate_vrf_keypair_from_secret(&sk32);
    assert_eq!(
        kp.public_key.as_ref(),
        vec.pk.as_slice(),
        "[cardano-base] {label}: derived pk mismatch\n  got:      {}\n  expected: {}",
        hex::encode(kp.public_key),
        hex::encode(&vec.pk)
    );

    // 2 — Proof generation: generate_vrf_proof(sk_seed, alpha) must match pi and beta.
    let (pi_computed, beta_computed) = generate_vrf_proof(&sk32, &vec.alpha)
        .unwrap_or_else(|e| panic!("[cardano-base] {label}: proof generation failed: {e}"));

    assert_eq!(
        &pi_computed[..],
        vec.pi.as_slice(),
        "[cardano-base] {label}: generated proof (pi) mismatch\n  got:      {}\n  expected: {}",
        hex::encode(pi_computed),
        hex::encode(&vec.pi)
    );

    assert_eq!(
        &beta_computed[..],
        vec.beta.as_slice(),
        "[cardano-base] {label}: generated output (beta) mismatch\n  got:      {}\n  expected: {}",
        hex::encode(beta_computed),
        hex::encode(&vec.beta)
    );

    // 3 — Proof verification: verify_vrf_proof(pk, pi_expected, alpha) → beta.
    let beta_verified = verify_vrf_proof(&vec.pk, &vec.pi, &vec.alpha)
        .unwrap_or_else(|e| panic!("[cardano-base] {label}: proof verification failed: {e}"));

    assert_eq!(
        &beta_verified[..],
        vec.beta.as_slice(),
        "[cardano-base] {label}: verified output (beta) mismatch\n  got:      {}\n  expected: {}",
        hex::encode(beta_verified),
        hex::encode(&vec.beta)
    );

    eprintln!("[cardano-base] PASS {label}: keypair + prove + verify all match");
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
             — run the corpus regeneration pipeline to populate Phase 5 VRF vectors.\n\
             Activation steps:\n\
             1. `just regenerate-corpus-local` (or trigger the GH workflow)\n\
             2. Update manifest.toml to the new release tag\n\
             3. `cargo xtask download-upstream-fixtures`\n\
             Note: KES has no static test vectors (property-based only).",
            dir.display()
        );
        return;
    }

    run_vrf_vectors(dir);
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
