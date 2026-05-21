//! Fuzz target for the bounded tx-metadata CBOR decoder (#554, D7 from
//! audit #544).
//!
//! Background: tx metadata is a recursive CBOR structure (Int/Bytes/Text/
//! List/Map) submitted by peers. A naive decoder calling
//! `Vec::with_capacity(declared_len)` on the wire-supplied length header
//! could be exploited by a peer declaring `array(u64::MAX)` to force a huge
//! allocation attempt before the decode loop runs.
//!
//! The hardened decoder (`decode_bounded::decode_metadatum_bounded`) caps:
//! - declared length against `MAX_METADATA_ENTRIES`
//! - declared length against remaining input bytes (physical realisability)
//! - recursion depth against `MAX_METADATA_DEPTH`
//! - field bytes against `MAX_METADATA_FIELD_BYTES`
//! - rejects indefinite-length arrays/maps/strings
//!
//! This fuzz target stresses the decoder with arbitrary inputs and asserts:
//!   1. It never panics.
//!   2. It never allocates more than `MAX_METADATA_ENTRIES` elements per
//!      collection (verified indirectly — if it allocated `u64::MAX`-class
//!      memory the process would be OOM-killed by the fuzz harness).
//!   3. On success, the returned value can be re-encoded and re-decoded
//!      consistently.
//!
//! Run with: cargo +nightly fuzz run fuzz_tx_metadata_decode -- -max_total_time=300

#![no_main]

use dugite_serialization::cbor::encode_metadatum;
use dugite_serialization::decode_bounded::decode_metadatum_from_bytes;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // 1. Decoder must never panic on arbitrary input.
    let result = decode_metadatum_from_bytes(data);

    // 2. On successful decode, encode-then-decode round-trip must agree on
    //    the structure (the canonical re-encoding may differ from the
    //    original wire form, but a second decode must yield the same value).
    if let Ok(meta) = result {
        let re_encoded = encode_metadatum(&meta);
        match decode_metadatum_from_bytes(&re_encoded) {
            Ok(meta2) => {
                assert_eq!(
                    meta, meta2,
                    "round-trip metadatum decode disagrees: re_encoded={re_encoded:?}"
                );
            }
            Err(e) => {
                // Re-encoded output must always be re-decodable.
                panic!("re-encoded metadatum failed to decode: {e}");
            }
        }
    }
});
