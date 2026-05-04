//! Fuzz target for the KeepAlive wire-protocol message decoder.
//!
//! KeepAlive messages arrive on protocol ID 8 (N2N) from every connected peer.
//! The decoder must never panic on arbitrary input.
//!
//! Wire format:
//!   `MsgKeepAlive`         = `[0, cookie]`  (cookie is u16)
//!   `MsgKeepAliveResponse` = `[1, cookie]`
//!   `MsgDone`              = `[2]`
//!
//! Run with:
//!   cargo +nightly fuzz run fuzz_keepalive_msg -- -max_total_time=300

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Should never panic regardless of input.
    let _ = dugite_network::protocol::keepalive::decode_message(data);
});
