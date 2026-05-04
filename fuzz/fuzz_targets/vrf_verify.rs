//! Fuzz target for `dugite_crypto::vrf::verify_vrf_proof`.
//!
//! Feeds arbitrary bytes as (vrf_vkey, proof_bytes, seed) to the VRF verifier.
//! The verifier must never panic regardless of input — only return Ok or Err.
//!
//! The exact byte split is: first 32 bytes = vrf_vkey, next 80 bytes = proof,
//! remainder = seed.  If data is shorter, slices are zero-padded to the
//! required lengths so the hot path (80-byte proof + 32-byte key) is always
//! exercised when the fuzzer generates ≥ 112 bytes.
//!
//! Run with: cargo +nightly fuzz run fuzz_vrf_verify

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Partition fuzz bytes: [0..32] = vkey, [32..112] = proof, [112..] = seed
    let mut vkey = [0u8; 32];
    let mut proof = [0u8; 80];

    let vkey_end = data.len().min(32);
    vkey[..vkey_end].copy_from_slice(&data[..vkey_end]);

    let proof_start = vkey_end;
    let proof_end = data.len().min(112);
    if proof_end > proof_start {
        proof[..proof_end - proof_start].copy_from_slice(&data[proof_start..proof_end]);
    }

    let seed = if data.len() > 112 { &data[112..] } else { &[] };

    // Must never panic — only Ok or Err.
    let _ = dugite_crypto::vrf::verify_vrf_proof(&vkey, &proof, seed);

    // Also exercise the hash-extraction path with arbitrary bytes.
    let _ = dugite_crypto::vrf::vrf_proof_to_hash(&proof);

    // And the leader-check arithmetic with the proof bytes as output.
    let _ = dugite_crypto::vrf::check_leader_value(&proof, 0.001, 0.05);
    let _ = dugite_crypto::vrf::check_leader_value_tpraos(&proof, 0.001, 0.05);
});
