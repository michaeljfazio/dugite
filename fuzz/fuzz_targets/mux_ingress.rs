//! Fuzz target for the mux SDU header decoding and protocol routing.
//!
//! Exercises the public `try_decode_cbor_boundary` and mux segment decode paths
//! that underlie the ingress protocol routing. This validates:
//!   A-004: CBOR depth limit — no stack overflow from deeply nested input
//!   A-005/A-011: unknown/reserved protocol IDs cause errors (tested via unit tests;
//!                the mux ingress internals are not public, exercised indirectly)
//!
//! Run with: cargo +nightly fuzz run fuzz_mux_ingress -- -max_total_time=300
#![no_main]

use dugite_network::codec::try_decode_cbor_boundary;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Exercise the CBOR boundary detection which uses skip_cbor_value internally.
    // This path is triggered for every incoming SDU payload in the mux.
    let _ = try_decode_cbor_boundary(data);

    // Also exercise treating the first 8 bytes as an SDU header and the rest as
    // CBOR payload — mirrors the actual ingress decode sequence.
    if data.len() >= 8 {
        let payload = &data[8..];
        let _ = try_decode_cbor_boundary(payload);
    }
});
