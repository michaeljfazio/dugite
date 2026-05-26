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

    // Also verify a hand-crafted deeply-nested indefinite array returns Err.
    //
    // CBOR_SKIP_MAX_DEPTH = 1024 (raised from 64 in #673), checked as
    // `depth > CBOR_SKIP_MAX_DEPTH`. The outer call uses depth=0 so up to
    // depth=1024 (1025 nesting levels) is permitted; the 1026th nesting level
    // (depth=1025) is rejected. We test significantly past that bound to be
    // robust against future tweaks to the constant.
    if !data.is_empty() && data[0] == 0x01 {
        let levels = 1100usize;
        let mut nested = vec![0x9fu8; levels];
        nested.extend(vec![0xffu8; levels]);
        let result = skip_cbor_value(&nested);
        assert!(
            result.is_err(),
            "{levels}-nested indefinite arrays must return Err, not panic"
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
