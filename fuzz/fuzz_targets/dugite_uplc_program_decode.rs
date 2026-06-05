//! Fuzz target for the **in-house** dugite-uplc program decoder.
//!
//! `dugite_uplc::Program::{from_cbor, from_flat}` is the production decoder
//! used by phase-2 script validation in dugite-ledger; the previously-existing
//! `fuzz_plutus_script_decode` only exercises the upstream Aiken `uplc` crate,
//! which is **not** the implementation that runs in production. Any panic or
//! out-of-bounds read in this target is a real DoS finding.
//!
//! Defensive properties checked here:
//!   1. Neither `from_cbor` nor `from_flat` may panic on arbitrary input.
//!   2. If decoding succeeds, the resulting program must round-trip via
//!      `to_flat()` and re-decode to a structurally identical program. This
//!      guards against decoder/encoder asymmetry that has historically been
//!      a source of subtle script-hash divergences.
//!
//! Byte layout:
//!   [0]    mode select:
//!            bit 0 clear → exercise `Program::from_cbor`
//!            bit 0 set   → exercise `Program::from_flat`
//!   [1..]  payload bytes.
//!
//! Run with:
//!   cargo +nightly fuzz run fuzz_dugite_uplc_program_decode -- -max_total_time=300
//!
//! ## Stack / ASAN note
//!
//! cargo-fuzz builds with AddressSanitizer, which triples the per-frame stack
//! cost (shadow memory + redzones). `stacker::maybe_grow` extends the OS stack
//! via `mmap`, but ASAN's shadow-memory layout can conflict with the newly
//! mapped segment before `stacker` gets a chance to establish it, resulting in
//! an ASAN DEADLY SIGNAL rather than a clean stack extension.
//!
//! The production decoder allows up to `FLAT_MAX_DEPTH = 32 768` levels (the
//! theoretical max for a 16 KiB script). Under ASAN, empirically, depth beyond
//! ~2 048 levels triggers the conflict. We therefore clamp the flat payload to
//! 1 KiB in this fuzz target (≤ 2 048 bits / 4 bits per term tag = ≤ 2 048
//! theoretical levels), which gives full coverage of real on-chain scripts
//! (typical DeFi validators are < 500 levels deep) while staying safe.
//!
//! The payload cap is applied AFTER the version bytes have been read from the
//! fuzzer's `data` slice (byte 0 is the mode selector, not counted), so the
//! fuzzer still explores the full mode space and version-triple encoding.

#![no_main]

use dugite_uplc::Program;
use libfuzzer_sys::fuzz_target;

/// Maximum flat-payload length this fuzz harness passes to the decoder.
///
/// 1 KiB → ≤ 2 048 term-tag bits → ≤ 2 048 recursion levels.
/// Safe under ASAN (empirical limit ≈ 2 048); well above any realistic
/// on-chain script depth (typical ≤ 500 levels).
const FUZZ_MAX_FLAT_BYTES: usize = 1024;

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }
    let mode = data[0] & 1;
    // Clamp payload to FUZZ_MAX_FLAT_BYTES to avoid ASAN stack-overflow on
    // pathologically deep inputs.  See module-level doc for rationale.
    let raw_payload = &data[1..];
    let payload = if raw_payload.len() > FUZZ_MAX_FLAT_BYTES {
        &raw_payload[..FUZZ_MAX_FLAT_BYTES]
    } else {
        raw_payload
    };

    let decoded = if mode == 0 {
        Program::from_cbor(payload)
    } else {
        Program::from_flat(payload)
    };

    let Ok(program) = decoded else {
        return;
    };

    // Round-trip via flat encoding.
    let re_encoded = match program.to_flat() {
        Ok(b) => b,
        // Encoder failures on a valid program are themselves a finding.
        Err(e) => panic!("Program::to_flat failed on decoded program: {e:?}"),
    };
    match Program::from_flat(&re_encoded) {
        Ok(round_trip) => assert_eq!(
            program,
            round_trip,
            "Program::from_flat ∘ to_flat must be the identity \
             (mode={mode}, len={len})",
            len = payload.len()
        ),
        Err(e) => panic!(
            "decoded program failed to re-decode after to_flat (mode={mode}, \
             len={len}): {e:?}",
            len = payload.len()
        ),
    }
});
