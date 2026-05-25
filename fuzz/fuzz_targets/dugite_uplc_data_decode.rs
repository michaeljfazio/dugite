//! Fuzz target for the **in-house** dugite-uplc `Data::from_cbor` decoder.
//!
//! `dugite_uplc::Data::from_cbor` is the production decoder used by
//! phase-2 script validation to materialise datums, redeemers, and inline
//! UTxO datums. Defensive invariants the decoder documents:
//!   - depth cap (`DATA_MAX_DEPTH`)
//!   - 64-byte chunk cap on definite-length byte / bignum chunks
//!   - clamped `Vec::with_capacity`
//!   - rejection of trailing bytes
//!
//! Any panic, OOM, or successful decode that fails to re-encode round-trip
//! is a real DoS / consensus-divergence finding. `Data` round-trip via
//! `to_cbor()` followed by `from_cbor()` must be the identity on every
//! decodable input (this is required for byte-exact datum hashing).
//!
//! Run with:
//!   cargo +nightly fuzz run fuzz_dugite_uplc_data_decode -- -max_total_time=300

#![no_main]

use dugite_uplc::Data;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(decoded) = Data::from_cbor(data) else {
        return;
    };

    let re_encoded = match decoded.to_cbor() {
        Ok(b) => b,
        Err(e) => panic!("Data::to_cbor failed on a decoded value: {e:?}"),
    };

    match Data::from_cbor(&re_encoded) {
        Ok(rt) => assert_eq!(
            decoded, rt,
            "Data::from_cbor ∘ to_cbor must be the identity"
        ),
        Err(e) => panic!(
            "decoded Data failed to round-trip through to_cbor → from_cbor: \
             {e:?}"
        ),
    }
});
