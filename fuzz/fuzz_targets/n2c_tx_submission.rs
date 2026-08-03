//! Fuzz target for the N2C LocalTxSubmission MsgSubmitTx decoder.
//!
//! LocalTxSubmission has no standalone `decode_message()` function — parsing
//! is embedded in `LocalTxSubmissionServer::run()` (async, requires a live
//! MuxChannel and TxValidator). Instead this target directly exercises the
//! same CBOR decode sequence that the server applies to every inbound message:
//!
//!   `[tag, payload]`
//!
//! Wire format: `MsgSubmitTx = [0, [era_id, tx_bytes]]`
//!
//! This covers the structural decode path (array, u64 tag, nested array,
//! u16 era_id, bytes) that processes untrusted network input before any
//! validator is invoked.
//!
//! Run with:
//!   cargo +nightly fuzz run fuzz_n2c_tx_submission -- -max_total_time=300

#![no_main]

use libfuzzer_sys::fuzz_target;
use minicbor::Decoder;

fuzz_target!(|data: &[u8]| {
    // Replicate the exact CBOR decode sequence from LocalTxSubmissionServer::run().
    let mut dec = Decoder::new(data);

    // Outer: array + message tag
    let Ok(_) = dec.array() else { return };
    let Ok(tag) = dec.u64() else { return };

    match tag {
        // TAG_SUBMIT_TX = 0: [0, [era_id, tx_bytes]]
        0 => {
            let _ = dec.array();
            let _ = dec.u16(); // era_id
            let _ = dec.bytes(); // tx_bytes
        }
        // TAG_DONE = 3: [3]  — no payload
        3 => {}
        // Any other tag: server would return a protocol error.
        _ => {}
    }
});
