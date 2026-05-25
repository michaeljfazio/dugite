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

#![no_main]

use dugite_uplc::Program;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }
    let mode = data[0] & 1;
    let payload = &data[1..];

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
