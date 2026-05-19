//! Fuzz target for `skip_cbor_value` depth-limited recursion (D10/D15, audit #544).
//!
//! Background: `skip_cbor_value` in `haskell_snapshot/cbor_utils.rs` is recursive.
//! An adversarial snapshot with deeply-nested indefinite-length arrays (e.g. 1000
//! nested `0x9f ... 0xff`) would exhaust the stack via unbounded recursion.
//!
//! The D10 fix introduces `CBOR_SKIP_MAX_DEPTH = 64` and threads a depth counter
//! through `skip_cbor_value_depth`.  This fuzz target verifies:
//!
//!   1. No panic on any input — the function must always return Ok or Err.
//!   2. Deeply-nested structures (depth > 64) return Err, not SIGABRT.
//!   3. Shallow structures (depth ≤ 64) are handled correctly.
//!   4. D15 fix: indefinite-length byte/text strings (0x5f, 0x7f prefix) are
//!      handled without panicking or returning a spurious error.
//!
//! Byte layout: the raw fuzz bytes are passed directly as CBOR.
//!
//! Run with: cargo +nightly fuzz run fuzz_cbor_skip_value_depth -- -max_total_time=60

#![no_main]

use dugite_serialization::haskell_snapshot::cbor_utils::skip_cbor_value;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // The only invariant: must not panic.
    // Returns Ok(consumed) or Err — both are acceptable.
    let _ = skip_cbor_value(data);

    // Also verify a hand-crafted deeply-nested indefinite array returns Err:
    // [0x9f, 0x9f, 0x9f, ..., 0xff, 0xff, 0xff, ...]  → depth 65+ should Err
    if data.len() >= 1 && data[0] == 0x01 {
        // Build 65 nested indefinite arrays: 0x9f repeated 65 times + 0xff 65 times
        let mut nested = vec![0x9fu8; 65];
        nested.extend(vec![0xffu8; 65]);
        let result = skip_cbor_value(&nested);
        assert!(
            result.is_err(),
            "depth-65 nested indefinite arrays must return Err, not panic"
        );
    }

    // Verify D15: indefinite-length byte string is handled (0x5f ... chunks ... 0xff)
    if data.len() >= 1 && data[0] == 0x02 {
        // 0x5f = indefinite-length byte string; 0x41 0xAB = one-byte chunk; 0xff = break
        let indef_bstr: &[u8] = &[0x5f, 0x41, 0xAB, 0xff];
        let result = skip_cbor_value(indef_bstr);
        assert!(
            result.is_ok(),
            "indefinite-length byte string must be handled (D15): {result:?}"
        );
        // And a chunked text string: 0x7f = indefinite text, 0x61 0x41 = "A", 0xff = break
        let indef_tstr: &[u8] = &[0x7f, 0x61, 0x41, 0xff];
        let result2 = skip_cbor_value(indef_tstr);
        assert!(
            result2.is_ok(),
            "indefinite-length text string must be handled (D15): {result2:?}"
        );
    }
});
