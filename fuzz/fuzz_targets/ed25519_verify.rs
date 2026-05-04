//! Fuzz target for `dugite_crypto::keys::PaymentVerificationKey::verify`.
//!
//! Exercises the Ed25519 verification wrapper with arbitrary public-key bytes,
//! arbitrary 64-byte signatures, and arbitrary messages.  Neither key parsing
//! nor signature verification must ever panic — only succeed or return a typed
//! error.
//!
//! Byte layout:
//!   [0..32]  = Ed25519 public key (32 bytes, may be an invalid curve point)
//!   [32..96] = Ed25519 signature (64 bytes, arbitrary)
//!   [96..]   = message
//!
//! Run with: cargo +nightly fuzz run fuzz_ed25519_verify

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // --- public key (32 bytes) ---
    let mut pk_bytes = [0u8; 32];
    let pk_end = data.len().min(32);
    pk_bytes[..pk_end].copy_from_slice(&data[..pk_end]);

    // --- signature (64 bytes) ---
    let mut sig_bytes = [0u8; 64];
    let sig_start = 32;
    let sig_end = data.len().min(96);
    if sig_end > sig_start {
        sig_bytes[..sig_end - sig_start].copy_from_slice(&data[sig_start..sig_end]);
    }

    // --- message (remainder) ---
    let message = if data.len() > 96 {
        &data[96..]
    } else {
        b"" as &[u8]
    };

    // from_bytes validates the key is a valid Ed25519 point; may return Err.
    // Must never panic.
    if let Ok(vk) = dugite_crypto::keys::PaymentVerificationKey::from_bytes(&pk_bytes) {
        // Verify must never panic, only Ok or Err.
        let _ = vk.verify(message, &sig_bytes);
    }

    // Also exercise from_bytes alone with the first 32 fuzz bytes.
    let _ = dugite_crypto::keys::PaymentVerificationKey::from_bytes(&pk_bytes);
});
