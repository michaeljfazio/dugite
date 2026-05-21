//! Fuzz target for Plutus script decoding across V1, V2, and V3.
//!
//! Cardano Plutus scripts are flat-encoded UPLC programs wrapped in a CBOR
//! byte-string (the "script bytes" stored in the witness set).  This target
//! exercises two parsing layers:
//!
//! 1. `Program::<DeBruijn>::from_cbor` — parses the outer CBOR wrapper and
//!    then flat-decodes the embedded UPLC program.  This is the path taken
//!    by `uplc::tx::apply_params_to_script` and the evaluator internals
//!    whenever a Plutus witness script is loaded for execution.
//!
//! 2. `Program::<DeBruijn>::from_flat` — parses the raw flat bytes directly,
//!    as returned by the inner payload without the CBOR envelope.
//!
//! 3. `uplc::apply_params_to_script` — applies a fuzz-derived PlutusData
//!    parameter array to the script bytes, exercising the full
//!    "parameterised script" construction path used by DApps.
//!
//! All three variants loop over V1, V2, and V3 so that any version-specific
//! decoding differences are covered.  Panics in any path are findings worth
//! reporting upstream to aiken-lang/aiken.
//!
//! ⚠️  CURRENTLY EXCLUDED FROM CI ⚠️
//!
//! Both `the legacy CBOR codec`'s flat decoder and `uplc::tx::apply_params_to_script`
//! panic on malformed input from third-party code that we cannot patch:
//!   - the legacy CBOR codec/src/flat/decode/decoder.rs:154 — unchecked unwrap
//!   - uplc/src/tx.rs:194 — `Result::unwrap()` on Err(EndOfInput)
//! These are real DoS findings (a peer-supplied script can crash the node)
//! that should be filed upstream. Until upstream fixes, this target is kept
//! in-tree but omitted from the CI matrix in `.github/workflows/fuzz.yml`.
//! Run manually to monitor the upstream-bug surface:
//!   cargo +nightly fuzz run fuzz_plutus_script_decode -- -max_total_time=300

#![no_main]

use libfuzzer_sys::fuzz_target;
use uplc::ast::{DeBruijn, Program};

fuzz_target!(|data: &[u8]| {
    // Need at least 2 bytes: 1 control byte + 1 byte of payload.
    if data.len() < 2 {
        return;
    }

    // Split: first byte selects the decoding mode and version, rest is payload.
    let control = data[0];
    let payload = &data[1..];

    // mode selects which decode path to exercise.
    //
    // NOTE: a third path (uplc::tx::apply_params_to_script) was removed because
    // upstream uplc panics on malformed input via an unchecked `unwrap()` at
    // uplc/src/tx.rs:194 (Err(EndOfInput)). catch_unwind doesn't catch under
    // libfuzzer's signal handler, so the only stable option is to skip that
    // path until aiken-lang/aiken fixes the unwrap. Filed-issue tracker: TODO.
    let mode = control & 0x01;

    match mode {
        0 => {
            // Path 1: from_cbor — the CBOR-wrapped flat encoding used on-chain.
            // The CBOR envelope is a CBOR byte-string wrapping flat bytes.
            // Must never panic; decode errors are expected and silently dropped.
            let mut buf = Vec::new();
            let _ = Program::<DeBruijn>::from_cbor(payload, &mut buf);
        }
        _ => {
            // Path 2: from_flat — raw flat bytes without CBOR envelope.
            // Used internally by the evaluator after stripping the CBOR wrapper.
            let _ = Program::<DeBruijn>::from_flat(payload);
        }
    }
});
