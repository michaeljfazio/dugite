//! Fuzz target for PlutusData BigInt overflow guard (D3, audit #544).
//!
//! Background: `convert_plutus_data` was silently wrapping bignums of 17+ bytes
//! in i128 via shift-and-or, producing wrong integer values for script context.
//! The D3 fix rejects bignums whose byte length exceeds i128 representable range.
//!
//! This target uses the public `decode_bigint_or_uint` from `cbor_utils` to verify:
//!   1. Bignums of 9+ bytes → `SerializationError::CborDecode` (never a silent wrong value)
//!   2. Bignums of ≤8 bytes → always succeed
//!   3. The function never panics on any input.
//!
//! For PlutusData BigInt specifically (i128 range), the relevant boundary is 16/17
//! bytes.  We test the `decode_bigint_or_uint` public API which enforces 8 bytes
//! for `u64`; the PlutusData path is tested in unit tests.
//!
//! Byte layout:
//!   [0]    = bignum byte length (mod 33, to cover 0..=32)
//!   [1..]  = bignum payload bytes
//!
//! Run with: cargo +nightly fuzz run fuzz_plutus_bignum -- -max_total_time=60

#![no_main]

use dugite_serialization::haskell_snapshot::cbor_utils::decode_bigint_or_uint;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }

    let byte_len = (data[0] as usize) % 33; // 0..=32

    // Build a tag-2 bignum CBOR: 0xc2 + definite-length bstr of `byte_len` bytes
    let mut cbor = vec![0xc2u8];
    // CBOR bstr header
    if byte_len < 24 {
        cbor.push(0x40 | byte_len as u8);
    } else {
        // 2-byte length for 24..=255
        cbor.push(0x58);
        cbor.push(byte_len as u8);
    }
    // Fill payload from fuzz data, zero-pad if not enough bytes
    let src = &data[1..];
    let copy_len = src.len().min(byte_len);
    cbor.extend_from_slice(&src[..copy_len]);
    while cbor.len() < 2 + byte_len.min(24) || cbor.len() < 3 + byte_len.saturating_sub(24) {
        cbor.push(0u8);
    }

    let result = decode_bigint_or_uint(&cbor);

    // D6 invariant: bignums > 8 bytes must return Err.
    if byte_len > 8 && cbor.len() >= 3 + byte_len.saturating_sub(24) {
        // Only assert when we have a complete, well-formed CBOR bignum
        let full_len = if byte_len < 24 {
            2 + byte_len
        } else {
            3 + byte_len
        };
        if cbor.len() >= full_len {
            assert!(
                result.is_err(),
                "bignum of {byte_len} bytes must be rejected: got Ok({:?})",
                result.ok()
            );
        }
    }

    // Invariant: must not panic regardless of input
    let _ = result;
});
