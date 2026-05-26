//! Fuzz target for dugite's `PlutusData` CBOR decoding path.
//!
//! `PlutusData` is the universal data type threaded through Plutus scripts as
//! datums, redeemers, and script arguments.  It appears in:
//!   - Transaction witness sets (inline datums, redeemers)
//!   - UTxO outputs (inline datums via tag 24 embedded CBOR)
//!   - N2C `GetUTxO*` query responses
//!   - `LocalTxSubmission` datum witnesses
//!
//! In production the only PlutusData decoder dugite exercises on attacker-
//! controlled bytes is `read_plutus_data` in `dugite-serialization`'s era
//! decoders (era_alonzo / era_babbage / era_conway), reached via
//! `decode_transaction` and `decode_block`. That path has an explicit
//! `MAX_PLUTUS_DATA_DEPTH = 1024` recursion cap.
//!
//! # Why upstream `uplc::plutus_data` is no longer called here
//!
//! Earlier revisions of this target also fed the fuzz bytes directly to
//! `uplc::plutus_data` (the Aiken/pallas-codec decoder). pallas-codec
//! 1.0.0-alpha.6 has an unbounded-recursion bug in its tag(102) PlutusData
//! decoder: `Constr::decode` reads the array header without enforcing the
//! declared length, so adversarial input of the form
//!
//!     d8 66 81 N  ...
//!
//! (tag(102), array(1), then `N` ignored, followed by arbitrary trailing
//! bytes) causes pallas to read PAST the inner array and treat the trailing
//! bytes as further PlutusData fields, recursing without bound. Under
//! AddressSanitizer this overflows the native stack and SIGSEGVs the
//! fuzzer; SIGSEGV is uncatchable from Rust (`catch_unwind` does not
//! intercept signal-driven aborts).
//!
//! Because dugite never invokes `uplc::plutus_data` on attacker-controlled
//! bytes in production — it always goes through dugite's own CBOR-level
//! decoders first, which now have a 1024-level recursion cap — the upstream
//! call here was a fuzz-harness-only liability. It is removed. The target
//! retains full coverage of the production PlutusData decode path via
//! `decode_block` and the per-era `decode_transaction` calls below.
//!
//! Panics in any path are findings.  Decode errors are expected and silently
//! dropped (returning `None`/`Err` is correct behaviour for malformed input).
//!
//! Run with:
//!   cargo +nightly fuzz run fuzz_plutus_data_decode -- -max_total_time=300

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Path 1: dugite-serialization in-house block decoder. If the bytes
    // decode as a block, every transaction body and witness set inside the
    // block is also decoded — exercising era_alonzo / era_babbage /
    // era_conway `read_plutus_data` (the production datum/redeemer path).
    // Most inputs will fail at the outer block envelope, which is expected
    // and fine.
    let _ = dugite_serialization::decode_block(data);

    // Path 2: dugite-serialization in-house transaction decoder, every era.
    // Even when the bytes are not a complete block, they may match a
    // transaction shape for one era. This exercises the same in-house
    // PlutusData decoders without the block-envelope guard.
    for era_id in 0..=6u16 {
        let _ = dugite_serialization::decode_transaction(era_id, data);
    }
});
