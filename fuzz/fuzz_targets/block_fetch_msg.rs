//! Fuzz target for the BlockFetch wire-protocol message decoder.
//!
//! BlockFetch messages are the primary data-plane path for block downloads.
//! Any peer can send arbitrary CBOR on protocol ID 3 (N2N BlockFetch), so
//! `decode_message` must never panic.
//!
//! Wire format:
//!   `MsgRequestRange` = `[0, from_point, to_point]`
//!   `MsgClientDone`   = `[1]`
//!   `MsgStartBatch`   = `[2]`
//!   `MsgNoBlocks`     = `[3]`
//!   `MsgBlock`        = `[4, tag(24) bstr(cbor)]`
//!   `MsgBatchDone`    = `[5]`
//!
//! Run with:
//!   cargo +nightly fuzz run fuzz_block_fetch_msg -- -max_total_time=300

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Should never panic regardless of input.
    let _ = dugite_network::protocol::blockfetch::decode_message(data);
});
