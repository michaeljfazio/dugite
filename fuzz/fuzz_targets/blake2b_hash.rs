//! Fuzz target for `dugite_primitives::hash` blake2b wrappers.
//!
//! Runs arbitrary-length input through `blake2b_256`, `blake2b_224`, and
//! `blake2b_224_tagged` to catch any length-prefix bugs, off-by-one errors
//! in the incremental hasher, or panics in the underlying blake2 crate.
//!
//! Also exercises the `Blake2b256Hasher` streaming API by splitting the input
//! at an offset derived from the first byte and hashing in two chunks, then
//! comparing against the one-shot result.
//!
//! Run with: cargo +nightly fuzz run fuzz_blake2b_hash

#![no_main]

use dugite_primitives::hash::{blake2b_224, blake2b_224_tagged, blake2b_256, Blake2b256Hasher};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // --- one-shot hashes ---
    let h256 = blake2b_256(data);
    let h224 = blake2b_224(data);
    let _h224_tagged_0 = blake2b_224_tagged(0, data);
    let _h224_tagged_1 = blake2b_224_tagged(1, data);
    let _h224_tagged_3 = blake2b_224_tagged(3, data);

    // Output lengths must always be exact.
    assert_eq!(h256.as_bytes().len(), 32);
    assert_eq!(h224.as_bytes().len(), 28);

    // --- streaming hasher: split at first byte offset (mod len) ---
    let split = if data.is_empty() {
        0
    } else {
        (data[0] as usize) % (data.len() + 1)
    };

    let mut hasher = Blake2b256Hasher::new();
    hasher.update(&data[..split]);
    hasher.update(&data[split..]);
    let h_stream = hasher.finalize();

    // The streaming result must match the one-shot result.
    assert_eq!(h_stream, h256, "streaming blake2b_256 mismatch");
});
