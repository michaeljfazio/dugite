//! Fuzz target: ChainSync server-side state machine.
//!
//! The ChainSync server (responder) accepts `MsgRequestNext`,
//! `MsgAwaitReply`, `MsgFindIntersect`, and `MsgDone` from a remote
//! initiator.  This target exercises the server's codec path to ensure
//! arbitrary input never causes a panic.
//!
//! Coverage goals:
//! - B3/B19: MsgFindIntersect with unlimited points list → bounded decoding
//! - B1: Wrong message in any state → error, not panic
//! - Indefinite-length arrays in MsgFindIntersect → cap respected
//!
//! The B3 fix adds `MAX_INTERSECT_POINTS = 100` to `decode_message`
//! in chainsync/mod.rs, which is what this target exercises.
//!
//! Run with:
//!   cargo +nightly fuzz run fuzz_chainsync_server_state_machine \
//!     -- -max_total_time=120

#![no_main]

use libfuzzer_sys::fuzz_target;

use dugite_network::protocol::chainsync::decode_message;

fuzz_target!(|data: &[u8]| {
    // The server decode path is the same codec function used by both sides.
    // Any panic here is a regression.
    let _ = decode_message(data);
});
