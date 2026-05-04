//! Fuzz target for the N2N TxSubmission2 wire-protocol message decoder.
//!
//! TxSubmission2 is the N2N tx-propagation protocol (protocol ID 4). Any
//! remote peer can send arbitrary CBOR on this protocol channel, so
//! `decode_message` must never panic.
//!
//! Wire format:
//!   `MsgInit`          = `[6]`
//!   `MsgRequestTxIds`  = `[0, blocking, ack_count, req_count]`
//!   `MsgReplyTxIds`    = `[1, [[tx_id, size]]]`  (indefinite or definite inner array)
//!   `MsgRequestTxs`    = `[2, [tx_id]]`
//!   `MsgReplyTxs`      = `[3, [[era_id, tag(24)(tx_cbor)]]]`
//!   `MsgDone`          = `[4]`
//!
//! The TxIds and TxBatch flow paths accept both definite and indefinite-length
//! CBOR arrays, so the fuzzer will naturally explore both encodings.
//!
//! Run with:
//!   cargo +nightly fuzz run fuzz_tx_submission2_msg -- -max_total_time=300

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Should never panic regardless of input.
    let _ = dugite_network::protocol::txsubmission::decode_message(data);
});
