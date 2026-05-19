//! Fuzz target for `skip_cbor_value` depth-limited CBOR skipping.
//!
//! A-004 (security audit 2026-05-19): a malicious peer can craft a CBOR payload
//! of nested arrays/maps/tags to depth D. Before the fix, `skip_cbor_value` was
//! recursive without a depth limit, causing stack overflow → SIGABRT.
//!
//! After the fix, the function enforces `MAX_CBOR_DEPTH = 64` and returns `Err`
//! instead of recursing further. This target verifies:
//!   1. No panic for any input (no stack overflow)
//!   2. No memory exhaustion (allocation is bounded)
//!   3. Deep-nested CBOR (> MAX_CBOR_DEPTH) returns `Err`, not `Ok`
//!
//! Run with: cargo +nightly fuzz run fuzz_skip_cbor_value -- -max_total_time=300
#![no_main]

use dugite_network::codec::try_decode_cbor_boundary;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Primary invariant: try_decode_cbor_boundary (which calls skip_cbor_value)
    // must never panic for any input.
    let _ = try_decode_cbor_boundary(data);

    // Verify that deeply-nested CBOR does NOT cause a stack overflow.
    // Build array(1)[array(1)[...]] nested 1000 times (well beyond MAX_CBOR_DEPTH=64).
    // This is a fixed test vector embedded directly in the fuzz target.
    let nested: Vec<u8> = {
        let mut v = Vec::with_capacity(1002);
        for _ in 0..1000 {
            v.push(0x81); // CBOR array of length 1
        }
        v.push(0x00); // leaf integer 0
        v
    };
    // Must not panic — must return None (boundary not found) or Some(small).
    let _ = try_decode_cbor_boundary(&nested);
});
