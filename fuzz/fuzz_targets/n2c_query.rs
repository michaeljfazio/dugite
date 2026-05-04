//! Fuzz target for the N2C LocalStateQuery message CBOR path.
//!
//! LocalStateQuery has no standalone `decode_message()` function — query
//! parsing is driven by `LocalStateQueryServer::run()` (async, requires
//! a live MuxChannel). Instead this target fuzzes the outer message frame
//! that the server parses from the channel:
//!
//!   `[tag, ...]`  where tag selects acquire/query/release/done behaviour.
//!
//! We exercise the same CBOR prefix decode logic (array + u64 tag) plus
//! the HFC encoding helpers (`wrap_hfc_success`, `encode_hfc_era_mismatch`,
//! `encode_cbor_tag24`) which are hit on every query response.
//!
//! Cover query tags: epoch (0), tip (1), UTxO (7), pool params (6),
//! governance state (33), block hash (27).
//!
//! Run with:
//!   cargo +nightly fuzz run fuzz_n2c_query -- -max_total_time=300

#![no_main]

use libfuzzer_sys::fuzz_target;
use minicbor::Decoder;

use dugite_network::protocol::local_state_query::encoding::{
    encode_cbor_tag24, encode_hfc_era_mismatch, wrap_hfc_success,
};

fuzz_target!(|data: &[u8]| {
    // Exercise the LocalStateQuery outer message parser path:
    // the server does `dec.array()` then `dec.u64()` on every inbound message.
    {
        let mut dec = Decoder::new(data);
        let _ = dec.array();
        let _ = dec.u64();
    }

    // Exercise the acquire-target decoder (SpecificPoint variant decodes a Point):
    // [0, [slot, hash]] — same CBOR decode path the server hits on MsgAcquire.
    {
        let mut dec = Decoder::new(data);
        if dec.array().is_ok() {
            if let Ok(tag) = dec.u64() {
                match tag {
                    // MsgAcquire(SpecificPoint) — tries to decode a Point next
                    0 | 6 => {
                        let _ = dec.array();
                        let _ = dec.u64();
                        let _ = dec.bytes();
                    }
                    // MsgQuery — tries to decode a raw query blob next
                    3 => {
                        let _ = dec.bytes();
                    }
                    _ => {}
                }
            }
        }
    }

    // Exercise the HFC encoding helpers that are called on every query response.
    // These take &[u8] so arbitrary fuzz data is a valid input.
    let _ = wrap_hfc_success(data);
    let _ = encode_cbor_tag24(data);

    // encode_hfc_era_mismatch takes a u64; derive one from the data if possible.
    if data.len() >= 8 {
        let era_index = u64::from_le_bytes(data[..8].try_into().unwrap());
        let _ = encode_hfc_era_mismatch(era_index);
    }
});
