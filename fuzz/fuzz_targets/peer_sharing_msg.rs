//! Fuzz target for the PeerSharing wire-protocol message decoder.
//!
//! PeerSharing messages are exchanged on protocol ID 10 (N2N) and contain
//! peer IP addresses. Any peer can send arbitrary CBOR here — the decoder
//! must never panic.
//!
//! Wire format:
//!   `MsgShareRequest` = `[0, amount]`
//!   `MsgSharePeers`   = `[1, [*addr]]`  where addr is `[0, u32, u16]` (IPv4)
//!                                          or `[1, u32, u32, u32, u32, u16]` (IPv6)
//!   `MsgDone`         = `[2]`
//!
//! Run with:
//!   cargo +nightly fuzz run fuzz_peer_sharing_msg -- -max_total_time=300

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Should never panic regardless of input.
    let _ = dugite_network::protocol::peersharing::decode_message(data);
});
