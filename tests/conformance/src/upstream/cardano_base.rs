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

use dugite_crypto::kes::{kes_evolve_to_period, kes_keygen, kes_sign_bytes, kes_verify_bytes};
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
    // Some vectors use the word "empty" for zero-length alpha (e.g., vrf_ver03_standard_10).
    let alpha = if alpha_hex.is_empty() || alpha_hex == "empty" {
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

/// Run VRF cross-validation against all `vrf*` files in `dir`.
///
/// Accepts files with or without a `.txt` extension: cardano-base stores
/// its test vectors as bare files (e.g., `vrf_ver03_generated_1`) with no
/// extension, matching the git-committed names in the upstream repo.
fn run_vrf_vectors(dir: &Path) {
    let vrf_files: Vec<_> = walkdir(dir)
        .into_iter()
        .filter(|p| {
            p.file_name()
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
        // ver field may be "13" or "ietfdraft13" depending on the file format.
        if vec.ver.contains("13") || vec.pi.len() == 128 {
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

/// KES property-based validation using dugite-crypto's KES API.
///
/// cardano-base uses property-based testing for KES and publishes no static
/// vector files. This function exercises the same properties (keygen →
/// sign → evolve → sign → verify) with deterministic inputs so that any
/// regression in dugite-crypto's Sum6KES implementation is caught here.
fn run_kes_property_check() {
    // Property 1: keygen + sign(period=0) + verify.
    let seed = [0x42u8; 32];
    let (sk0, pk) = kes_keygen(&seed).expect("[cardano-base] KES: kes_keygen failed");

    let msg0 = b"Cardano KES period-0 test message";
    let (sig0_bytes, period0) =
        kes_sign_bytes(&sk0, msg0).expect("[cardano-base] KES: sign period 0 failed");
    assert_eq!(period0, 0, "[cardano-base] KES: initial period must be 0");
    kes_verify_bytes(&pk, 0, &sig0_bytes, msg0)
        .expect("[cardano-base] KES: verify period 0 failed");

    // Property 2: evolve to period 5 → sign → verify with original pk.
    let sk5 = kes_evolve_to_period(&sk0, 5).expect("[cardano-base] KES: evolve to period 5 failed");
    let msg5 = b"Cardano KES period-5 test message";
    let (sig5_bytes, period5) =
        kes_sign_bytes(&sk5, msg5).expect("[cardano-base] KES: sign period 5 failed");
    assert_eq!(
        period5, 5,
        "[cardano-base] KES: period after evolve must be 5"
    );
    kes_verify_bytes(&pk, 5, &sig5_bytes, msg5)
        .expect("[cardano-base] KES: verify period 5 with original pk failed");

    // Property 3: period-5 sig must not verify at wrong period or with wrong message.
    assert!(
        kes_verify_bytes(&pk, 0, &sig5_bytes, msg5).is_err(),
        "[cardano-base] KES: period-5 sig must not verify at period 0"
    );
    assert!(
        kes_verify_bytes(&pk, 5, &sig5_bytes, b"wrong message").is_err(),
        "[cardano-base] KES: period-5 sig must not verify wrong message"
    );

    // Property 4: different seeds produce different public keys.
    let seed2 = [0xabu8; 32];
    let (_, pk2) = kes_keygen(&seed2).expect("[cardano-base] KES: second kes_keygen failed");
    assert_ne!(
        pk, pk2,
        "[cardano-base] KES: different seeds must yield different public keys"
    );

    eprintln!(
        "[cardano-base] KES property check PASS: \
         keygen + sign(p=0) + evolve(p=5) + sign + verify all correct"
    );
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
    // KES property check runs unconditionally — it exercises the KES API with
    // deterministic inputs, mirroring cardano-base's property-based KES tests.
    // (cardano-base publishes no static KES vector files; property-testing is
    // the authoritative validation approach for KES in the Haskell ecosystem.)
    run_kes_property_check();

    if has_only_readme(dir) {
        eprintln!(
            "[cardano-base] VRF: fixture area is stub placeholder at {} \
             — run the corpus regeneration pipeline to populate Phase 5 VRF vectors.\n\
             Activation steps:\n\
             1. `just regenerate-corpus-local` (or trigger the GH workflow)\n\
             2. Update manifest.toml to the new release tag\n\
             3. `cargo xtask download-upstream-fixtures`",
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
