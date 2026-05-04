//! Fuzz target for `dugite_crypto::ocert::ocert_signable_bytes` + Ed25519
//! verify — the OCertSignable regression target.
//!
//! Background: a 2026-05-01 soak failure (InvalidSignatureOCERT) was caused by
//! signing a CBOR-encoded `array(3)` instead of the canonical 48-byte raw
//! layout `kes_vkey(32) || seqNo(u64 BE) || kesPeriod(u64 BE)`.  This target
//! exercises the shared helper against arbitrary fuzz bytes to ensure:
//!   1. `ocert_signable_bytes` always produces exactly `kes_vkey.len() + 16` bytes.
//!   2. Ed25519 verification of arbitrarily-modified bytes never panics.
//!
//! Byte layout:
//!   [0..32]   = cold verification key bytes (Ed25519, 32 bytes)
//!   [32..96]  = Ed25519 signature (64 bytes)
//!   [96..128] = KES vkey bytes (32 bytes, used as the "hot" key in the cert)
//!   [128..136] = sequence number (8 bytes LE → u64)
//!   [136..144] = KES period (8 bytes LE → u64)
//!
//! Run with: cargo +nightly fuzz run fuzz_opcert_verify

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // --- cold vkey (32 bytes) ---
    let mut cold_vk_bytes = [0u8; 32];
    let end = data.len().min(32);
    cold_vk_bytes[..end].copy_from_slice(&data[..end]);

    // --- signature (64 bytes) ---
    let mut sig_bytes = [0u8; 64];
    let sig_start = 32;
    let sig_end = data.len().min(96);
    if sig_end > sig_start {
        sig_bytes[..sig_end - sig_start].copy_from_slice(&data[sig_start..sig_end]);
    }

    // --- KES vkey (32 bytes) ---
    let mut kes_vkey = [0u8; 32];
    let kv_start = 96;
    let kv_end = data.len().min(128);
    if kv_end > kv_start {
        kes_vkey[..kv_end - kv_start].copy_from_slice(&data[kv_start..kv_end]);
    }

    // --- sequence number + kes period (8 bytes each, LE) ---
    let mut seq_bytes = [0u8; 8];
    let mut kp_bytes = [0u8; 8];
    if data.len() > 128 {
        let n = (data.len() - 128).min(8);
        seq_bytes[..n].copy_from_slice(&data[128..128 + n]);
    }
    if data.len() > 136 {
        let n = (data.len() - 136).min(8);
        kp_bytes[..n].copy_from_slice(&data[136..136 + n]);
    }
    let sequence_number = u64::from_le_bytes(seq_bytes);
    let kes_period = u64::from_le_bytes(kp_bytes);

    // Build the canonical OCertSignable payload — must never panic.
    let signable = dugite_crypto::ocert::ocert_signable_bytes(&kes_vkey, sequence_number, kes_period);

    // The payload must always be exactly kes_vkey.len() + 16 bytes.
    assert_eq!(signable.len(), kes_vkey.len() + 16);

    // Verify first byte is the KES vkey, not a CBOR header (regression check).
    assert_eq!(signable[0], kes_vkey[0]);

    // Attempt Ed25519 verification with the fuzz-supplied vkey + sig bytes.
    // PaymentVerificationKey::from_bytes already validates the point is on curve.
    if let Ok(vk) = dugite_crypto::keys::PaymentVerificationKey::from_bytes(&cold_vk_bytes) {
        // verify() must never panic — only return Ok or Err.
        let _ = vk.verify(&signable, &sig_bytes);
    }
});
