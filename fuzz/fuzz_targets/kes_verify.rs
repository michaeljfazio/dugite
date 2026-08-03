//! Fuzz target for `dugite_crypto::kes::kes_verify_bytes`.
//!
//! KES verification takes a 32-byte public key, a u32 period, a 448-byte
//! signature, and an arbitrary message.  The Sum6KesSig deserializer and the
//! underlying KES verifier must never panic — only succeed or return
//! an error.
//!
//! Byte layout consumed from `data`:
//!   [0..32]   = public key (zero-padded if short)
//!   [32..36]  = period as little-endian u32
//!   [36..484] = signature bytes (zero-padded if short)
//!   [484..]   = message
//!
//! Run with: cargo +nightly fuzz run fuzz_kes_verify

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // --- parse pk (32 bytes) ---
    let mut pk = [0u8; 32];
    let pk_end = data.len().min(32);
    pk[..pk_end].copy_from_slice(&data[..pk_end]);

    // --- parse period (4 bytes LE u32) ---
    let period_start = 32;
    let period_end = data.len().min(36);
    let mut period_bytes = [0u8; 4];
    if period_end > period_start {
        period_bytes[..period_end - period_start].copy_from_slice(&data[period_start..period_end]);
    }
    // Clamp to [0, MAX_KES_EVOLUTIONS] so we hit the valid-period path often.
    let raw_period = u32::from_le_bytes(period_bytes);
    let period = raw_period % (dugite_crypto::kes::MAX_KES_EVOLUTIONS as u32 + 1);

    // --- parse sig bytes (448 bytes) ---
    const SIG_SIZE: usize = 448;
    let sig_start = 36;
    let sig_end = data.len().min(sig_start + SIG_SIZE);
    let mut sig_bytes = [0u8; SIG_SIZE];
    if sig_end > sig_start {
        sig_bytes[..sig_end - sig_start].copy_from_slice(&data[sig_start..sig_end]);
    }

    // --- remainder is the message ---
    let msg_start = sig_start + SIG_SIZE;
    let message = if data.len() > msg_start {
        &data[msg_start..]
    } else {
        b"fuzz" as &[u8]
    };

    // Must never panic.
    let _ = dugite_crypto::kes::kes_verify_bytes(&pk, period, &sig_bytes, message);
});
