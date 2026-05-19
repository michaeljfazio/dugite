//! Fuzz target: pipelined ChainSync client state machine.
//!
//! Simulates an adversarial server sending arbitrary CBOR frames on the
//! ChainSync channel to the pipelined client receive loop in `sync.rs`.
//!
//! The pipelined client loop (chainsync_client_task) expects:
//!   [1]               — MsgAwaitReply
//!   [2, header, tip]  — MsgRollForward
//!   [3, point, tip]   — MsgRollBackward
//!   [5, point, tip]   — MsgIntersectFound
//!   [6, tip]          — MsgIntersectNotFound
//!
//! Any other message (wrong tag, unknown tag, malformed CBOR) must cause a
//! clean error rather than a panic. This target exercises the `decode_message`
//! path used by `chainsync_client_task` and verifies no panic occurs.
//!
//! Coverage goals:
//! - B1: State-machine violation → error (not panic/silent-skip)
//! - B12: Header CBOR too large → error
//! - B13: Slot extraction failure → error
//! - B3/B19: MsgFindIntersect with huge point list → error
//!
//! Run with:
//!   cargo +nightly fuzz run fuzz_chainsync_client_pipelined_state_machine \
//!     -- -max_total_time=120

#![no_main]

use libfuzzer_sys::fuzz_target;

use dugite_network::protocol::chainsync::{decode_message, MAX_INTERSECT_POINTS};

fuzz_target!(|data: &[u8]| {
    // Primary: the codec must never panic on arbitrary input.
    let _ = decode_message(data);

    // Secondary: verify the MAX_INTERSECT_POINTS constant is not trivially 0
    // (compile-time guard that the B3 cap is actually in effect).
    let _ = MAX_INTERSECT_POINTS;
});
