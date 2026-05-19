//! Fuzz target: TxSubmission2 server state machine codec.
//!
//! The TxSubmission2 server drives the protocol by sending `MsgRequestTxIds`
//! and `MsgRequestTxs`.  The *remote peer* (client role) sends:
//!   [6]                                — MsgInit
//!   [1, [[tx_id, size]]]              — MsgReplyTxIds (indefinite or definite)
//!   [3, [[era_id, tag(24)(tx_cbor)]]] — MsgReplyTxs
//!   [4]                               — MsgDone
//!
//! Coverage goals:
//! - B2: Indefinite-length MsgReplyTxIds / MsgRequestTxs / MsgReplyTxs arrays
//!       are capped at MAX_INFLIGHT (u16::MAX) per the B2 fix.
//! - B6: MsgReplyTxs entries MUST carry tag(24) — wrong tag is an error.
//! - B8: Non-MsgReplyTxs response to MsgRequestTxs → StateViolation.
//!
//! This target exercises `decode_message` — the codec layer shared by
//! both client and server paths.
//!
//! Run with:
//!   cargo +nightly fuzz run fuzz_txsubmission2_server_state_machine \
//!     -- -max_total_time=120

#![no_main]

use libfuzzer_sys::fuzz_target;

use dugite_network::protocol::txsubmission::decode_message;

fuzz_target!(|data: &[u8]| {
    // The codec must never panic on arbitrary remote input.
    let _ = decode_message(data);
});
