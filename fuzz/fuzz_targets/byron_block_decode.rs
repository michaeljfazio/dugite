//! Fuzz target for the Byron-era block decoders (issue #613).
//!
//! `decode_block` covers all eras but under random mutation the fuzzer
//! converges on Shelley+ era tags because they have richer downstream
//! structure. Byron has been comparatively under-fuzzed; the current focus
//! issue #613 is hardening the Byron block-header decode path.
//!
//! Calls the Byron-specific inner decoders directly via the re-exports in
//! `dugite_serialization::decode_byron_main_block` /
//! `decode_byron_ebb_block`. This bypasses the era envelope so every byte
//! the fuzzer mutates is fed straight into the Byron decoder — maximum
//! coverage density.
//!
//! Byte layout:
//!   [0]      control byte
//!              bit 0 set → main block (decode_byron_main_block)
//!              bit 0 clear → EBB block (decode_byron_ebb_block)
//!   [1..9]   little-endian u64 byron_epoch_length, clamped to [1, 2^20]
//!            to avoid useless overflow inside slot arithmetic
//!   [9..]    inner Byron CBOR — fed verbatim into the Byron decoder
//!
//! Property: the Byron decoders must never panic, abort, or hit an integer
//! overflow on arbitrary input. They may only return Err.
//!
//! Run with:
//!   cargo +nightly fuzz run fuzz_byron_block_decode -- -max_total_time=300

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() < 10 {
        return;
    }
    let control = data[0];
    let epoch_len_raw = u64::from_le_bytes(data[1..9].try_into().unwrap());
    let byron_epoch_length = (epoch_len_raw & 0x000F_FFFF).max(1);
    let inner = &data[9..];

    if (control & 1) != 0 {
        let _ = dugite_serialization::decode_byron_main_block(inner, byron_epoch_length);
    } else {
        let _ = dugite_serialization::decode_byron_ebb_block(inner, byron_epoch_length);
    }
});
