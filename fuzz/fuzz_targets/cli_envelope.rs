//! Fuzz the `dugite-cli` key-material parsers (issue #975).
//!
//! `envelope::unwrap_key_bytes` is the strict replacement #935 introduced for
//! four lenient CBOR unwrap heuristics. One of them tested `byte & 0xe0` and so
//! ate the first byte of any raw key starting `0x40..=0x5f` — a 1-in-8 silent
//! corruption of key material, with no error and no diagnostic.
//!
//! A strict replacement for a subtly-wrong parser is exactly what should be
//! pinned by a fuzz target, and it had none: `dugite-cli` was a binary-only
//! crate, so nothing outside it could call these functions at all.
//!
//! ## Properties
//!
//! - neither parser panics on arbitrary bytes or arbitrary text
//! - **no silent truncation**: whatever `unwrap_key_bytes` returns is exactly
//!   `expected_len` bytes AND is a subslice of the input. The old heuristic's
//!   failure mode was returning the right LENGTH from the wrong OFFSET, so
//!   length alone is not a sufficient check — the returned bytes must actually
//!   appear at the position the CBOR header implies.
//! - `parse_inline_verification_key` returns exactly `expected_len` bytes or
//!   an error, never something in between
//!
//! Run with: cargo +nightly fuzz run fuzz_cli_envelope -- -max_total_time=300

#![no_main]

use dugite_cli::envelope::{parse_inline_verification_key, unwrap_key_bytes};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }

    // First byte picks the expected key width, biased onto the widths real
    // Cardano key material uses plus the CBOR header boundary at 23/24.
    const WIDTHS: [usize; 7] = [23, 24, 28, 32, 64, 1, 0];
    let expected_len = WIDTHS[(data[0] as usize) % WIDTHS.len()];
    let payload = &data[1..];

    if let Ok(unwrapped) = unwrap_key_bytes(payload, expected_len, "fuzz key") {
        assert_eq!(
            unwrapped.len(),
            expected_len,
            "unwrap_key_bytes returned {} bytes for an expected_len of {expected_len}",
            unwrapped.len(),
        );

        // The returned slice must be the payload's own tail, at the offset the
        // CBOR header implies — not merely something of the right length.
        // Returning the right length from the wrong offset is precisely what
        // the pre-#935 `& 0xe0` heuristic did.
        let offset = payload.len() - unwrapped.len();
        assert!(
            unwrapped == &payload[offset..],
            "unwrap_key_bytes returned {expected_len} bytes that are not the \
             input's own tail — a silent re-framing of the key material",
        );
    }

    // Text path: bech32, hex, and CBOR-wrapped hex.
    if let Ok(text) = std::str::from_utf8(payload) {
        if let Ok(parsed) = parse_inline_verification_key(text, expected_len, "fuzz key") {
            assert_eq!(
                parsed.len(),
                expected_len,
                "parse_inline_verification_key returned {} bytes for an \
                 expected_len of {expected_len}",
                parsed.len(),
            );
        }
    }
});
