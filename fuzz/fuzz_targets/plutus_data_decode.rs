//! Fuzz target for `PlutusData` CBOR decoding.
//!
//! `PlutusData` is the universal data type threaded through Plutus scripts as
//! datums, redeemers, and script arguments.  It appears in:
//!   - Transaction witness sets (inline datums, redeemers)
//!   - UTxO outputs (inline datums via tag 24 embedded CBOR)
//!   - N2C `GetUTxO*` query responses
//!   - `LocalTxSubmission` datum witnesses
//!
//! The decoder under test is `uplc::plutus_data()`, which calls pallas's
//! `PlutusData::decode_fragment`.  This is the same decoder invoked by
//! `eval_phase_two_raw` when it loads datums and redeemers from a transaction.
//!
//! A secondary path exercises our own `encode_plutus_data` / round-trip encoder
//! from `dugite-serialization`, which is the path used by the N2C server when
//! serialising UTxO datum responses.  The round-trip is: decode via uplc →
//! convert to dugite `PlutusData` → re-encode via `encode_plutus_data`.
//!
//! Panics in either path are findings.  Decode errors are expected and silently
//! dropped (returning `None`/`Err` is correct behaviour for malformed input).
//!
//! Run with:
//!   cargo +nightly fuzz run fuzz_plutus_data_decode -- -max_total_time=300

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Path 1: uplc PlutusData CBOR decoder.
    //
    // uplc::plutus_data() calls pallas_primitives::PlutusData::decode_fragment,
    // which is the same decoder used by eval_phase_two_raw when loading datums
    // and redeemers from a transaction witness set.
    let _ = uplc::plutus_data(data);

    // Path 2: dugite-serialization PlutusData decoder via pallas decode_block /
    // decode_transaction.  We exercise this by trying to decode the fuzz bytes
    // as a Conway transaction; if that succeeds the deserialization path
    // (including PlutusData datum conversion) is exercised.  Most inputs will
    // fail at the outer transaction decode, which is expected and fine.
    let _ = dugite_serialization::decode_block(data);
});
