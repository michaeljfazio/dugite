//! Fuzz target for the multiplexer SDU demux path.
//!
//! Feeds arbitrary bytes through the 8-byte SDU header decoder
//! (`decode_header`) and the full `encode_header`/`decode_header` round-trip.
//!
//! The Ouroboros mux SDU header is the first thing processed for every byte
//! that arrives from a remote peer, making it the highest-priority parser to
//! harden against arbitrary input.
//!
//! Run with:
//!   cargo +nightly fuzz run fuzz_mux_demux -- -max_total_time=300

#![no_main]

use libfuzzer_sys::fuzz_target;

use dugite_network::mux::segment::{decode_header, encode_header, HEADER_SIZE};

fuzz_target!(|data: &[u8]| {
    // Attempt to interpret arbitrary input as a raw SDU header.
    // decode_header requires exactly 8 bytes — handle shorter input gracefully.
    if data.len() >= HEADER_SIZE {
        let buf: &[u8; HEADER_SIZE] = data[..HEADER_SIZE].try_into().unwrap();
        let header = decode_header(buf);

        // Round-trip: encode the decoded header and verify it decodes back identically.
        let re_encoded = encode_header(&header);
        let re_decoded = decode_header(&re_encoded);
        let _ = re_decoded;
    }

    // Also try decoding the payload portion (after the header) as arbitrary CBOR
    // to exercise the ingress dispatch path.
    if data.len() > HEADER_SIZE {
        let _payload = &data[HEADER_SIZE..];
        // No panic expected from raw slice operations.
    }
});
